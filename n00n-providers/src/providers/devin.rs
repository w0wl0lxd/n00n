//! Native Devin provider using Connect protocol over gRPC-Web.
//!
//! Protocol:
//! 1. Read `~/.local/share/devin/credentials.toml` or env for session token
//! 2. Call `POST /exa.auth_pb.AuthService/GetUserJwt` (application/proto) to get user JWT
//! 3. Call `POST /exa.api_server_pb.ApiServerService/GetChatMessage` (application/connect+proto)
//!    with gzip-framed request, stream of gzip-framed responses
//! 4. Parse Connect frames, gunzip, decode protobuf, emit ProviderEvents

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use flate2::Compression;
use flate2::write::GzEncoder;
use flume::Sender;
use isahc::AsyncReadResponseExt;
use isahc::RequestExt;
use serde::Deserialize;
use tracing::debug;

use crate::model::{ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::types::{Role, System};
use crate::{
    AgentError, Message, ProviderEvent, RequestOptions, StopReason, StreamResponse, TokenUsage,
};

use super::ResolvedAuth;
use super::devin_connect::{
    CONNECT_COMPRESSED_FLAG, FrameBuffer, decode_frame_payload, encode_frame,
};
use super::devin_proto::*;

const DEVIN_API_URL: &str = "https://server.codeium.com";
const DEVIN_AUTH_PATH: &str = "/exa.auth_pb.AuthService/GetUserJwt";
const DEVIN_CHAT_PATH: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
const DEVIN_SESSION_TOKEN_PREFIX: &str = "devin-session-token$";

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "devin",
    display_name: "Devin",
    protocol: n00n_config::providers::Protocol::Devin,
    default_base_url: DEVIN_API_URL,
    default_api_key_env: "DEVIN_API_KEY",
    default_model: "devin/swe-1-7-max",
    plans: None,
    login_url: None,
    needs_url: false,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["swe-1-7"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["swe-1-7-max"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: true,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["swe-1-7-lightning"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["claude-sonnet-4-6"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gpt-5-4-none"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gemini-3-1-pro-low"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 1_000_000,
        },
    ]
}

#[derive(Debug, Clone)]
struct DevinCredentials {
    session_token: String,
    api_server_url: String,
}

impl DevinCredentials {
    fn from_env() -> Option<Self> {
        let session_token = std::env::var("WINDSURF_API_KEY")
            .or_else(|_| std::env::var("DEVIN_API_KEY"))
            .ok()?;
        Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url: DEVIN_API_URL.to_string(),
        })
    }

    fn from_file() -> Option<Self> {
        let creds_path =
            PathBuf::from(std::env::var("HOME").ok()?).join(".local/share/devin/credentials.toml");

        let content = std::fs::read_to_string(&creds_path).ok()?;

        #[derive(Deserialize)]
        struct TomlCredentials {
            windsurf_api_key: Option<String>,
            api_server_url: Option<String>,
        }

        let creds: TomlCredentials = toml::from_str(&content).ok()?;
        let session_token = creds.windsurf_api_key?;
        let api_server_url = creds
            .api_server_url
            .unwrap_or_else(|| DEVIN_API_URL.to_string());

        Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url,
        })
    }
}

fn normalize_session_token(token: &str) -> String {
    if token.starts_with(DEVIN_SESSION_TOKEN_PREFIX) {
        token.to_string()
    } else {
        format!("{}{}", DEVIN_SESSION_TOKEN_PREFIX, token)
    }
}

pub struct Devin {
    credentials: Option<DevinCredentials>,
    base_url: String,
}

impl Devin {
    pub fn new(timeouts: super::Timeouts) -> Self {
        let _ = timeouts; // Not used in native implementation

        let credentials = DevinCredentials::from_env().or_else(|| DevinCredentials::from_file());

        let base_url = credentials
            .as_ref()
            .map(|c| c.api_server_url.clone())
            .unwrap_or_else(|| DEVIN_API_URL.to_string());

        Self {
            credentials,
            base_url,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn with_auth(
        auth: &Arc<Mutex<ResolvedAuth>>,
        _timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        let resolved = match auth.lock() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };

        let base_url = resolved
            .base_url
            .clone()
            .unwrap_or_else(|| DEVIN_API_URL.to_string());

        let session_token = resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_session_token);

        let credentials = session_token.map(|token| DevinCredentials {
            session_token: token,
            api_server_url: base_url.clone(),
        });

        Ok(Self {
            credentials,
            base_url,
        })
    }

    async fn get_user_jwt(&self) -> Result<(String, String), AgentError> {
        let creds = self
            .credentials
            .as_ref()
            .ok_or_else(|| AgentError::Config {
                message: "no Devin credentials found".to_string(),
            })?;

        let request_bytes = encode_get_user_jwt_request(&creds.session_token);

        let url = format!("{}{}", creds.api_server_url, DEVIN_AUTH_PATH);

        let response = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build auth request: {e}"),
            })?
            .send_async()
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: format!("auth request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read error body".to_string());
            return Err(AgentError::Api {
                status,
                message: format!("auth failed: {body}"),
            });
        }

        let response_bytes = response.bytes().await.map_err(|e| AgentError::Api {
            status: 0,
            message: format!("failed to read auth response: {e}"),
        })?;

        let auth_response =
            decode_get_user_jwt_response(&response_bytes).map_err(|e| AgentError::Api {
                status: 0,
                message: format!("failed to decode auth response: {e}"),
            })?;

        if auth_response.user_jwt.is_empty() {
            return Err(AgentError::Api {
                status: 0,
                message: "auth response missing user_jwt".to_string(),
            });
        }

        let base_url = if auth_response.custom_api_server_url.is_empty() {
            creds.api_server_url.clone()
        } else {
            auth_response
                .custom_api_server_url
                .trim_end_matches('/')
                .to_string()
        };

        Ok((auth_response.user_jwt, base_url))
    }

    async fn stream_chat_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        _tools: &'a serde_json::Value,
        event_tx: &'a Sender<ProviderEvent>,
        _opts: RequestOptions,
    ) -> Result<StreamResponse, AgentError> {
        let (user_jwt, base_url) = self.get_user_jwt().await?;
        let creds = self
            .credentials
            .as_ref()
            .ok_or_else(|| AgentError::Config {
                message: "no Devin credentials found".to_string(),
            })?;

        let model_uid = model.id.split('/').last().unwrap_or(&model.id);

        let prompt = system
            .blocks()
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        let cascade_id = uuid::Uuid::now_v7().to_string();

        let request_bytes = encode_get_chat_message_request(
            &creds.session_token,
            &user_jwt,
            &prompt,
            model_uid,
            &cascade_id,
        );

        let gzipped = {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(&request_bytes)
                .map_err(|e| AgentError::Config {
                    message: format!("gzip compression failed: {e}"),
                })?;
            encoder.finish().map_err(|e| AgentError::Config {
                message: format!("gzip finish failed: {e}"),
            })?
        };

        let frame =
            encode_frame(CONNECT_COMPRESSED_FLAG, &gzipped).map_err(|e| AgentError::Config {
                message: format!("failed to encode connect frame: {e}"),
            })?;

        let url = format!("{}{}", base_url, DEVIN_CHAT_PATH);

        let response = isahc::Request::post(&url)
            .header("content-type", "application/connect+proto")
            .header("connect-protocol-version", "1")
            .header("connect-content-encoding", "gzip")
            .header("accept-encoding", "identity")
            .header("user-agent", "connect-go/1.18.1 (go1.26.3)")
            .header("connect-accept-encoding", "gzip")
            .body(frame)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build chat request: {e}"),
            })?
            .send_async()
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: format!("chat request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read error body".to_string());
            return Err(AgentError::Api {
                status,
                message: format!("chat failed: {body}"),
            });
        }

        let mut reader = response.body().map_err(|e| AgentError::Api {
            status: 0,
            message: format!("failed to get response body: {e}"),
        })?;

        let mut frame_buffer = FrameBuffer::default();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut usage = TokenUsage::default();
        let mut stop_reason = StopReason::EndTurn;
        let mut tool_calls: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();

        let mut buffer = vec![0u8; 8192];

        loop {
            let n = reader.read(&mut buffer).map_err(|e| AgentError::Api {
                status: 0,
                message: format!("failed to read response: {e}"),
            })?;

            if n == 0 {
                break;
            }

            frame_buffer.push(&buffer[..n]);

            while let Some(frame_result) = frame_buffer.next_frame() {
                let frame = frame_result.map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("invalid connect frame: {e}"),
                })?;

                if frame.end_stream {
                    // Parse trailer JSON for errors
                    let payload = decode_frame_payload(&frame).map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to decode trailer: {e}"),
                    })?;
                    let trailer = String::from_utf8_lossy(&payload);
                    if !trailer.trim().is_empty() {
                        debug!("devin trailer: {}", trailer);
                    }
                    continue;
                }

                let payload = decode_frame_payload(&frame).map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("failed to decode frame payload: {e}"),
                })?;

                let response =
                    decode_get_chat_message_response(&payload).map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to decode chat response: {e}"),
                    })?;

                if !response.delta_text.is_empty() {
                    let delta = response.delta_text;
                    text.push_str(&delta);
                    let _ = event_tx
                        .send_async(ProviderEvent::TextDelta { text: delta })
                        .await;
                }

                if !response.delta_thinking.is_empty() {
                    let delta = response.delta_thinking;
                    thinking.push_str(&delta);
                    let _ = event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text: delta })
                        .await;
                }

                for tc in response.delta_tool_calls {
                    if !tool_calls.contains_key(&tc.id) {
                        let _ = event_tx
                            .send_async(ProviderEvent::ToolUseStart {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                            })
                            .await;
                        tool_calls.insert(tc.id.clone(), (tc.name.clone(), tc.arguments_json));
                    }
                }

                if response.stop_reason != 0 {
                    stop_reason = StopReason::MaxTokens;
                }

                if let Some(u) = response.usage {
                    usage.input = u.input_tokens as u32;
                    usage.output = u.output_tokens as u32;
                    usage.cache_read = u.cache_read_tokens as u32;
                    usage.cache_creation = u.cache_write_tokens as u32;
                }
            }
        }

        let mut content_blocks = Vec::new();
        if !thinking.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Thinking {
                thinking,
                signature: None,
            });
        }
        if !text.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text { text });
        }
        if content_blocks.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text {
                text: String::new(),
            });
        }

        let message = Message {
            role: Role::Assistant,
            content: content_blocks,
            display_text: None,
            control: false,
        };

        Ok(StreamResponse {
            message,
            usage,
            stop_reason: Some(stop_reason),
        })
    }
}

impl Provider for Devin {
    fn stream_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a serde_json::Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a n00n_storage::id::SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            self.stream_chat_message(model, messages, system, tools, event_tx, opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async move {
            let models = models()
                .iter()
                .map(|e| crate::model::ModelInfo {
                    id: e.prefixes[0].to_string(),
                    name: None,
                    context_window: Some(e.context_window),
                    max_output_tokens: Some(e.max_output_tokens),
                    pricing: Some(e.pricing),
                    supports_thinking: None,
                    supports_vision: Some(e.vision),
                    tier: Some(e.tier),
                    is_free: None,
                    is_promo: None,
                    provider_info: None,
                })
                .collect();
            Ok(models)
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_session_token_adds_prefix() {
        assert_eq!(
            normalize_session_token("abc123"),
            "devin-session-token$abc123"
        );
    }

    #[test]
    fn normalize_session_token_preserves_prefix() {
        assert_eq!(
            normalize_session_token("devin-session-token$abc123"),
            "devin-session-token$abc123"
        );
    }
}

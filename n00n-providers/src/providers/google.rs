use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_lock::RwLock;
use flume::Sender;
use futures_lite::io::{AsyncBufReadExt, BufReader};
use isahc::{AsyncReadResponseExt, HttpClient, Request};
use n00n_storage::id::{SessionRef, n00nId};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::model::{Model, ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::{
    AgentError, ContentBlock, Message, ProviderEvent, RequestOptions, Role, StopReason,
    StreamResponse, System, ThinkingConfig, TokenUsage,
};

use super::{KeyPool, ResolvedAuth, http_client, next_sse_line};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const ENV_VAR: &str = "GEMINI_API_KEY";
const FLASH_MAX_THINKING: u32 = 24_576;
const PRO_MAX_THINKING: u32 = 32_768;
const CACHE_PREFIX_LEN: usize = 3;
const CACHE_TTL_SECONDS: u64 = 300;
const CACHE_TTL: Duration = Duration::from_secs(CACHE_TTL_SECONDS);
const CACHE_FAILURE_RETRY_DELAY: Duration = Duration::from_mins(1);
const PRO_CACHE_WRITE_PRICE: f64 = 1.625;
const FLASH_CACHE_WRITE_PRICE: f64 = 0.233_333_333_333_333_34;

/// The generic per-model max, capped by Google's documented `thinkingBudget`
/// hard limits per family.
fn supports_explicit_cache(model_id: &str) -> bool {
    !model_id.starts_with("gemini-2.0-flash-lite")
}

fn max_thinking(model: &Model) -> u32 {
    let cap = if model.id.contains("flash") {
        FLASH_MAX_THINKING
    } else {
        PRO_MAX_THINKING
    };
    model.max_thinking_budget().map_or(cap, |m| m.min(cap))
}

fn cached_content_metadata(cached: &Value) -> Result<(String, u32), AgentError> {
    let name = cached["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AgentError::Config {
            message: "Google cached content response omitted its name".to_string(),
        })?;
    let total_token_count = cached["usageMetadata"]["totalTokenCount"]
        .as_u64()
        .ok_or_else(|| AgentError::Config {
            message: "Google cached content response omitted its token count".to_string(),
        })
        .and_then(|count| {
            u32::try_from(count).map_err(|_| AgentError::Config {
                message: "Google cached content token count exceeded supported range".to_string(),
            })
        })?;
    Ok((name.to_string(), total_token_count))
}

fn cached_input_hash(
    model_id: &str,
    system: &System,
    tools: &Value,
    messages: &[Message],
    prefix_len: usize,
) -> Result<u64, AgentError> {
    let mut hasher = DefaultHasher::new();
    model_id.hash(&mut hasher);
    system.to_string().hash(&mut hasher);
    serde_json::to_string(tools)?.hash(&mut hasher);
    serde_json::to_string(&convert_messages(&messages[..prefix_len]))?.hash(&mut hasher);
    Ok(hasher.finish())
}

#[derive(Clone, Debug)]
struct CachedContentState {
    name: Option<String>,
    input_hash: u64,
    cached_count: usize,
    retry_at: Instant,
    attempt_id: Option<u64>,
}

impl CachedContentState {
    fn ready(name: String, input_hash: u64, cached_count: usize) -> Self {
        Self {
            name: Some(name),
            input_hash,
            cached_count,
            retry_at: Instant::now() + CACHE_TTL,
            attempt_id: None,
        }
    }

    fn failed(input_hash: u64) -> Self {
        Self {
            name: None,
            input_hash,
            cached_count: 0,
            retry_at: Instant::now() + CACHE_FAILURE_RETRY_DELAY,
            attempt_id: None,
        }
    }

    fn creating(input_hash: u64, attempt_id: u64) -> Self {
        Self {
            name: None,
            input_hash,
            cached_count: 0,
            retry_at: Instant::now() + CACHE_FAILURE_RETRY_DELAY,
            attempt_id: Some(attempt_id),
        }
    }

    fn owns_attempt(&self, input_hash: u64, attempt_id: u64) -> bool {
        self.input_hash == input_hash && self.attempt_id == Some(attempt_id)
    }

    fn cached_content(
        &self,
        input_hash: u64,
        current_message_count: usize,
        now: Instant,
    ) -> Option<(&str, usize)> {
        (self.input_hash == input_hash
            && current_message_count >= self.cached_count
            && now < self.retry_at)
            .then(|| self.name.as_deref())
            .flatten()
            .map(|name| (name, self.cached_count))
    }

    fn suppress_retry(&self, input_hash: u64, now: Instant) -> bool {
        self.name.is_none() && self.input_hash == input_hash && now < self.retry_at
    }
}

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "google",
    display_name: "Google",
    protocol: n00n_config::providers::Protocol::Google,
    default_base_url: BASE_URL,
    default_api_key_env: ENV_VAR,
    default_model: "google/gemini-2.5-pro",
    plans: None,
    login_url: Some("https://aistudio.google.com/apikey"),
    needs_url: false,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["gemini-2.5-pro"],
            tier: ModelTier::Strong,
            family: ModelFamily::Gemini,
            vision: true,
            default: true,
            pricing: ModelPricing {
                input: 1.25,
                output: 5.00,
                cache_write: PRO_CACHE_WRITE_PRICE,
                cache_read: 0.31,
                fast: None,
            },
            max_output_tokens: 65_536,
            context_window: 1_048_576,
        },
        ModelEntry {
            prefixes: &["gemini-2.5-flash"],
            tier: ModelTier::Medium,
            family: ModelFamily::Gemini,
            vision: true,
            default: true,
            pricing: ModelPricing {
                input: 0.15,
                output: 0.60,
                cache_write: FLASH_CACHE_WRITE_PRICE,
                cache_read: 0.04,
                fast: None,
            },
            max_output_tokens: 65_536,
            context_window: 1_048_576,
        },
        ModelEntry {
            prefixes: &["gemini-2.0-flash-lite"],
            tier: ModelTier::Weak,
            family: ModelFamily::Gemini,
            vision: true,
            default: true,
            pricing: ModelPricing {
                input: 0.075,
                output: 0.30,
                cache_write: 0.00,
                cache_read: 0.01,
                fast: None,
            },
            max_output_tokens: 65_536,
            context_window: 1_048_576,
        },
    ]
}

fn resolve_auth_from_key(key: &str) -> ResolvedAuth {
    ResolvedAuth {
        base_url: None,
        headers: vec![("x-goog-api-key".into(), key.to_string())],
    }
}

pub struct Google {
    client: HttpClient,
    auth: Arc<Mutex<ResolvedAuth>>,
    key_pool: Option<KeyPool>,
    stream_timeout: Duration,
    cache_state: Arc<Mutex<HashMap<n00nId, CachedContentState>>>,
    auth_scope_lock: Arc<RwLock<()>>,
}

impl Google {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        let pool = KeyPool::resolve("google", ENV_VAR)?;
        let resolved = resolve_auth_from_key(pool.current());
        Ok(Self {
            client: http_client(timeouts)?,
            auth: Arc::new(Mutex::new(resolved)),
            key_pool: Some(pool),
            stream_timeout: timeouts.stream,
            cache_state: Arc::new(Mutex::new(HashMap::new())),
            auth_scope_lock: Arc::new(RwLock::new(())),
        })
    }

    pub(crate) fn with_auth(
        auth: Arc<Mutex<super::ResolvedAuth>>,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            client: http_client(timeouts)?,
            auth,
            key_pool: None,
            stream_timeout: timeouts.stream,
            cache_state: Arc::new(Mutex::new(HashMap::new())),
            auth_scope_lock: Arc::new(RwLock::new(())),
        })
    }

    fn build_request(&self, method: &str, url: &str) -> isahc::http::request::Builder {
        let auth = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        auth.configure_request(
            Request::builder()
                .method(method)
                .uri(url)
                .header("user-agent", super::user_agent()),
        )
    }

    fn api_key(&self) -> String {
        let auth = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        auth.headers
            .iter()
            .find(|(k, _)| k == "x-goog-api-key")
            .map_or_else(String::default, |(_, v)| v.clone())
    }

    fn stream_url(&self, model_id: &str) -> String {
        let base = {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            auth.base_url
                .as_deref()
                .unwrap_or_else(|| BASE_URL)
                .to_string()
        };
        let encoded = super::urlenc(model_id);
        format!("{base}/models/{encoded}:streamGenerateContent?alt=sse")
    }

    fn models_url(&self) -> String {
        let base = {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            auth.base_url
                .as_deref()
                .unwrap_or_else(|| BASE_URL)
                .to_string()
        };
        let key = self.api_key();
        format!("{base}/models?key={key}&pageSize=1000")
    }

    fn cached_contents_url(&self) -> String {
        let base = {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            auth.base_url
                .as_deref()
                .unwrap_or_else(|| BASE_URL)
                .to_string()
        };
        let key = self.api_key();
        format!("{base}/cachedContents?key={key}")
    }

    async fn create_cached_content(
        &self,
        model_id: &str,
        system: &System,
        tools: &Value,
        messages: &[Message],
    ) -> Result<(String, usize, u32), AgentError> {
        let url = self.cached_contents_url();
        let prefix_len = messages.len().min(CACHE_PREFIX_LEN);
        let system_text = system.to_string();

        let mut body = json!({
            "model": format!("models/{}", model_id),
            "contents": convert_messages(&messages[..prefix_len]),
            "ttl": format!("{CACHE_TTL_SECONDS}s"),
        });

        if !system_text.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": system_text}]});
        }

        let tool_decls = convert_tools(tools);
        if !tool_defs_empty(&tool_decls) {
            body["tools"] = json!([{"functionDeclarations": tool_decls}]);
        }

        let json_body = serde_json::to_vec(&body)?;
        let request = self
            .build_request("POST", &url)
            .header("content-type", "application/json")
            .body(json_body)?;

        let mut response = self.client.send_async(request).await?;
        if response.status().as_u16() != 200 {
            return Err(AgentError::from_response(response).await);
        }

        let response_text = response.text().await?;
        let cached: Value = serde_json::from_str(&response_text)?;
        let (name, total_token_count) = cached_content_metadata(&cached)?;
        Ok((name, prefix_len, total_token_count))
    }

    async fn delete_cached_content(&self, name: &str) -> Result<(), AgentError> {
        let base = {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            auth.base_url
                .as_deref()
                .unwrap_or_else(|| BASE_URL)
                .to_string()
        };
        let key = self.api_key();
        let url = format!("{base}/{}?key={key}", name.trim_start_matches('/'));

        let request = self.build_request("DELETE", &url).body(())?;
        let response = self.client.send_async(request).await?;

        if response.status().as_u16() == 200 || response.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(AgentError::from_response(response).await)
        }
    }

    fn build_body(
        model: &Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        thinking: ThinkingConfig,
    ) -> Value {
        let system_text = system.to_string();
        let mut body = json!({
            "contents": convert_messages(messages),
        });

        if !system_text.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": system_text}]});
        }

        thinking.apply_google_thinking(&mut body, max_thinking(model));

        if let Some(max_output) = model.max_output_tokens {
            body["generationConfig"]["maxOutputTokens"] = json!(max_output);
        }

        let tool_decls = convert_tools(tools);
        if !tool_defs_empty(&tool_decls) {
            body["tools"] = json!([{"functionDeclarations": tool_decls}]);
        }

        body
    }

    async fn do_stream(
        &self,
        model: &Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        event_tx: &Sender<ProviderEvent>,
        thinking: ThinkingConfig,
    ) -> Result<StreamResponse, AgentError> {
        let body = Self::build_body(model, messages, system, tools, thinking);
        let url = self.stream_url(&model.id);
        let json_body = serde_json::to_vec(&body)?;

        let request = self
            .build_request("POST", &url)
            .header("content-type", "application/json")
            .body(json_body)?;

        let response = self.client.send_async(request).await?;
        let status = response.status().as_u16();

        if status == 200 {
            parse_sse(response, event_tx, self.stream_timeout).await
        } else {
            Err(AgentError::from_response(response).await)
        }
    }
}

impl Provider for Google {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let current_message_count = messages.len();

            // Caching requires a stable session key and a prefix worth caching.
            let Some(sid) = session_id else {
                return self
                    .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await;
            };
            if current_message_count <= CACHE_PREFIX_LEN || !supports_explicit_cache(&model.id) {
                return self
                    .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await;
            }

            let _auth_scope_guard = self.auth_scope_lock.read().await;
            let prefix_len = current_message_count.min(CACHE_PREFIX_LEN);
            let current_input_hash =
                cached_input_hash(&model.id, system, tools, messages, prefix_len)?;
            let now = Instant::now();

            let session_key = sid.id();
            let cache_attempt_id = fastrand::u64(..);
            let (cached, suppress_retry, old_name_to_delete, owns_cache_attempt) = {
                let mut cache_state = self
                    .cache_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(state) = cache_state.get(&session_key) {
                    if let Some((name, cached_count)) =
                        state.cached_content(current_input_hash, current_message_count, now)
                    {
                        (Some((name.to_string(), cached_count)), false, None, false)
                    } else if state.suppress_retry(current_input_hash, now) {
                        (None, true, None, false)
                    } else {
                        let old_name = state.name.clone();
                        cache_state.insert(
                            session_key,
                            CachedContentState::creating(current_input_hash, cache_attempt_id),
                        );
                        (None, false, old_name, true)
                    }
                } else {
                    cache_state.insert(
                        session_key,
                        CachedContentState::creating(current_input_hash, cache_attempt_id),
                    );
                    (None, false, None, true)
                }
            };

            if suppress_retry {
                return self
                    .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await;
            }

            if let Some(name) = old_name_to_delete
                && let Err(error) = self.delete_cached_content(&name).await
            {
                warn!(error = %error, "failed to delete stale Google cached content");
            }

            let (cached_content_name, cached_count, cache_creation_tokens) = if let Some(pair) =
                cached
            {
                (pair.0, pair.1, 0)
            } else if owns_cache_attempt {
                match self
                    .create_cached_content(&model.id, system, tools, messages)
                    .await
                {
                    Ok((name, prefix_len, cache_creation_tokens)) => {
                        let accepted = {
                            let mut cache_state = self
                                .cache_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if cache_state.get(&session_key).is_some_and(|state| {
                                state.owns_attempt(current_input_hash, cache_attempt_id)
                            }) {
                                cache_state.insert(
                                    session_key,
                                    CachedContentState::ready(
                                        name.clone(),
                                        current_input_hash,
                                        prefix_len,
                                    ),
                                );
                                true
                            } else {
                                false
                            }
                        };
                        if accepted {
                            (name, prefix_len, cache_creation_tokens)
                        } else {
                            if let Err(error) = self.delete_cached_content(&name).await {
                                warn!(error = %error, "failed to delete superseded Google cached content");
                            }
                            return self
                                .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                                .await;
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "Google cached content unavailable; cooling down cache creation");
                        {
                            let mut cache_state = self
                                .cache_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if cache_state.get(&session_key).is_some_and(|state| {
                                state.owns_attempt(current_input_hash, cache_attempt_id)
                            }) {
                                cache_state.insert(
                                    session_key,
                                    CachedContentState::failed(current_input_hash),
                                );
                            }
                        }
                        return self
                            .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                            .await;
                    }
                }
            } else {
                return self
                    .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await;
            };

            if cached_content_name.is_empty() {
                return self
                    .do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await;
            }

            // The cached content already contains the system prompt and tool declarations;
            // the generation request only carries the new messages.
            let no_tools = json!([]);
            let mut body = Self::build_body(
                model,
                &messages[cached_count..],
                &System::default(),
                &no_tools,
                opts.thinking,
            );
            body["cachedContent"] = json!(cached_content_name);

            let url = self.stream_url(&model.id);
            let json_body = serde_json::to_vec(&body)?;

            let request = self
                .build_request("POST", &url)
                .header("content-type", "application/json")
                .body(json_body)?;

            let response = self.client.send_async(request).await?;
            let status = response.status().as_u16();

            if status == 200 {
                let mut stream_response =
                    parse_sse(response, event_tx, self.stream_timeout).await?;
                stream_response.usage.cache_creation = stream_response
                    .usage
                    .cache_creation
                    .saturating_add(cache_creation_tokens);
                Ok(stream_response)
            } else if status == 404 {
                {
                    let mut cache_state = self
                        .cache_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if cache_state
                        .get(&session_key)
                        .and_then(|state| state.name.as_deref())
                        == Some(cached_content_name.as_str())
                    {
                        cache_state.remove(&session_key);
                    }
                }
                self.do_stream(model, messages, system, tools, event_tx, opts.thinking)
                    .await
            } else {
                Err(AgentError::from_response(response).await)
            }
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        let url = self.models_url();
        let client = self.client.clone();
        Box::pin(async move {
            let request = self.build_request("GET", &url).body(())?;
            let mut response = client.send_async(request).await?;
            if response.status().as_u16() != 200 {
                return Err(AgentError::from_response(response).await);
            }
            let body_text = response.text().await?;
            let models_response: ModelsListResponse = serde_json::from_str(&body_text)?;
            let mut infos: Vec<crate::model::ModelInfo> = models_response
                .models
                .into_iter()
                .filter(|m| {
                    m.supported_generation_methods
                        .iter()
                        .any(|m| m == "generateContent")
                })
                .map(|m| {
                    let id = m
                        .name
                        .strip_prefix("models/")
                        .map_or_else(|| m.name.clone(), String::from);
                    crate::model::ModelInfo::id_only(id)
                })
                .collect();
            infos.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(infos)
        })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            let _auth_scope_guard = self.auth_scope_lock.write().await;
            let pool = KeyPool::resolve("google", ENV_VAR)?;
            *self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                resolve_auth_from_key(pool.current());
            self.cache_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            Ok(())
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async {
            let _auth_scope_guard = self.auth_scope_lock.write().await;
            let rotated = self
                .key_pool
                .as_ref()
                .is_some_and(|p| p.rotate_auth(&self.auth, resolve_auth_from_key));
            if rotated {
                self.cache_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
            }
            Ok(rotated)
        })
    }
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let tool_names: std::collections::HashMap<&str, &str> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();

    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "model",
        };

        let mut parts: Vec<Value> = Vec::new();
        // Gemini is strict about function-response turns: tool-returned
        // images get their own user turn instead of mixing inlineData into
        // the functionResponse content.
        let has_tool_results = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        let mut image_parts: Vec<Value> = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(json!({"text": text}));
                }
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let mut part = json!({"text": thinking, "thought": true});
                    if let Some(sig) = signature {
                        part["thoughtSignature"] = json!(sig);
                    }
                    parts.push(part);
                }
                ContentBlock::RedactedThinking { .. } => {}
                ContentBlock::ToolUse { id: _, name, input } => {
                    parts.push(json!({
                        "functionCall": {
                            "name": name,
                            "args": input,
                        }
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let mut response_val = serde_json::from_str(content)
                        .unwrap_or_else(|_| json!({"result": content}));
                    if *is_error {
                        response_val = json!({"error": response_val});
                    }
                    let name = tool_names
                        .get(tool_use_id.as_str())
                        .copied()
                        .unwrap_or_else(|| "unknown");
                    parts.push(json!({
                        "functionResponse": {
                            "name": name,
                            "response": response_val,
                        }
                    }));
                }
                ContentBlock::Image { source } => {
                    let part = json!({
                        "inlineData": {
                            "mimeType": source.media_type.mime(),
                            "data": source.data,
                        }
                    });
                    if has_tool_results {
                        image_parts.push(part);
                    } else {
                        parts.push(part);
                    }
                }
            }
        }

        if !parts.is_empty() {
            out.push(json!({"role": role, "parts": parts}));
        }
        if !image_parts.is_empty() {
            out.push(json!({"role": "user", "parts": image_parts}));
        }
    }

    out
}

fn convert_tools(tools: &Value) -> Vec<Value> {
    let Some(arr) = tools.as_array() else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            let description = t.get("description")?.as_str().unwrap_or_else(|| "");
            let parameters = t
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            Some(json!({
                "name": name,
                "description": description,
                "parameters": parameters,
            }))
        })
        .collect()
}

fn tool_defs_empty(tool_decls: &[Value]) -> bool {
    tool_decls.is_empty()
}

// --- SSE response types ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SseCandidate {
    content: Option<SseContent>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SseContent {
    parts: Option<Vec<SsePart>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SsePart {
    text: Option<String>,
    thought: Option<bool>,
    thought_signature: Option<String>,
    function_call: Option<SseFunctionCall>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SseFunctionCall {
    name: String,
    args: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_field_names)]
struct SseUsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    cached_content_token_count: Option<u32>,
    #[serde(default)]
    thoughts_token_count: u32,
    #[serde(default)]
    tool_use_prompt_token_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SseResponse {
    candidates: Option<Vec<SseCandidate>>,
    usage_metadata: Option<SseUsageMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsListResponse {
    models: Vec<ApiModelInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiModelInfo {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

async fn parse_sse(
    response: isahc::Response<isahc::AsyncBody>,
    event_tx: &Sender<ProviderEvent>,
    stream_timeout: Duration,
) -> Result<StreamResponse, AgentError> {
    let reader = BufReader::new(response.into_body());
    let mut lines = reader.lines();

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<StopReason> = None;
    let mut deadline = Instant::now() + stream_timeout;

    while let Some(line) = next_sse_line(&mut lines, &mut deadline, stream_timeout).await? {
        let data = match line.strip_prefix("data:") {
            Some(d) => d.strip_prefix(' ').unwrap_or_else(|| d),
            _ => continue,
        };

        let chunk: SseResponse = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to parse Gemini SSE chunk");
                continue;
            }
        };

        if let Some(meta) = chunk.usage_metadata {
            let cached = match meta.cached_content_token_count {
                Some(cached) => cached.min(meta.prompt_token_count),
                None => 0,
            };
            usage.input = meta
                .prompt_token_count
                .saturating_sub(cached)
                .saturating_add(meta.tool_use_prompt_token_count);
            usage.output = meta
                .candidates_token_count
                .saturating_add(meta.thoughts_token_count);
            usage.cache_read = cached;
        }

        let Some(candidates) = chunk.candidates else {
            continue;
        };

        for candidate in candidates {
            if let Some(reason) = candidate.finish_reason {
                stop_reason = Some(StopReason::from_google(&reason)).or(stop_reason);
            }

            let Some(content) = candidate.content else {
                continue;
            };
            let Some(parts) = content.parts else {
                continue;
            };

            for part in parts {
                if let Some(func_call) = part.function_call {
                    let id = format!("call_{}", func_call.name);
                    let input = func_call.args.unwrap_or_else(Default::default);
                    event_tx
                        .send_async(ProviderEvent::ToolUseStart {
                            id: id.clone(),
                            name: func_call.name.clone(),
                        })
                        .await?;
                    content_blocks.push(ContentBlock::ToolUse {
                        id,
                        name: func_call.name,
                        input,
                    });
                    stop_reason = Some(StopReason::ToolUse);
                } else if let Some(text) = part.text {
                    if part.thought.unwrap_or_else(|| false) {
                        if !text.is_empty() {
                            event_tx
                                .send_async(ProviderEvent::ThinkingDelta { text: text.clone() })
                                .await?;
                        }
                        content_blocks.push(ContentBlock::Thinking {
                            thinking: text,
                            signature: part.thought_signature,
                        });
                    } else if !text.is_empty() {
                        event_tx
                            .send_async(ProviderEvent::TextDelta { text: text.clone() })
                            .await?;
                        content_blocks.push(ContentBlock::Text { text });
                    }
                }
            }
        }
    }

    Ok(StreamResponse {
        message: Message {
            role: Role::Assistant,
            content: content_blocks,
            ..Default::default()
        },
        usage,
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn test_model() -> Model {
        Model {
            id: "gemini-2.5-flash".into(),
            provider: std::sync::Arc::<str>::from("google"),
            tier: ModelTier::Medium,
            family: ModelFamily::Gemini,
            supports_vision_override: Some(true),
            supports_tool_examples_override: None,
            supports_thinking_override: None,
            pricing: ModelPricing::default(),
            max_output_tokens: Some(8192),
            context_window: 1_048_576,
        }
    }

    #[test]
    fn google_build_body_basic() {
        let model = test_model();
        let messages = vec![Message::user("hello".into())];
        let body = Google::build_body(
            &model,
            &messages,
            &System::from("be helpful"),
            &json!([]),
            ThinkingConfig::Off,
        );

        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 8192);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn google_build_body_thinking_adaptive() {
        let messages = vec![Message::user("think".into())];
        let body = Google::build_body(
            &test_model(),
            &messages,
            &System::from(""),
            &json!([]),
            ThinkingConfig::Adaptive,
        );

        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn google_build_body_thinking_budget() {
        let messages = vec![Message::user("think hard".into())];
        let body = Google::build_body(
            &test_model(),
            &messages,
            &System::from(""),
            &json!([]),
            ThinkingConfig::Budget(8192),
        );

        // Clamped to the model's max thinking budget (half of 8192 output tokens).
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4096
        );
    }

    #[test_case("STOP", StopReason::EndTurn ; "stop")]
    #[test_case("MAX_TOKENS", StopReason::MaxTokens ; "max_tokens")]
    #[test_case("SAFETY", StopReason::EndTurn ; "safety")]
    #[test_case("RECITATION", StopReason::EndTurn ; "recitation")]
    #[test_case("unknown", StopReason::EndTurn ; "unknown")]
    fn stop_reason_from_google(input: &str, expected: StopReason) {
        assert_eq!(StopReason::from_google(input), expected);
    }

    #[test]
    fn convert_messages_user_and_assistant() {
        let messages = vec![
            Message::user("hello".into()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "hi there".into(),
                }],
                ..Default::default()
            },
        ];
        let result = convert_messages(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[0]["parts"][0]["text"], "hello");
        assert_eq!(result[1]["role"], "model");
        assert_eq!(result[1]["parts"][0]["text"], "hi there");
    }

    #[test]
    fn convert_messages_thinking_block() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: Some("sig123".into()),
            }],
            ..Default::default()
        }];
        let result = convert_messages(&messages);
        assert_eq!(result[0]["parts"][0]["text"], "hmm");
        assert_eq!(result[0]["parts"][0]["thought"], true);
        assert_eq!(result[0]["parts"][0]["thoughtSignature"], "sig123");
    }

    #[test]
    fn convert_messages_tool_use_and_result() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "/tmp/a"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "file contents".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let result = convert_messages(&messages);
        assert_eq!(result[0]["parts"][0]["functionCall"]["name"], "read_file");
        assert_eq!(
            result[1]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
    }

    #[test]
    fn convert_messages_tool_returned_image_gets_own_user_turn() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "[image: pic.png 1KB]".into(),
                    is_error: false,
                },
                ContentBlock::Image {
                    source: crate::ImageSource::new(
                        crate::ImageMediaType::Png,
                        std::sync::Arc::from("aGVsbG8="),
                    ),
                },
            ],
            ..Default::default()
        }];
        let result = convert_messages(&messages);
        assert_eq!(result.len(), 2);
        assert!(result[0]["parts"][0].get("functionResponse").is_some());
        assert_eq!(result[0]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(result[1]["role"], "user");
        assert_eq!(result[1]["parts"][0]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn convert_messages_chat_pasted_image_stays_in_same_turn() {
        // Split the turn and the question drifts apart from its picture.
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image {
                    source: crate::ImageSource::new(
                        crate::ImageMediaType::Webp,
                        std::sync::Arc::from("aGVsbG8="),
                    ),
                },
            ],
            ..Default::default()
        }];
        let result = convert_messages(&messages);
        assert_eq!(result.len(), 1);
        let parts = result[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/webp");
    }

    #[test]
    fn convert_tools_basic() {
        let tools = json!([{
            "name": "bash",
            "description": "run a command",
            "input_schema": {
                "type": "object",
                "properties": {
                    "cmd": {"type": "string"},
                    "opts": {
                        "type": "object",
                        "properties": {"verbose": {"type": "boolean"}}
                    }
                }
            }
        }]);
        let result = convert_tools(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "bash");
        assert_eq!(result[0]["description"], "run a command");
        assert!(result[0]["parameters"]["properties"]["cmd"].is_object());
    }

    #[test]
    fn models_list_has_defaults() {
        let models = models();
        assert!(!models.is_empty());
        for entry in models {
            assert!(!entry.prefixes.is_empty());
            assert!(entry.max_output_tokens > 0);
            assert!(entry.context_window >= entry.max_output_tokens);
        }
    }

    fn mock_response(data: &'static [u8]) -> isahc::Response<isahc::AsyncBody> {
        let body = isahc::AsyncBody::from_bytes_static(data);
        isahc::Response::builder().status(200).body(body).unwrap()
    }

    #[test]
    fn parse_sse_plain_text() {
        let data = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":10}}\n\n";
        let response = mock_response(data);
        let (tx, _rx) = flume::unbounded();
        let result = smol::block_on(parse_sse(response, &tx, Duration::from_secs(30))).unwrap();
        assert_eq!(result.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(result.usage.input, 5);
        assert_eq!(result.usage.output, 10);
        assert!(matches!(
            &result.message.content[0],
            ContentBlock::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn explicit_cache_skips_unsupported_flash_lite() {
        assert!(supports_explicit_cache("gemini-2.5-flash"));
        assert!(supports_explicit_cache("gemini-3-flash"));
        assert!(!supports_explicit_cache("gemini-2.0-flash-lite"));
        assert!(!supports_explicit_cache("gemini-2.0-flash-lite-001"));
    }

    #[test]
    fn cached_content_metadata_requires_name_and_token_count() {
        assert_eq!(
            cached_content_metadata(&json!({
                "name": "cachedContents/one",
                "usageMetadata": {"totalTokenCount": 2048}
            }))
            .unwrap(),
            ("cachedContents/one".to_string(), 2048)
        );

        let missing_name = cached_content_metadata(&json!({
            "usageMetadata": {"totalTokenCount": 2048}
        }))
        .unwrap_err();
        assert!(missing_name.to_string().contains("omitted its name"));

        let missing_count =
            cached_content_metadata(&json!({"name": "cachedContents/one"})).unwrap_err();
        assert!(
            missing_count
                .to_string()
                .contains("omitted its token count")
        );
    }

    #[test]
    fn parse_sse_thinking_part() {
        let data = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"thinking...\",\"thought\":true,\"thoughtSignature\":\"sig1\"}]}},{\"content\":{\"parts\":[{\"text\":\"answer\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":20}}\n\n";
        let response = mock_response(data);
        let (tx, _rx) = flume::unbounded();
        let result = smol::block_on(parse_sse(response, &tx, Duration::from_secs(30))).unwrap();
        assert!(matches!(
            &result.message.content[0],
            ContentBlock::Thinking { thinking, signature } if thinking == "thinking..." && signature.as_deref() == Some("sig1")
        ));
        assert!(matches!(
            &result.message.content[1],
            ContentBlock::Text { text } if text == "answer"
        ));
    }

    #[test]
    fn parse_sse_tool_call() {
        let data = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"bash\",\"args\":{\"cmd\":\"ls\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":15}}\n\n";
        let response = mock_response(data);
        let (tx, _rx) = flume::unbounded();
        let result = smol::block_on(parse_sse(response, &tx, Duration::from_secs(30))).unwrap();
        assert_eq!(result.stop_reason, Some(StopReason::ToolUse));
        assert!(matches!(
            &result.message.content[0],
            ContentBlock::ToolUse { name, .. } if name == "bash"
        ));
    }

    #[test]
    fn parse_sse_cached_thinking_and_tool_tokens() {
        let data = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":100,\"candidatesTokenCount\":10,\"cachedContentTokenCount\":50,\"thoughtsTokenCount\":7,\"toolUsePromptTokenCount\":5}}\n\n";
        let response = mock_response(data);
        let (tx, _rx) = flume::unbounded();
        let result = smol::block_on(parse_sse(response, &tx, Duration::from_secs(30))).unwrap();
        assert_eq!(result.usage.input, 55);
        assert_eq!(result.usage.output, 17);
        assert_eq!(result.usage.cache_read, 50);
        assert_eq!(result.usage.total_input(), 105);
    }

    #[test]
    fn cached_content_state_valid_when_input_and_count_match() {
        let now = Instant::now();
        let state = CachedContentState::ready("cache1".to_string(), 123, 5);
        assert_eq!(state.cached_content(123, 5, now), Some(("cache1", 5)));
        assert_eq!(state.cached_content(123, 6, now), Some(("cache1", 5)));
        assert_eq!(state.cached_content(124, 5, now), None);
        assert_eq!(state.cached_content(123, 4, now), None);
    }

    #[test]
    fn cached_content_state_invalid_after_ttl() {
        let now = Instant::now();
        let mut state = CachedContentState::ready("cache1".to_string(), 123, 5);
        state.retry_at = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(state.cached_content(123, 5, now), None);
    }

    #[test]
    fn failed_cache_creation_suppresses_matching_retry_until_delay() {
        let now = Instant::now();
        let mut state = CachedContentState::failed(123);
        assert!(state.suppress_retry(123, now));
        assert!(!state.suppress_retry(124, now));

        state.retry_at = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert!(!state.suppress_retry(123, now));
    }

    #[test]
    fn creating_cache_reserves_only_its_attempt() {
        let now = Instant::now();
        let state = CachedContentState::creating(123, 456);
        assert!(state.suppress_retry(123, now));
        assert!(state.owns_attempt(123, 456));
        assert!(!state.owns_attempt(123, 457));
        assert!(!state.owns_attempt(124, 456));
    }

    #[test]
    fn cached_input_hash_covers_all_immutable_cache_inputs() {
        let system = System::from("system");
        let tools = json!([{"name": "bash", "input_schema": {"type": "object"}}]);
        let messages = vec![
            Message::user("one".into()),
            Message::synthetic("two".into()),
            Message::user("three".into()),
            Message::synthetic("tail".into()),
        ];
        let hash = cached_input_hash("gemini-2.5-flash", &system, &tools, &messages, 3).unwrap();

        assert_eq!(
            hash,
            cached_input_hash("gemini-2.5-flash", &system, &tools, &messages, 3).unwrap()
        );
        assert_ne!(
            hash,
            cached_input_hash("gemini-2.5-pro", &system, &tools, &messages, 3).unwrap()
        );
        assert_ne!(
            hash,
            cached_input_hash(
                "gemini-2.5-flash",
                &System::from("changed"),
                &tools,
                &messages,
                3
            )
            .unwrap()
        );
        assert_ne!(
            hash,
            cached_input_hash(
                "gemini-2.5-flash",
                &system,
                &json!([{"name": "read"}]),
                &messages,
                3
            )
            .unwrap()
        );
        let mut changed_messages = messages;
        changed_messages[0] = Message::user("changed".into());
        assert_ne!(
            hash,
            cached_input_hash("gemini-2.5-flash", &system, &tools, &changed_messages, 3).unwrap()
        );
    }
}

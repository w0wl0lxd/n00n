use std::sync::{Arc, Mutex};

use flume::Sender;
use n00n_storage::id::SessionRef;
use serde_json::{Value, json};

use crate::model::{Model, ModelEntry, ModelInfo, ModelPricing};
use crate::provider::{BoxFuture, Provider};
use crate::types::{ThinkingFieldConfig, ToggleEntry};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, System, dialect};

use super::openai_compat::OpenAiCompatProvider;
use super::{KeyPool, ResolvedAuth};

include!(concat!(env!("OUT_DIR"), "/provider_configs/tensorx.rs"));

/// `TensorX` routes heterogeneous models: the discovered capabilities decide
/// whether the body carries a `thinking` toggle, a `reasoning_effort` string,
/// or both.
fn tensorx_fields(has_thinking: bool, has_reasoning_effort: bool) -> ThinkingFieldConfig {
    let toggles = if has_thinking {
        vec![ToggleEntry {
            path: "thinking".into(),
            on: Some(json!(true)),
            off: Some(json!(false)),
            ..Default::default()
        }]
    } else {
        Vec::new()
    };
    ThinkingFieldConfig {
        effort_path: has_reasoning_effort.then(|| "reasoning_effort".into()),
        toggles,
        ..Default::default()
    }
}

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "tensorx",
    display_name: "TensorX",
    protocol: n00n_config::providers::Protocol::Openai,
    default_base_url: "https://api.tensorx.ai/v1",
    default_api_key_env: "TENSORX_API_KEY",
    default_model: "tensorx/z-ai/glm-5.2",
    plans: None,
    login_url: Some("https://tensorx.ai"),
    needs_url: false,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[]
}

#[derive(Debug)]
struct TensorXModelInfo {
    has_thinking: bool,
    has_reasoning_effort: bool,
}

pub struct TensorX {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    key_pool: Option<KeyPool>,
    system_prefix: Option<String>,
}

impl TensorX {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        let pool = KeyPool::resolve("tensorx", CONFIG.api_key_env)?;
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth: Arc::new(Mutex::new(ResolvedAuth::bearer(pool.current()))),
            key_pool: Some(pool),
            system_prefix: None,
        })
    }

    pub(crate) fn with_auth(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            key_pool: None,
            system_prefix: None,
        })
    }

    pub(crate) fn with_system_prefix(mut self, prefix: Option<String>) -> Self {
        self.system_prefix = prefix;
        self
    }
}

impl Provider for TensorX {
    #[allow(clippy::result_map_or_into_option)]
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
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                session_id.map(n00n_storage::id::SessionRef::as_str),
                self.system_prefix.as_deref(),
                opts.message_cache_breakpoints,
                opts.fast,
            );

            // TensorX rejects requests whose `max_tokens` would cause
            // input+output to exceed the context window, so we rely on the
            // API's default output limit rather than send our own.
            if let Some(obj) = body.as_object_mut() {
                let _ = obj.remove("max_tokens");
            }

            let (has_thinking, has_reasoning_effort) = {
                // Discovery keys by the builtin slug; a dynamic wrap's model
                // carries its own slug, so don't key by model.provider.
                let guard = crate::model_registry::model_registry()
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let info = guard
                    .discovered("tensorx", &model.id)
                    .and_then(|d| d.provider_info.clone())
                    .and_then(|arc| {
                        Arc::downcast::<TensorXModelInfo>(arc).map_or_else(|_| None, Some)
                    });
                if let Some(info) = info {
                    (info.has_thinking, info.has_reasoning_effort)
                } else {
                    (false, false)
                }
            };

            let fields = tensorx_fields(has_thinking, has_reasoning_effort);
            opts.thinking
                .apply_thinking(&mut body, model, &dialect::TENSORX, &fields);
            // Fallback for deepseek models that use chat_template_kwargs
            if !has_thinking
                && !has_reasoning_effort
                && opts.thinking.is_enabled()
                && model.id.starts_with("deepseek/deepseek-v4")
            {
                body["chat_template_kwargs"] = json!({"thinking": true});
            }
            super::apply_body_overrides(&mut body, model, &[super::MESSAGES_FIELD]);

            self.compat
                .do_stream(model, &[], &body, event_tx, &auth, &opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async move {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let url = format!("{}/model/info", CONFIG.base_url);
            let text = self.compat.get_text(&auth, &url).await?;
            let body: Value = serde_json::from_str(&text)?;

            let mut models: Vec<ModelInfo> =
                body["data"]
                    .as_array()
                    .map_or_else(Default::default, |arr| {
                        arr.iter()
                            .filter_map(|entry| {
                                let id = entry["model_name"].as_str()?;
                                let info = entry.get("model_info")?;

                                // Only include models with mode "chat" or mode null
                                let mode_ok = info
                                    .get("mode")
                                    .and_then(|v| v.as_str())
                                    .is_none_or(|m| m == "chat");
                                if !mode_ok {
                                    return None;
                                }

                                // Context window: prefer max_tokens, fall back to max_input_tokens
                                let context_window = info["max_tokens"]
                                    .as_u64()
                                    .or_else(|| info["max_input_tokens"].as_u64())
                                    .and_then(|v| u32::try_from(v).ok());

                                // TensorX rejects explicit `max_tokens` when the requested
                                // output would push input+output past the context window. We
                                // still record the advertised output limit so the UI and
                                // thinking-budget clamping are accurate, but drop `max_tokens`
                                // from the request body before sending.
                                let max_output_tokens = info["max_output_tokens"]
                                    .as_u64()
                                    .and_then(|v| u32::try_from(v).ok());

                                // Convert per-token costs to per-million costs
                                let input_cost = info["input_cost_per_token"].as_f64();
                                let output_cost = info["output_cost_per_token"].as_f64();
                                let pricing = if input_cost.is_some() || output_cost.is_some() {
                                    let per_million = 1_000_000.0;
                                    Some(ModelPricing {
                                        input: input_cost.unwrap_or_else(|| 0.0) * per_million,
                                        output: output_cost.unwrap_or_else(|| 0.0) * per_million,
                                        cache_write: info["cache_creation_input_token_cost"]
                                            .as_f64()
                                            .unwrap_or_else(|| 0.0)
                                            * per_million,
                                        cache_read: info["cache_read_input_token_cost"]
                                            .as_f64()
                                            .unwrap_or_else(|| 0.0)
                                            * per_million,
                                        fast: None,
                                    })
                                } else {
                                    None
                                };

                                let supports_vision = info
                                    .get("supports_vision")
                                    .and_then(Value::as_bool)
                                    .unwrap_or_else(|| false);

                                let supports_thinking =
                                    info.get("supports_reasoning").and_then(Value::as_bool);

                                let supported_params = info
                                    .get("supported_openai_params")
                                    .and_then(Value::as_array)
                                    .map(|params| TensorXModelInfo {
                                        has_thinking: params
                                            .iter()
                                            .any(|v| v.as_str() == Some("thinking")),
                                        has_reasoning_effort: params
                                            .iter()
                                            .any(|v| v.as_str() == Some("reasoning_effort")),
                                    });

                                Some(ModelInfo {
                                    id: id.to_string(),
                                    name: None,
                                    context_window,
                                    max_output_tokens,
                                    pricing,
                                    supports_thinking,
                                    supports_vision: Some(supports_vision),
                                    tier: None,
                                    is_free: None,
                                    is_promo: None,
                                    provider_info: supported_params.map(|p| {
                                        Arc::new(p) as Arc<dyn std::any::Any + Send + Sync>
                                    }),
                                })
                            })
                            .collect()
                    });
            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(models)
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async {
            Ok(self
                .key_pool
                .as_ref()
                .is_some_and(|p| p.rotate_auth(&self.auth, ResolvedAuth::bearer)))
        })
    }
}

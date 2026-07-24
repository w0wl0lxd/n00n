use std::sync::{Arc, Mutex};

use flume::Sender;
use n00n_storage::id::SessionRef;
use serde_json::Value;

use crate::model::{Model, ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse};

use super::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use super::{KeyPool, ResolvedAuth};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    slug: "windsurf",
    api_key_env: "WINDSURF_API_KEY",
    base_url: "http://localhost:3003/v1",
    max_tokens_field: "max_tokens",
    include_stream_usage: true,
    provider_name: "Windsurf",
    supports_prompt_cache_key: false,
    supports_prompt_cache_breakpoint: false,
};

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "windsurf",
    display_name: "Windsurf / Devin",
    protocol: n00n_config::providers::Protocol::Openai,
    default_base_url: "http://localhost:3003/v1",
    default_api_key_env: "WINDSURF_API_KEY",
    default_model: "windsurf/claude-sonnet-4.6",
    plans: None,
    login_url: Some("https://windsurf.com/show-auth-token"),
    needs_url: true,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["claude-sonnet-4.6"],
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
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gpt-5.4"],
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
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gemini-3.1-pro"],
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
            context_window: 1_000_000,
        },
    ]
}

pub struct Windsurf {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    key_pool: Option<KeyPool>,
    system_prefix: Option<String>,
}

impl Windsurf {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        let pool = KeyPool::resolve("windsurf", CONFIG.api_key_env)?;
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

impl Provider for Windsurf {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        _opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut buf = String::new();
            let system = super::with_prefix(self.system_prefix.as_deref(), system, &mut buf);
            let body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                session_id.map(n00n_storage::id::SessionRef::as_str),
            );
            self.compat
                .do_stream(model, &[], &body, event_tx, &auth)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async move {
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            self.compat.do_list_models(&auth).await
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

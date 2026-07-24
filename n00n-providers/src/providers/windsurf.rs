use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use etcetera::BaseStrategy;
use flume::Sender;
use n00n_storage::id::SessionRef;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::model::{Model, ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse};

use super::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use super::{KeyPool, ResolvedAuth};

const WINDSURF_API_KEY_ENV: &str = "WINDSURF_API_KEY";
const DEVIN_API_KEY_ENV: &str = "DEVIN_API_KEY";

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    slug: "windsurf",
    api_key_env: DEVIN_API_KEY_ENV,
    base_url: "http://localhost:3003/v1",
    max_tokens_field: "max_tokens",
    include_stream_usage: true,
    provider_name: "Devin CLI",
    supports_prompt_cache_key: false,
    supports_prompt_cache_breakpoint: false,
};

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "windsurf",
    display_name: "Devin CLI",
    protocol: n00n_config::providers::Protocol::Openai,
    default_base_url: "http://localhost:3003/v1",
    default_api_key_env: DEVIN_API_KEY_ENV,
    default_model: "windsurf/claude-sonnet-4.6",
    plans: None,
    login_url: Some("https://docs.devin.ai/cli/enterprise/windsurf-auth"),
    needs_url: true,
});

#[derive(Debug, Clone, Deserialize)]
struct DevinCliCredentials {
    windsurf_api_key: String,
}

fn devin_credentials_path() -> Result<PathBuf, AgentError> {
    let strategy = etcetera::choose_base_strategy().map_err(|e| AgentError::Config {
        message: format!("could not resolve home directory for Devin CLI credentials: {e}"),
    })?;
    Ok(strategy.data_dir().join("devin").join("credentials.toml"))
}

fn load_devin_cli_credentials() -> Result<Option<DevinCliCredentials>, AgentError> {
    load_devin_credentials_at(&devin_credentials_path()?)
}

fn load_devin_credentials_at(
    path: &std::path::Path,
) -> Result<Option<DevinCliCredentials>, AgentError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(AgentError::Config {
                message: format!(
                    "failed to read Devin CLI credentials at {}: {e}",
                    path.display()
                ),
            });
        }
    };
    toml::from_str(&content)
        .map(Some)
        .map_err(|e| AgentError::Config {
            message: format!(
                "failed to parse Devin CLI credentials at {}: {e}",
                path.display()
            ),
        })
}

fn resolve_api_key() -> Result<KeyPool, AgentError> {
    if let Ok(pool) = KeyPool::from_env(DEVIN_API_KEY_ENV) {
        debug!(var = %DEVIN_API_KEY_ENV, source = "env", "resolved Devin API key");
        return Ok(pool);
    }
    if let Ok(pool) = KeyPool::from_env(WINDSURF_API_KEY_ENV) {
        debug!(var = %WINDSURF_API_KEY_ENV, source = "env", "resolved Devin API key from legacy env var");
        return Ok(pool);
    }
    if let Some(creds) = load_devin_cli_credentials()? {
        debug!(
            source = "credentials.toml",
            "resolved Devin API key from devin auth login"
        );
        return Ok(KeyPool::from_keys(vec![creds.windsurf_api_key]));
    }
    KeyPool::resolve(CONFIG.slug, CONFIG.api_key_env)
}

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
        let pool = resolve_api_key()?;
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn load_devin_credentials_parses_valid_toml() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"windsurf_api_key = "test-key"
api_server_url = "https://server.example.com"
devin_webapp_host = "https://webapp.example.com"
devin_api_url = "https://api.example.com"
"#
        )
        .unwrap();

        let creds = load_devin_credentials_at(file.path()).unwrap().unwrap();
        assert_eq!(creds.windsurf_api_key, "test-key");
    }

    #[test]
    fn load_devin_credentials_missing_file_returns_none() {
        let path = std::path::PathBuf::from("/nonexistent/devin/credentials.toml");
        assert!(load_devin_credentials_at(&path).unwrap().is_none());
    }

    #[test]
    fn load_devin_credentials_invalid_toml_errors() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "not valid toml [").unwrap();

        let err = load_devin_credentials_at(file.path()).unwrap_err();
        assert!(matches!(err, AgentError::Config { .. }));
    }
}

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use flume::Sender;
use serde_json::Value;
use strum::{Display, EnumIter, EnumString};
use tracing::{debug, warn};

use n00n_storage::id::SessionRef;

use crate::model::{Model, ModelFamily, ModelInfo};
use crate::providers::Timeouts;
use crate::providers::anthropic::Anthropic;
use crate::providers::anthropic::bedrock;
use crate::providers::copilot::Copilot;
use crate::providers::deepseek::DeepSeek;
use crate::providers::dynamic;
use crate::providers::google::Google;
use crate::providers::local::{LLAMACPP, LocalEndpoint, OLLAMA};
use crate::providers::mistral::Mistral;
use crate::providers::openai::OpenAi;
use crate::providers::opencode::Opencode;
use crate::providers::openrouter::OpenRouter;
use crate::providers::synthetic::Synthetic;
use crate::providers::tensorx::TensorX;
use crate::providers::windsurf::Windsurf;
use crate::providers::zai::Zai;
use crate::{
    AgentError, Message, OpenAiOptions, ProviderEvent, ProviderUsage, RequestOptions,
    StreamResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    #[strum(serialize = "openai")]
    OpenAi,
    Google,
    Copilot,
    Ollama,
    LlamaCpp,
    Mistral,
    Zai,
    #[strum(serialize = "deepseek")]
    DeepSeek,
    #[strum(serialize = "openrouter")]
    OpenRouter,
    Synthetic,
    #[strum(serialize = "tensorx")]
    TensorX,
    #[strum(serialize = "opencode")]
    Opencode,
    #[strum(serialize = "windsurf")]
    Windsurf,
}

impl ProviderKind {
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Google => "Google",
            Self::Copilot => "Copilot",
            Self::Ollama => "Ollama",
            Self::LlamaCpp => "LlamaCpp",
            Self::Mistral => "Mistral",
            Self::Zai => "Z.AI",
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::Synthetic => "Synthetic",
            Self::TensorX => "TensorX",
            Self::Opencode => "Opencode",
            Self::Windsurf => "Windsurf / Devin",
        }
    }

    #[must_use]
    pub const fn api_key_env(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Google => "GEMINI_API_KEY",
            Self::Copilot => "GH_COPILOT_TOKEN",
            Self::Ollama => "OLLAMA_API_KEY",
            Self::LlamaCpp => "LLAMA_CPP_API_KEY",
            Self::Mistral => "MISTRAL_API_KEY",
            Self::Zai => "ZHIPU_API_KEY",
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Synthetic => "SYNTHETIC_API_KEY",
            Self::TensorX => "TENSORX_API_KEY",
            Self::Opencode => "OPENCODE_API_KEY",
            Self::Windsurf => "WINDSURF_API_KEY",
        }
    }

    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Google => "https://generativelanguage.googleapis.com/v1beta",
            Self::Copilot => {
                "https://api.githubcopilot.com (or GraphQL-discovered Copilot API endpoint)"
            }
            Self::Ollama => "http://localhost:11434/v1",
            Self::LlamaCpp => "http://localhost:8080/v1",
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::Zai => "https://api.z.ai/api/paas/v4",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Synthetic => "https://api.synthetic.new/openai/v1",
            Self::TensorX => "https://api.tensorx.ai/v1",
            Self::Opencode => "https://opencode.ai/zen/v1",
            Self::Windsurf => "http://localhost:3003/v1 (WindsurfAPI/Devin Desktop proxy)",
        }
    }

    #[must_use]
    pub const fn features(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => {
                Some("Prompt caching, thinking mode (adaptive/budgeted), advanced tool use")
            }
            Self::Google => Some("Native Gemini API with thinking support"),
            Self::Copilot => Some("Native Copilot Chat HTTP API with model endpoint discovery"),
            Self::Ollama => {
                Some("Local or remote inference via OLLAMA_HOST, cloud fallback via OLLAMA_API_KEY")
            }
            Self::LlamaCpp => Some(
                "Local or remote inference via LLAMA_CPP_HOST, set optional key via LLAMA_CPP_API_KEY",
            ),
            Self::Synthetic => {
                Some("Reasoning effort support (low/medium/high), open-weight models")
            }
            Self::TensorX => Some("Open-weight models, zero data retention, prompt caching"),
            Self::DeepSeek => Some("Thinking mode toggle (on/off), open-weight models"),
            Self::OpenRouter => {
                Some("300+ models from all providers, prompt caching, provider routing")
            }
            Self::Opencode => Some(
                "Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Zen API",
            ),
            Self::Windsurf => Some(
                "OpenAI-compatible endpoint for the Windsurf / Devin Desktop proxy; fully configurable base URL and model",
            ),
            _ => None,
        }
    }

    #[must_use]
    pub const fn family(self) -> ModelFamily {
        match self {
            Self::Anthropic => ModelFamily::Claude,
            Self::OpenAi => ModelFamily::Gpt,
            Self::Google => ModelFamily::Gemini,
            Self::Copilot
            | Self::Ollama
            | Self::LlamaCpp
            | Self::Mistral
            | Self::DeepSeek
            | Self::OpenRouter
            | Self::TensorX
            | Self::Opencode
            | Self::Windsurf => ModelFamily::Generic,
            Self::Zai => ModelFamily::Glm,
            Self::Synthetic => ModelFamily::Synthetic,
        }
    }

    /// `None` when we honestly don't know the output window: llama.cpp
    /// serves whatever model the user loaded, and `TensorX` rejects explicit
    /// `max_tokens` (see tensorx.rs). Unknown means "don't limit", never
    /// "assume small"; a `0` sentinel here once silently capped llama.cpp
    /// thinking budgets at the floor.
    #[must_use]
    pub const fn fallback_max_output(self) -> Option<u32> {
        match self {
            Self::OpenAi | Self::Copilot => Some(100_000),
            Self::Google => Some(65_536),
            Self::Anthropic | Self::OpenRouter | Self::Opencode | Self::Windsurf => Some(128_000),
            Self::Ollama => Some(16_384),
            Self::LlamaCpp | Self::TensorX => None,
            Self::Mistral | Self::Synthetic => Some(32_000),
            Self::Zai => Some(16_000),
            Self::DeepSeek => Some(384_000),
        }
    }

    #[must_use]
    pub const fn fallback_context_window(self) -> u32 {
        match self {
            Self::Anthropic
            | Self::OpenAi
            | Self::Copilot
            | Self::OpenRouter
            | Self::TensorX
            | Self::Windsurf => 200_000,
            Self::Google | Self::DeepSeek => 1_000_000,
            Self::Ollama | Self::LlamaCpp | Self::Mistral | Self::Zai | Self::Synthetic => 128_000,
            Self::Opencode => 256_000,
        }
    }

    /// Creates a new provider instance for this kind.
    ///
    /// # Errors
    /// Returns an error if the provider's configuration is missing or invalid
    /// (e.g., missing API key, invalid base URL, or provider-specific setup failure).
    pub fn create(self, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
        if self == Self::OpenAi {
            return Ok(Box::new(OpenAi::new(timeouts)?));
        }
        self.create_with_openai_options(timeouts, OpenAiOptions::default())
    }

    /// Creates a provider with OpenAI-specific runtime options.
    ///
    /// # Errors
    /// Returns an error if the provider configuration is missing or invalid.
    pub fn create_with_openai_options(
        self,
        timeouts: Timeouts,
        openai_options: OpenAiOptions,
    ) -> Result<Box<dyn Provider>, AgentError> {
        match self {
            Self::Anthropic => {
                if bedrock::is_enabled() {
                    Ok(Box::new(bedrock::Bedrock::new(timeouts)?))
                } else {
                    Ok(Box::new(Anthropic::new(timeouts)?))
                }
            }
            Self::OpenAi => Ok(Box::new(OpenAi::new_with_options(
                timeouts,
                openai_options,
            )?)),
            Self::Google => Ok(Box::new(Google::new(timeouts)?)),
            Self::Copilot => Ok(Box::new(Copilot::new(timeouts)?)),
            Self::Ollama => Ok(Box::new(LocalEndpoint::new(&OLLAMA, timeouts)?)),
            Self::LlamaCpp => Ok(Box::new(LocalEndpoint::new(&LLAMACPP, timeouts)?)),
            Self::Mistral => Ok(Box::new(Mistral::new(timeouts)?)),
            Self::Zai => Ok(Box::new(Zai::new(timeouts)?)),
            Self::DeepSeek => Ok(Box::new(DeepSeek::new(timeouts)?)),
            Self::OpenRouter => Ok(Box::new(OpenRouter::new(timeouts)?)),
            Self::Synthetic => Ok(Box::new(Synthetic::new(timeouts)?)),
            Self::TensorX => Ok(Box::new(TensorX::new(timeouts)?)),
            Self::Opencode => Ok(Box::new(Opencode::new(timeouts)?)),
            Self::Windsurf => Ok(Box::new(Windsurf::new(timeouts)?)),
        }
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Provider: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>>;

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>>;

    /// Fetch provider-side usage quota (remaining percentage / reset times).
    /// `Ok(None)` means the provider does not expose a programmatic usage endpoint.
    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async { Ok(None) })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }

    fn adjust_model(&self, _model: &mut Model) {}
}

/// Create a provider for the given slug.
///
/// # Errors
///
/// Returns `AgentError` if the slug does not match a builtin, dynamic,
/// or custom provider, or if provider construction fails.
pub fn provider_for_slug(slug: &str, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    if let Ok(kind) = ProviderKind::from_str(slug) {
        return kind.create(timeouts);
    }
    if dynamic::display_name(slug).is_some() {
        dynamic::create(slug, timeouts)
    } else {
        crate::providers::custom::create(slug, timeouts)
    }
}

#[must_use]
pub fn provider_available(slug: &str) -> bool {
    provider_for_slug(slug, Timeouts::default()).is_ok()
}

/// Create a provider for a resolved model, applying provider-specific adjustments.
///
/// # Errors
///
/// Returns `AgentError` if the provider cannot be created.
pub fn from_model(model: &mut Model, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    from_model_with_openai_options(model, timeouts, OpenAiOptions::default())
}

/// Create a provider for a resolved model with OpenAI-compatible options.
///
/// # Errors
///
/// Returns `AgentError` if the provider cannot be created.
pub fn from_model_with_openai_options(
    model: &mut Model,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    if let Ok(kind) = ProviderKind::from_str(&model.provider) {
        let provider = kind.create_with_openai_options(timeouts, openai_options)?;
        provider.adjust_model(model);
        debug!(provider = %model.provider, model = %model.id, "provider created");
        return Ok(provider);
    }
    let provider = provider_for_slug(&model.provider, timeouts)?;
    provider.adjust_model(model);
    debug!(provider = %model.provider, model = %model.id, "provider created");
    Ok(provider)
}

pub fn from_model_fallback(model: &mut Model, timeouts: Timeouts) -> Box<dyn Provider> {
    from_model_fallback_with_openai_options(model, timeouts, OpenAiOptions::default())
}

#[must_use]
pub fn from_model_fallback_with_openai_options(
    model: &mut Model,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
) -> Box<dyn Provider> {
    match from_model_with_openai_options(model, timeouts, openai_options) {
        Ok(provider) => provider,
        Err(e) => {
            warn!(error = %e, "provider creation failed, using unconfigured provider");
            Box::new(UnconfiguredProvider)
        }
    }
}

struct UnconfiguredProvider;

const NOT_CONFIGURED: &str = "no provider configured — run /login or `n00n auth login`";

impl Provider for UnconfiguredProvider {
    fn stream_message<'a>(
        &'a self,
        _model: &'a Model,
        _messages: &'a [Message],
        _system: &'a str,
        _tools: &'a Value,
        _event_tx: &'a Sender<ProviderEvent>,
        _opts: RequestOptions,
        _session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async {
            Err(AgentError::Config {
                message: NOT_CONFIGURED.to_string(),
            })
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async {
            Err(AgentError::Config {
                message: NOT_CONFIGURED.to_string(),
            })
        })
    }
}

/// Creates a provider instance from a model configuration asynchronously.
///
/// # Errors
/// Returns an error if the provider cannot be created (e.g., missing API key,
/// invalid configuration, or provider-specific setup failure).
pub async fn from_model_async(
    model: &mut Model,
    timeouts: Timeouts,
) -> Result<Box<dyn Provider>, AgentError> {
    let slug = Arc::clone(&model.provider);
    let id = model.id.clone();
    let provider = smol::unblock(move || provider_for_slug(&slug, timeouts)).await?;
    provider.adjust_model(model);
    debug!(provider = %model.provider, model = %id, "provider created");
    Ok(provider)
}

pub struct ModelBatch {
    pub models: Vec<String>,
    pub warnings: Vec<String>,
}

/// Offline version of model discovery: returns specs from static tables
/// and configured dynamic providers. See [`fetch_all_models`] for live lookups.
#[must_use]
pub fn available_model_specs() -> Vec<String> {
    let mut specs: Vec<String> = crate::manifest::ManifestRegistry::builtins()
        .iter()
        .filter(|m| provider_available(m.slug))
        .flat_map(|m| {
            m.models
                .iter()
                .flat_map(|entry| entry.prefixes.iter())
                .map(move |p| format!("{}/{}", m.slug, p))
        })
        .collect();
    for slug in dynamic::discovered_slugs() {
        specs.extend(dynamic::dynamic_model_specs_for(slug));
    }
    for spec in crate::providers::custom::declared_model_specs() {
        if !specs.contains(&spec) {
            specs.push(spec);
        }
    }
    specs
}

#[cfg(test)]
fn ollama_is_configured(has_host: bool, has_api_key: bool, has_provider_config: bool) -> bool {
    has_host || has_api_key || has_provider_config
}

#[cfg(test)]
fn llama_cpp_is_configured(has_host: bool, has_api_key: bool, has_provider_config: bool) -> bool {
    has_host || has_api_key || has_provider_config
}

/// Fetches all available models from all providers asynchronously.
#[allow(clippy::too_many_lines)]
pub async fn fetch_all_models(
    mut on_ready: impl FnMut(ModelBatch),
    on_done: Option<Box<dyn FnOnce() + Send>>,
) {
    let (tx, rx) = flume::unbounded();
    let timeouts = Timeouts::default();

    for manifest in crate::manifest::ManifestRegistry::builtins() {
        let slug = manifest.slug;
        let Ok(provider) = smol::unblock(move || provider_for_slug(slug, timeouts)).await else {
            warn!(provider = slug, "failed to create provider, skipping");
            continue;
        };
        let display_name = manifest.display_name;
        let tx = tx.clone();
        smol::spawn(async move {
            let batch = match provider.list_models().await {
                Ok(models) => {
                    if manifest.accepts_arbitrary_models {
                        let slug: Arc<str> = Arc::from(slug);
                        crate::model_registry::model_registry()
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .set_known_models(&slug, models.clone());
                    }
                    let mut specs: Vec<String> =
                        models.iter().map(|m| format!("{slug}/{}", m.id)).collect();
                    for entry in manifest.models {
                        for prefix in entry.prefixes {
                            let spec = format!("{slug}/{prefix}");
                            if !specs.contains(&spec) {
                                specs.push(spec);
                            }
                        }
                    }
                    ModelBatch {
                        models: specs,
                        warnings: Vec::new(),
                    }
                }
                Err(e) => {
                    warn!(provider = slug, error = %e, "failed to list models, using static fallback");
                    let fallback: Vec<String> = manifest
                        .models
                        .iter()
                        .flat_map(|entry| entry.prefixes.iter())
                        .map(|p| format!("{slug}/{p}"))
                        .collect();
                    ModelBatch {
                        models: fallback,
                        warnings: vec![format!(
                            "{display_name}: {e} (using static fallback)"
                        )],
                    }
                }
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }

    for slug in dynamic::discovered_slugs() {
        let tx = tx.clone();
        let slug = slug.to_string();
        smol::spawn(async move {
            let static_fallback = |reason: String| {
                warn!(
                    slug,
                    error = reason,
                    "dynamic model listing failed, using static fallback"
                );
                ModelBatch {
                    models: dynamic::dynamic_model_specs_for(&slug),
                    warnings: vec![format!("{slug}: {reason} (using static fallback)")],
                }
            };
            let batch = match dynamic::create(&slug, timeouts) {
                Ok(provider) => match provider.list_models().await {
                    Ok(models) => ModelBatch {
                        models: models.iter().map(|m| format!("{slug}/{}", m.id)).collect(),
                        warnings: Vec::new(),
                    },
                    Err(e) => static_fallback(e.to_string()),
                },
                Err(e) => static_fallback(e.to_string()),
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }

    let custom_timeouts = timeouts;
    let tx_custom = tx.clone();
    smol::spawn(async move {
        let declared = crate::providers::custom::declared_model_specs();
        if !declared.is_empty() {
            let _ = tx_custom
                .send_async(ModelBatch {
                    models: declared,
                    warnings: Vec::new(),
                })
                .await;
        }
        let custom_specs =
            smol::unblock(move || crate::providers::custom::discover_models(custom_timeouts)).await;
        if !custom_specs.is_empty() {
            let _ = tx_custom
                .send_async(ModelBatch {
                    models: custom_specs,
                    warnings: Vec::new(),
                })
                .await;
        }
    })
    .detach();

    drop(tx);

    while let Ok(batch) = rx.recv_async().await {
        on_ready(batch);
    }
    if let Some(done) = on_done {
        done();
    }
}

#[cfg(test)]
mod tests {
    use super::{llama_cpp_is_configured, ollama_is_configured};

    #[test_case::test_case(false, false, false => false; "unconfigured")]
    #[test_case::test_case(true, false, false => true; "host")]
    #[test_case::test_case(false, true, false => true; "api_key")]
    #[test_case::test_case(false, false, true => true; "provider_config")]
    fn llama_cpp_configuration_sources(
        has_host: bool,
        has_api_key: bool,
        has_provider_config: bool,
    ) -> bool {
        // Keep this pure rather than mutating process environment variables, whose values are
        // shared with concurrently running tests.
        llama_cpp_is_configured(has_host, has_api_key, has_provider_config)
    }

    #[test_case::test_case(false, false, false => false; "unconfigured")]
    #[test_case::test_case(true, false, false => true; "host")]
    #[test_case::test_case(false, true, false => true; "api_key")]
    #[test_case::test_case(false, false, true => true; "provider_config")]
    fn ollama_configuration_sources(
        has_host: bool,
        has_api_key: bool,
        has_provider_config: bool,
    ) -> bool {
        ollama_is_configured(has_host, has_api_key, has_provider_config)
    }
}

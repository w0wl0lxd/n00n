use std::sync::Mutex;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_config::providers::ProvidersConfig;
use n00n_providers::model::{Model, ModelTier};
use n00n_storage::StateDir;
use n00n_storage::log::RotatingFileWriter;
use n00n_storage::model::read_model;
use tracing_subscriber::EnvFilter;

const PROVIDER_PRIORITY: &[&str] = &[
    "anthropic",
    "openai",
    "copilot",
    "zai",
    "synthetic",
    "deepseek",
];

pub fn resolve_model(
    explicit: Option<&str>,
    provider_config: &n00n_config::ProviderConfig,
    providers_toml: &ProvidersConfig,
    storage: &StateDir,
) -> Result<Model> {
    let configured_slugs: Option<Vec<&str>> = if providers_toml.providers.is_empty() {
        None
    } else {
        Some(
            providers_toml
                .providers
                .keys()
                .map(String::as_str)
                .collect(),
        )
    };

    if let Some(spec) = explicit {
        let model = Model::from_spec(spec).context("invalid --model spec")?;
        return Ok(model);
    }
    if let Some(spec) = read_model(storage) {
        if let Ok(m) = Model::from_spec(&spec) {
            if provider_allowed(&m.provider, configured_slugs.as_deref())
                && n00n_providers::provider::provider_available(&m.provider)
            {
                return Ok(m);
            }
            tracing::warn!(
                spec,
                provider = %m.provider,
                "saved model provider is not available, falling back to default"
            );
        } else {
            tracing::warn!(spec, "saved model no longer valid, falling back to default");
        }
    }
    if let Some(spec) = provider_config.default_model.as_deref() {
        let model = Model::from_spec(spec).context("invalid default_model in config")?;
        if provider_allowed(&model.provider, configured_slugs.as_deref()) {
            return Ok(model);
        }
        tracing::warn!(
            spec,
            provider = %model.provider,
            "default_model provider is not in providers.toml, falling back to auto-detection"
        );
    }
    auto_detect_model(configured_slugs.as_deref()).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "no provider available - set an API key (e.g. ANTHROPIC_API_KEY), run `n00n auth login`, or use -m to specify a model\n\nSee https://github.com/w0wl0lxd/n00n/docs/providers/ for setup instructions"
        )
    })
}

pub(crate) fn provider_allowed(provider: &str, configured_slugs: Option<&[&str]>) -> bool {
    let Some(slugs) = configured_slugs else {
        return true;
    };
    slugs.contains(&provider)
}

fn auto_detect_model(configured_slugs: Option<&[&str]>) -> Option<Model> {
    let slugs: Vec<&str> = match configured_slugs {
        Some(s) => {
            let mut ordered: Vec<&str> = PROVIDER_PRIORITY
                .iter()
                .copied()
                .filter(|p| s.contains(p))
                .collect();
            let builtins: std::collections::HashSet<&str> =
                PROVIDER_PRIORITY.iter().copied().collect();
            let mut extras: Vec<&str> = s
                .iter()
                .copied()
                .filter(|p| !builtins.contains(p))
                .collect();
            extras.sort_unstable();
            ordered.extend(extras);
            ordered
        }
        None => PROVIDER_PRIORITY.to_vec(),
    };
    for tier in [ModelTier::Strong, ModelTier::Medium] {
        for &slug in &slugs {
            if !provider_allowed(slug, configured_slugs) {
                continue;
            }
            if n00n_providers::provider::provider_available(slug)
                && let Ok(model) = Model::from_tier_dynamic(slug, tier)
            {
                return Some(model);
            }
        }
    }
    None
}

pub fn install_panic_log_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".into()
        };
        let location = info.location().map(std::string::ToString::to_string);
        tracing::error!(
            panic.payload = %payload,
            panic.location = location.as_deref().unwrap_or_else(|| "<unknown>"),
            "panic occurred"
        );
        prev(info);
    }));
}

pub fn init_logging(storage_config: &n00n_config::StorageConfig) {
    let Ok(writer) =
        RotatingFileWriter::new(storage_config.max_log_bytes, storage_config.max_log_files)
    else {
        return;
    };
    let writer = Mutex::new(writer);
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_config::ProviderConfig;
    use n00n_config::providers::{ProviderDef, ProvidersConfig};
    use n00n_storage::StateDir;
    use n00n_storage::model::persist_model;
    use tempfile::TempDir;

    fn temp_state() -> (TempDir, StateDir) {
        let dir = TempDir::new().expect("temp dir");
        let state = StateDir::from_path(dir.path().to_path_buf());
        (dir, state)
    }

    #[test]
    fn provider_allowed_with_empty_config_allows_any() {
        assert!(provider_allowed("openai", None));
        assert!(!provider_allowed("anthropic", Some(&[])));
    }

    #[test]
    fn provider_allowed_with_config_allows_only_configured() {
        let slugs = ["anthropic"];
        assert!(provider_allowed("anthropic", Some(&slugs)));
        assert!(!provider_allowed("openai", Some(&slugs)));
    }

    #[test]
    fn resolve_explicit_ignores_providers_toml() {
        let (_, state) = temp_state();
        let provider_config = ProviderConfig::default();
        let providers_toml = {
            let mut c = ProvidersConfig::default();
            c.upsert("anthropic".into(), ProviderDef::default());
            c
        };
        let model = resolve_model(
            Some("anthropic/claude-opus-4-6"),
            &provider_config,
            &providers_toml,
            &state,
        )
        .expect("explicit model should resolve");
        assert_eq!(model.spec(), "anthropic/claude-opus-4-6");
    }

    #[test]
    fn resolve_default_model_skips_provider_not_in_providers_toml() {
        let (_, state) = temp_state();
        let provider_config = ProviderConfig {
            default_model: Some("openai/gpt-5.6-sol".into()),
            ..ProviderConfig::default()
        };
        let providers_toml = {
            let mut c = ProvidersConfig::default();
            c.upsert("anthropic".into(), ProviderDef::default());
            c
        };
        let result = resolve_model(None, &provider_config, &providers_toml, &state);
        assert!(
            result.is_err(),
            "default_model should be skipped when not in providers.toml"
        );
    }

    #[test]
    fn resolve_default_model_from_provider_config() {
        let (_, state) = temp_state();
        let provider_config = ProviderConfig {
            default_model: Some("anthropic/claude-opus-4-6".into()),
            ..ProviderConfig::default()
        };
        let model = resolve_model(None, &provider_config, &ProvidersConfig::default(), &state)
            .expect("default_model should resolve");
        assert_eq!(model.spec(), "anthropic/claude-opus-4-6");
    }

    #[test]
    fn resolve_saved_ignores_provider_not_in_providers_toml() {
        let (dir, state) = temp_state();
        persist_model(&state, "openai/gpt-5.6-sol");
        let provider_config = ProviderConfig::default();
        let providers_toml = {
            let mut c = ProvidersConfig::default();
            c.upsert("my-custom".into(), ProviderDef::default());
            c
        };
        let result = resolve_model(None, &provider_config, &providers_toml, &state);
        assert!(
            result.is_err(),
            "saved openai should be skipped when not in providers.toml"
        );
        drop(dir);
    }
}

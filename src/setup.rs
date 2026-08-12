use std::collections::HashSet;
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
    "codex",
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
    resolve_model_with_fusion(explicit, provider_config, providers_toml, storage, None)
}

pub fn resolve_model_with_fusion(
    explicit: Option<&str>,
    provider_config: &n00n_config::ProviderConfig,
    providers_toml: &ProvidersConfig,
    storage: &StateDir,
    fusion: Option<&n00n_config::FusionConfig>,
) -> Result<Model> {
    resolve_model_with_availability(
        explicit,
        provider_config,
        providers_toml,
        storage,
        fusion,
        n00n_providers::provider::provider_available,
    )
}

fn resolve_model_with_availability(
    explicit: Option<&str>,
    provider_config: &n00n_config::ProviderConfig,
    providers_toml: &ProvidersConfig,
    storage: &StateDir,
    fusion: Option<&n00n_config::FusionConfig>,
    provider_available: impl Fn(&str) -> bool,
) -> Result<Model> {
    if let Some(spec) = explicit {
        return Model::from_spec(spec).context("invalid --model spec");
    }
    if let Some(fusion) = fusion.filter(|fusion| fusion.enabled) {
        return Model::from_spec(&fusion.lead_model).context("invalid Fusion lead model");
    }
    if let Some(spec) = read_model(storage) {
        match Model::from_spec(&spec) {
            Ok(model) if provider_available(&model.provider) => return Ok(model),
            Ok(model) => tracing::warn!(
                spec,
                provider = %model.provider,
                "saved model provider is not available, falling back to configured defaults"
            ),
            Err(_) => tracing::warn!(
                spec,
                "saved model no longer valid, falling back to configured defaults"
            ),
        }
    }
    if let Some(spec) = provider_config.default_model.as_deref() {
        return Model::from_spec(spec).context("invalid default_model in config");
    }
    auto_detect_model(providers_toml, provider_available).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "no provider available - set an API key (e.g. ANTHROPIC_API_KEY), run `n00n auth login`, or use -m to specify a model\n\nSee https://github.com/w0wl0lxd/n00n/docs/providers/ for setup instructions"
        )
    })
}

pub(crate) fn available_model_from_spec(spec: &str) -> Option<Model> {
    let model = Model::from_spec(spec).ok()?;
    n00n_providers::provider::provider_available(&model.provider).then_some(model)
}

fn provider_candidates<'a>(
    providers_toml: &'a ProvidersConfig,
    dynamic_slugs: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for &slug in PROVIDER_PRIORITY {
        if seen.insert(slug) {
            candidates.push(slug);
        }
    }

    let mut configured: Vec<&str> = providers_toml
        .providers
        .keys()
        .map(String::as_str)
        .collect();
    configured.sort_unstable();
    for slug in configured {
        if seen.insert(slug) {
            candidates.push(slug);
        }
    }

    let mut dynamic: Vec<&str> = dynamic_slugs.into_iter().collect();
    dynamic.sort_unstable();
    for slug in dynamic {
        if seen.insert(slug) {
            candidates.push(slug);
        }
    }

    candidates
}

fn auto_detect_model(
    providers_toml: &ProvidersConfig,
    provider_available: impl Fn(&str) -> bool,
) -> Option<Model> {
    let dynamic_slugs = n00n_providers::dynamic::discovered_slugs();
    let candidates = provider_candidates(providers_toml, dynamic_slugs);
    for tier in [ModelTier::Strong, ModelTier::Medium] {
        for &slug in &candidates {
            if provider_available(slug)
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
    use n00n_config::providers::{ProviderDef, ProvidersConfig};
    use n00n_config::{FusionConfig, ProviderConfig};
    use n00n_storage::model::persist_model;
    use tempfile::TempDir;

    use super::*;

    fn temp_state() -> (TempDir, StateDir) {
        let temp = TempDir::new().unwrap();
        let storage = StateDir::from_path(temp.path().to_path_buf());
        (temp, storage)
    }

    #[test]
    fn provider_candidates_add_configured_and_dynamic_without_filtering_builtins() {
        let mut providers = ProvidersConfig::default();
        providers.upsert("z-custom".into(), ProviderDef::default());
        providers.upsert("a-custom".into(), ProviderDef::default());
        providers.upsert("anthropic".into(), ProviderDef::default());

        let candidates = provider_candidates(&providers, ["z-script", "a-script"]);

        assert_eq!(
            candidates,
            [
                "anthropic",
                "openai",
                "codex",
                "copilot",
                "zai",
                "synthetic",
                "deepseek",
                "a-custom",
                "z-custom",
                "a-script",
                "z-script",
            ]
        );
    }

    #[test]
    fn explicit_model_overrides_fusion_and_saved_model() {
        let (_temp, storage) = temp_state();
        persist_model(&storage, "codex/gpt-5.5");
        let fusion = FusionConfig {
            enabled: true,
            ..FusionConfig::default()
        };

        let model = resolve_model_with_availability(
            Some("codex/gpt-5.6-terra"),
            &ProviderConfig::default(),
            &ProvidersConfig::default(),
            &storage,
            Some(&fusion),
            |_| false,
        )
        .unwrap();

        assert_eq!(model.spec(), "codex/gpt-5.6-terra");
    }

    #[test]
    fn fusion_lead_overrides_saved_and_provider_defaults() {
        let (_temp, storage) = temp_state();
        persist_model(&storage, "codex/gpt-5.5");
        let provider = ProviderConfig {
            default_model: Some("codex/gpt-5.4".to_owned()),
            ..ProviderConfig::default()
        };
        let fusion = FusionConfig {
            enabled: true,
            ..FusionConfig::default()
        };

        let model = resolve_model_with_availability(
            None,
            &provider,
            &ProvidersConfig::default(),
            &storage,
            Some(&fusion),
            |_| false,
        )
        .unwrap();

        assert_eq!(model.spec(), "codex/gpt-5.6-sol");
    }

    #[test]
    fn available_saved_model_overrides_provider_default() {
        let (_temp, storage) = temp_state();
        persist_model(&storage, "codex/gpt-5.5");
        let provider = ProviderConfig {
            default_model: Some("anthropic/claude-opus-4-6".to_owned()),
            ..ProviderConfig::default()
        };

        let model = resolve_model_with_availability(
            None,
            &provider,
            &ProvidersConfig::default(),
            &storage,
            None,
            |slug| slug == "codex",
        )
        .unwrap();

        assert_eq!(model.spec(), "codex/gpt-5.5");
    }

    #[test]
    fn unavailable_saved_model_falls_back_to_configured_default() {
        let (_temp, storage) = temp_state();
        persist_model(&storage, "openai/gpt-5.6-sol");
        let mut providers = ProvidersConfig::default();
        providers.upsert("my-custom".into(), ProviderDef::default());
        let provider = ProviderConfig {
            default_model: Some("anthropic/claude-opus-4-6".to_owned()),
            ..ProviderConfig::default()
        };

        let model =
            resolve_model_with_availability(None, &provider, &providers, &storage, None, |_| false)
                .unwrap();

        assert_eq!(model.spec(), "anthropic/claude-opus-4-6");
    }

    #[test]
    fn provider_default_is_intentional_even_when_not_currently_available() {
        let (_temp, storage) = temp_state();
        let provider = ProviderConfig {
            default_model: Some("openai/gpt-5.6-sol".to_owned()),
            ..ProviderConfig::default()
        };
        let mut providers = ProvidersConfig::default();
        providers.upsert("my-custom".into(), ProviderDef::default());

        let model =
            resolve_model_with_availability(None, &provider, &providers, &storage, None, |_| false)
                .unwrap();

        assert_eq!(model.spec(), "openai/gpt-5.6-sol");
    }

    #[test]
    fn disabled_fusion_preserves_available_saved_model() {
        let (_temp, storage) = temp_state();
        persist_model(&storage, "codex/gpt-5.5");

        let model = resolve_model_with_availability(
            None,
            &ProviderConfig::default(),
            &ProvidersConfig::default(),
            &storage,
            Some(&FusionConfig::default()),
            |slug| slug == "codex",
        )
        .unwrap();

        assert_eq!(model.spec(), "codex/gpt-5.5");
    }

    #[test]
    fn auto_detection_uses_first_available_provider_by_tier_and_priority() {
        let (_temp, storage) = temp_state();

        let model = resolve_model_with_availability(
            None,
            &ProviderConfig::default(),
            &ProvidersConfig::default(),
            &storage,
            None,
            |slug| slug == "zai",
        )
        .unwrap();

        assert_eq!(model.provider.as_ref(), "zai");
        assert_eq!(model.tier, ModelTier::Strong);
    }
}

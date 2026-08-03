use std::sync::Mutex;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_providers::model::ModelTier;
use n00n_providers::{Model, ModelResolver};
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
    storage: &StateDir,
) -> Result<Model> {
    let resolver = ModelResolver::current();
    if let Some(spec) = explicit {
        return resolver
            .resolve(spec)
            .context("model is not configured or available");
    }
    if let Some(spec) = read_model(storage) {
        if let Ok(model) = resolver.resolve(&spec) {
            return Ok(model);
        }
        tracing::warn!(
            spec,
            "saved model is no longer configured or available, falling back to default"
        );
    }
    if let Some(spec) = provider_config.default_model.as_deref() {
        return resolver
            .resolve(spec)
            .context("default_model is not configured or available");
    }
    auto_detect_model(&resolver).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "no provider available - set an API key (e.g. ANTHROPIC_API_KEY), run `n00n auth login`, or use -m to specify a model\n\nSee https://github.com/w0wl0lxd/n00n/blob/main/site/docs/content/providers/_index.md for setup instructions"
        )
    })
}

fn auto_detect_model(resolver: &ModelResolver) -> Option<Model> {
    for tier in [ModelTier::Strong, ModelTier::Medium] {
        for &slug in PROVIDER_PRIORITY {
            if let Ok(model) = Model::from_tier(slug, tier)
                && let Ok(model) = resolver.resolve(&model.spec())
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

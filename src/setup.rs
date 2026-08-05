use std::sync::Mutex;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_providers::model::{Model, ModelTier};
use n00n_providers::provider;
use n00n_storage::StateDir;
use n00n_storage::log::RotatingFileWriter;
use n00n_storage::model::{read_model, read_recents};
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
    storage: &StateDir,
) -> Result<Model> {
    if let Some(spec) = explicit {
        let model = Model::from_spec(spec).context("invalid --model spec")?;
        return Ok(model);
    }
    if let Some(spec) = read_model(storage) {
        if let Ok(m) = Model::from_spec(&spec) {
            return Ok(m);
        }
        tracing::warn!(spec, "saved model no longer valid, falling back to default");
    }
    if let Some(spec) = provider_config.default_model.as_deref() {
        return Model::from_spec(spec).context("invalid default_model in config");
    }
    auto_detect_model().ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "no provider available - set an API key (e.g. ANTHROPIC_API_KEY), run `n00n auth login`, or use -m to specify a model\n\nSee https://github.com/w0wl0lxd/n00n/docs/providers/ for setup instructions"
        )
    })
}

pub fn auto_detect_model() -> Option<Model> {
    auto_detect_model_preferred(None)
}

pub fn auto_detect_model_preferred(preferred: Option<&[&str]>) -> Option<Model> {
    let providers = preferred.unwrap_or_else(|| PROVIDER_PRIORITY);
    for tier in [ModelTier::Strong, ModelTier::Medium] {
        for &slug in providers {
            if provider::provider_available(slug)
                && let Ok(model) = Model::from_tier(slug, tier)
            {
                return Some(model);
            }
        }
    }
    None
}

pub fn fallback_to_recent_model(storage: &StateDir) -> Option<Model> {
    let recents = read_recents(storage);
    for spec in recents {
        if let Ok(model) = Model::from_spec(&spec)
            && provider::provider_available(&model.provider)
        {
            return Some(model);
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

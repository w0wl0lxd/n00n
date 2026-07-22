// clippy.toml forbids `unwrap_or`/`unwrap_or_default`; the only alternative is
// the lazy `unwrap_or_else` form, which triggers `unwrap_or_default`.
// `Default::default` in those closures also triggers `default_trait_access`.
// Allow these style lints crate-wide so we can keep using the
// disallowed-method-safe form without per-function attributes.
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::default_trait_access)]

pub(crate) mod error;
pub mod model;
pub mod model_registry;
pub mod provider;
pub(crate) mod providers;
pub mod retry;
pub(crate) mod types;

pub use error::{AgentError, RequestDeliveryMetadata, RequestDeliveryPhase};
pub use model::{
    FastPricing, Model, ModelEntry, ModelError, ModelFamily, ModelInfo, ModelPricing, ModelTier,
    TokenUsage, models_for_provider,
};
pub use providers::Timeouts;
pub use providers::copilot::auth as copilot_auth;
pub use providers::dynamic;
pub use providers::openai::OpenAiOptions;
pub use providers::openai::auth as openai_auth;
pub use providers::opencode::{
    ProviderData, catalog_provider, catalog_providers, catalog_providers_if_available,
};
pub use types::{
    ContentBlock, Effort, EffortDialect, IMAGE_OMITTED_NOTE, ImageMediaType, ImageSource, Message,
    ProviderEvent, ProviderUsage, RequestOptions, Role, StopReason, StreamResponse, ThinkingConfig,
    UsageLimit, adapt_images_for_model, dialect,
};

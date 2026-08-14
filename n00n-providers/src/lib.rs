// clippy.toml forbids `unwrap_or`/`unwrap_or_default`; the only alternative is
// the lazy `unwrap_or_else` form, which triggers `unwrap_or_default`.
// `Default::default` in those closures also triggers `default_trait_access`.
// Allow these style lints crate-wide so we can keep using the
// disallowed-method-safe form without per-function attributes.
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::default_trait_access)]

pub mod admission;
pub(crate) mod error;
pub mod manifest;
pub mod model;
pub mod model_catalog;
pub mod model_registry;
pub mod provider;
pub(crate) mod providers;
pub mod retry;
pub(crate) mod types;

pub use error::{AgentError, HistoryReplayReason, RequestDeliveryMetadata, RequestDeliveryPhase};
pub use model::{
    FastPricing, Model, ModelEntry, ModelError, ModelFamily, ModelInfo, ModelPricing, ModelTier,
    TokenUsage,
};
pub use model_catalog::{ModelCatalog, ModelCatalogError, ModelResolver};
pub use providers::Timeouts;
pub use providers::copilot::auth as copilot_auth;
pub use providers::dynamic;
pub use providers::openai::OpenAiOptions;
pub use providers::openai::auth as openai_auth;
pub use providers::openai::websocket::ensure_rustls_crypto_provider;
pub use providers::opencode::{
    ProviderData, catalog_provider, catalog_providers, catalog_providers_if_available,
};
pub use types::{
    BodyOverride, CacheControl, CacheHealth, CacheKind, ContentBlock, Effort, EffortDialect,
    EffortDialectId, FILE_OMITTED_NOTE, FileSource, IMAGE_OMITTED_NOTE, ImageDetail,
    ImageMediaType, ImageSource, Message, ProviderEvent, ProviderUsage, ReasoningContext,
    ReasoningMode, RequestOptions, Role, StopReason, StreamResponse, System, SystemBlock,
    ThinkingConfig, ThinkingExtras, ThinkingFieldConfig, ToggleEntry, UsageLimit,
    adapt_files_for_model, adapt_images_for_model, dialect, effort_dialect_for,
};

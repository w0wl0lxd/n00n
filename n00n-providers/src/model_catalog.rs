//! Authenticated model discovery and fail-closed model selection.
//!
//! `Model::from_spec` is deliberately permissive because providers need to
//! construct requests for static metadata and provider-specific fallbacks.
//! User-controlled model selections must go through [`ModelResolver`] instead.

use std::collections::BTreeSet;

use crate::manifest::ManifestRegistry;
use crate::model::{Model, ModelError, ModelInfo};
use crate::provider::provider_for_slug;
use crate::providers::{Timeouts, custom, dynamic};

/// The models a provider has exposed during the current authenticated
/// discovery pass. The catalog contains no entries from failed or unauthenticated
/// providers.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    specs: BTreeSet<String>,
}

impl ModelCatalog {
    /// Build a catalog from already authenticated provider output.
    ///
    /// This constructor is crate-private so untrusted model IDs cannot create
    /// an authorization catalog outside the discovery boundary.
    #[must_use]
    pub(crate) fn from_specs<I, S>(specs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            specs: specs
                .into_iter()
                .map(Into::into)
                .filter(|spec| is_model_spec(spec))
                .collect(),
        }
    }

    /// Resolve a user supplied model only when the exact spec is in this
    /// authenticated catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::InvalidFormat`] for malformed input,
    /// [`ModelError::Unavailable`] for a model absent from this snapshot, or
    /// the underlying model parsing error for a corrupted catalog entry.
    pub fn resolve(&self, spec: &str) -> Result<Model, ModelError> {
        if !is_model_spec(spec) {
            return Err(ModelError::InvalidFormat);
        }
        if !self.specs.contains(spec) {
            return Err(ModelError::Unavailable(spec.to_string()));
        }
        Model::from_spec(spec)
    }

    #[must_use]
    pub fn contains(&self, spec: &str) -> bool {
        self.specs.contains(spec)
    }

    /// Return the presentation-safe model list. It never includes rejected or
    /// unavailable entries because those are never inserted into the catalog.
    #[must_use]
    pub fn specs(&self) -> Vec<String> {
        self.specs.iter().cloned().collect()
    }
}

/// A typed boundary for every externally supplied model selection.
#[derive(Debug, Clone)]
pub struct ModelResolver {
    catalog: ModelCatalog,
}

impl ModelResolver {
    #[must_use]
    pub fn new(catalog: ModelCatalog) -> Self {
        Self { catalog }
    }

    /// Build a resolver from a fresh authenticated discovery pass.
    #[must_use]
    pub fn current() -> Self {
        Self::new(discover_model_catalog_sync())
    }

    /// Build a resolver from a fresh authenticated discovery pass.
    pub async fn current_async() -> Self {
        Self::new(discover_model_catalog().await)
    }

    /// Resolve a model against this authenticated snapshot.
    ///
    /// # Errors
    ///
    /// Returns a fail-closed [`ModelError`] when the spec is malformed or is
    /// absent from the current catalog.
    pub fn resolve(&self, spec: &str) -> Result<Model, ModelError> {
        self.catalog.resolve(spec)
    }

    /// Replace the snapshot after a login, logout, key rotation, or auth
    /// refresh. A resolver intentionally never retains entries from the old
    /// snapshot: callers must revalidate before accepting another selection.
    pub fn revalidate(&mut self, catalog: ModelCatalog) {
        self.catalog = catalog;
    }

    /// Refresh the snapshot after an authentication transition.
    pub async fn refresh(&mut self) {
        self.revalidate(discover_model_catalog().await);
    }

    /// Synchronous counterpart to [`Self::refresh`].
    pub fn refresh_sync(&mut self) {
        self.revalidate(discover_model_catalog_sync());
    }

    #[must_use]
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }
}

fn is_model_spec(spec: &str) -> bool {
    spec.split_once('/')
        .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
}

fn add_provider_models(
    catalog: &mut BTreeSet<String>,
    slug: &str,
    models: Vec<ModelInfo>,
    static_manifest: Option<&crate::manifest::ProviderManifest>,
) {
    for model in models {
        if !model.id.is_empty() {
            catalog.insert(format!("{slug}/{}", model.id));
        }
    }
    if let Some(manifest) = static_manifest {
        for entry in manifest.models {
            for prefix in entry.prefixes {
                catalog.insert(format!("{slug}/{prefix}"));
            }
        }
    }
}

async fn discover_provider(
    catalog: &mut BTreeSet<String>,
    slug: &str,
    static_manifest: Option<&crate::manifest::ProviderManifest>,
    timeouts: Timeouts,
) {
    let is_dynamic = dynamic::display_name(slug).is_some();
    let Ok(provider) = provider_for_slug(slug, timeouts) else {
        return;
    };
    // Dynamic providers can expose static model metadata even after logout.
    // Their reload hook re-resolves the script credentials before listing, so
    // a failed auth refresh contributes no models.
    if is_dynamic && provider.reload_auth().await.is_err() {
        return;
    }
    let Ok(models) = provider.list_models().await else {
        return;
    };
    add_provider_models(catalog, slug, models, static_manifest);
}

/// Discover models from providers that can currently be constructed and list
/// models with their current authentication. A failed provider contributes no
/// static fallback, preventing stale auth and cached metadata from authorizing
/// a selection.
pub async fn discover_model_catalog() -> ModelCatalog {
    discover_model_catalog_with(Timeouts::default()).await
}

/// Discover a new snapshot using the supplied provider timeouts. This is
/// useful to integrations that already have a timeout policy and keeps the
/// refresh boundary explicit.
pub async fn discover_model_catalog_with(timeouts: Timeouts) -> ModelCatalog {
    let mut specs = BTreeSet::new();

    for manifest in ManifestRegistry::builtins() {
        discover_provider(&mut specs, manifest.slug, Some(manifest), timeouts).await;
    }

    for slug in dynamic::discovered_slugs() {
        discover_provider(&mut specs, slug, None, timeouts).await;
    }

    // A configured custom model is usable when the provider can be
    // authenticated, even when model listing is disabled or unavailable.
    // Discovery output is still required for models that were not configured.
    let declared = custom::declared_model_specs();
    for spec in declared {
        let Some((slug, _)) = spec.split_once('/') else {
            continue;
        };
        if provider_for_slug(slug, timeouts).is_ok() {
            specs.insert(spec);
        }
    }
    let discovered = smol::unblock(move || custom::discover_models(timeouts)).await;
    specs.extend(discovered.into_iter().filter(|spec| is_model_spec(spec)));

    ModelCatalog::from_specs(specs)
}

/// Synchronous boundary used by CLI, TUI, ACP, SDK, and Lua APIs.
#[must_use]
pub fn discover_model_catalog_sync() -> ModelCatalog {
    smol::block_on(discover_model_catalog())
}

/// Resolve an externally supplied model against a fresh authenticated catalog.
///
/// # Errors
///
/// Returns a fail-closed [`ModelError`] when the model is malformed or not
/// present in the current authenticated snapshot.
pub fn resolve_configured_model(spec: &str) -> Result<Model, ModelError> {
    ModelResolver::new(discover_model_catalog_sync()).resolve(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_custom_model_is_resolvable_when_authenticated() {
        let resolver =
            ModelResolver::new(ModelCatalog::from_specs(["openai/custom-configured-model"]));

        let model = resolver
            .resolve("openai/custom-configured-model")
            .expect("an authenticated configured model resolves");
        assert_eq!(model.spec(), "openai/custom-configured-model");
    }

    #[test]
    fn resolver_accepts_only_exact_authenticated_specs() {
        let resolver = ModelResolver::new(ModelCatalog::from_specs([
            "anthropic/claude-sonnet-4-20250514",
            "openai/gpt-5.6",
        ]));

        assert!(
            resolver
                .resolve("anthropic/claude-sonnet-4-20250514")
                .is_ok()
        );
        let error = resolver
            .resolve("anthropic/claude-sonnet-4-20250514-preview")
            .expect_err("unlisted model must fail closed");
        assert_eq!(
            error.to_string(),
            "model 'anthropic/claude-sonnet-4-20250514-preview' is not available from a configured, authenticated provider"
        );
    }

    #[test]
    fn arbitrary_ids_and_logged_out_providers_are_rejected() {
        let logged_out = ModelResolver::new(ModelCatalog::default());

        let arbitrary = logged_out
            .resolve("openai/model-the-user-invented")
            .expect_err("arbitrary IDs must not be accepted");
        assert!(matches!(arbitrary, ModelError::Unavailable(_)));
        let logged_out_error = logged_out
            .resolve("anthropic/claude-sonnet-4-20250514")
            .expect_err("a provider absent from the current snapshot is logged out");
        assert!(matches!(logged_out_error, ModelError::Unavailable(_)));
    }

    #[test]
    fn aliases_are_explicit_and_resolvable() {
        let resolver = ModelResolver::new(ModelCatalog::from_specs([
            "openai/gpt-5.6",
            "openai/gpt-5.6-luna",
        ]));

        assert!(resolver.resolve("openai/gpt-5.6-luna").is_ok());
        assert!(resolver.resolve("openai/gpt-5.6-preview").is_err());
    }

    #[test]
    fn revalidation_removes_stale_authenticated_models() {
        let mut resolver = ModelResolver::new(ModelCatalog::from_specs(["openai/gpt-5.6"]));
        assert!(resolver.resolve("openai/gpt-5.6").is_ok());

        resolver.revalidate(ModelCatalog::default());

        assert!(matches!(
            resolver.resolve("openai/gpt-5.6"),
            Err(ModelError::Unavailable(_))
        ));
        assert!(resolver.catalog().specs().is_empty());
    }

    #[test]
    fn catalog_preserves_explicit_aliases_without_exposing_other_ids() {
        let catalog = ModelCatalog::from_specs([
            "openai/gpt-5.6",
            "openai/gpt-5.6-sol",
            "custom/provider-model",
        ]);

        assert!(catalog.contains("openai/gpt-5.6-sol"));
        assert!(!catalog.contains("openai/gpt-6"));
        assert_eq!(catalog.specs().len(), 3);
    }

    #[test]
    fn unavailable_error_does_not_expose_catalog_contents() {
        let error = ModelCatalog::from_specs(["anthropic/visible-only"])
            .resolve("anthropic/hidden-model")
            .expect_err("hidden model must not resolve");
        assert!(error.to_string().contains("anthropic/hidden-model"));
        assert!(!error.to_string().contains("visible-only"));
        assert!(
            !error
                .to_string()
                .contains("configured, authenticated provider:")
        );
    }
}

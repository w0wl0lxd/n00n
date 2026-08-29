use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::manifest::ManifestRegistry;
use crate::model::Model;
use crate::model_registry::ModelRegistry;
use crate::provider::{available_model_specs, provider_available};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelCatalogError {
    #[error("model must be a configured provider/model spec")]
    InvalidSpec,
    #[error("model '{0}' is not configured or available")]
    Unavailable(String),
    #[error("configured model metadata is invalid")]
    InvalidModel,
}

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    specs: Arc<[String]>,
    aliases: Arc<HashMap<String, String>>,
    registry: Arc<RwLock<ModelRegistry>>,
}

impl ModelCatalog {
    #[must_use]
    pub fn current() -> Self {
        Self::from_specs(available_model_specs())
    }

    #[must_use]
    pub fn current_with_specs(specs: impl IntoIterator<Item = String>) -> Self {
        Self::from_specs(available_model_specs().into_iter().chain(specs))
    }

    #[must_use]
    pub fn from_specs(specs: impl IntoIterator<Item = String>) -> Self {
        let mut unique: Vec<String> = specs.into_iter().filter(|s| is_spec(s)).collect();
        unique.sort();
        unique.dedup();
        Self {
            specs: unique.into(),
            aliases: Arc::new(HashMap::new()),
            registry: Arc::new(RwLock::new(ModelRegistry::default())),
        }
    }

    /// Add an alias for a model already present in this catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when `spec` is malformed or is not present in the catalog.
    pub fn with_alias(
        mut self,
        alias: impl Into<String>,
        spec: impl Into<String>,
    ) -> Result<Self, ModelCatalogError> {
        let alias = alias.into();
        let spec = spec.into();
        if !is_spec(&spec) {
            return Err(ModelCatalogError::InvalidSpec);
        }
        if !self.contains(&spec) {
            return Err(ModelCatalogError::Unavailable(spec));
        }
        Arc::make_mut(&mut self.aliases).insert(alias, spec);
        Ok(self)
    }

    #[must_use]
    pub fn specs(&self) -> &[String] {
        &self.specs
    }

    #[must_use]
    pub fn contains(&self, spec: &str) -> bool {
        self.specs.iter().any(|candidate| candidate == spec)
    }

    /// Resolve an externally supplied model identifier without allowing the
    /// permissive model parser to create an unconfigured model.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is malformed, unavailable, or has invalid metadata.
    pub fn resolve(&self, input: &str) -> Result<Model, ModelCatalogError> {
        let spec = self.canonical_spec(input)?;
        let (provider, _) = spec.split_once('/').ok_or(ModelCatalogError::InvalidSpec)?;
        if !provider_available(provider) {
            return Err(ModelCatalogError::Unavailable(spec));
        }
        Model::from_spec(&self.registry, &spec).map_err(|error| {
            tracing::warn!(error = %error, "configured model metadata failed validation");
            ModelCatalogError::InvalidModel
        })
    }

    fn canonical_spec(&self, input: &str) -> Result<String, ModelCatalogError> {
        if let Some(spec) = self.aliases.get(input) {
            return Ok(spec.clone());
        }
        if self
            .specs
            .iter()
            .any(|candidate| is_compatible_spec(input, candidate))
            || is_arbitrary_model_provider_spec(input)
        {
            return Ok(input.to_string());
        }
        // This is the only built-in shorthand. It is deliberately an explicit
        // alias family, not a fallback to arbitrary provider/model parsing.
        if input.starts_with("claude-") {
            let spec = format!("anthropic/{input}");
            if self
                .specs
                .iter()
                .any(|candidate| is_compatible_spec(&spec, candidate))
            {
                return Ok(spec);
            }
        }
        Err(ModelCatalogError::Unavailable(input.to_string()))
    }

    #[must_use]
    pub fn allows(&self, input: &str) -> bool {
        self.resolve(input).is_ok()
    }

    /// Whether the spec resolves in the catalog or is a model the provider
    /// accepts through live discovery even though it is not in the static
    /// tables (`OpenRouter`, `Ollama`, and other flexible providers).
    #[must_use]
    pub fn allows_live(&self, input: &str) -> bool {
        self.allows(input) || is_discoverable_spec(input)
    }
}

#[derive(Debug, Clone)]
pub struct ModelResolver {
    catalog: ModelCatalog,
}

impl ModelResolver {
    #[must_use]
    pub fn current() -> Self {
        Self {
            catalog: ModelCatalog::current(),
        }
    }

    #[must_use]
    pub fn from_catalog(catalog: ModelCatalog) -> Self {
        Self { catalog }
    }

    #[must_use]
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    /// Resolve a model through the configured catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is malformed, unavailable, or has invalid metadata.
    pub fn resolve(&self, input: &str) -> Result<Model, ModelCatalogError> {
        self.catalog.resolve(input)
    }

    /// Re-read the local catalog after login/logout or provider configuration
    /// changes. Resolution itself never performs network discovery.
    pub fn refresh(&mut self) {
        self.catalog = ModelCatalog::current();
    }
}

fn is_spec(spec: &str) -> bool {
    let Some((provider, model)) = spec.split_once('/') else {
        return false;
    };
    !provider.trim().is_empty()
        && !model.trim().is_empty()
        && !model.split('/').any(|segment| segment.trim().is_empty())
}

fn is_compatible_spec(input: &str, catalogued: &str) -> bool {
    input == catalogued
        || input.strip_prefix(catalogued).is_some_and(|suffix| {
            suffix.len() == 9
                && suffix.starts_with('-')
                && suffix[1..].bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_arbitrary_model_provider_spec(spec: &str) -> bool {
    if !is_spec(spec) {
        return false;
    }
    let Some((provider, _)) = spec.split_once('/') else {
        return false;
    };
    ManifestRegistry::for_slug(provider).is_some_and(|manifest| manifest.accepts_arbitrary_models)
}

fn is_discoverable_spec(spec: &str) -> bool {
    if !is_spec(spec) {
        return false;
    }
    let Some((provider, _)) = spec.split_once('/') else {
        return false;
    };
    provider_available(provider)
        && ManifestRegistry::for_slug(provider)
            .is_some_and(|manifest| manifest.accepts_arbitrary_models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_specs_are_resolved_only_when_catalogued() {
        let catalog = ModelCatalog::from_specs(["test/custom".to_string()]);
        assert!(matches!(
            catalog.resolve("test/unknown"),
            Err(ModelCatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn aliases_are_explicit_and_target_catalogued_models() {
        let catalog = ModelCatalog::from_specs(["test/canonical".to_string()])
            .with_alias("friendly", "test/canonical")
            .unwrap();
        assert_eq!(
            catalog.aliases.get("friendly"),
            Some(&"test/canonical".to_string())
        );
        assert!(matches!(
            catalog.canonical_spec("unknown"),
            Err(ModelCatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn aliases_reject_invalid_or_uncataloged_specs() {
        let catalog = ModelCatalog::from_specs(["test/canonical".to_string()]);
        assert!(matches!(
            catalog.clone().with_alias("malformed", "not-a-model-spec"),
            Err(ModelCatalogError::InvalidSpec)
        ));
        assert!(matches!(
            catalog.with_alias("missing", "test/missing"),
            Err(ModelCatalogError::Unavailable(spec)) if spec == "test/missing"
        ));
    }

    #[test]
    fn versioned_specs_preserve_catalog_prefix_compatibility() {
        let catalog = ModelCatalog::from_specs(["anthropic/claude-opus-4-5".to_string()]);
        assert_eq!(
            catalog
                .canonical_spec("anthropic/claude-opus-4-5-20251101")
                .unwrap(),
            "anthropic/claude-opus-4-5-20251101"
        );
        assert_eq!(
            catalog.canonical_spec("claude-opus-4-5-20251101").unwrap(),
            "anthropic/claude-opus-4-5-20251101"
        );
        assert!(matches!(
            catalog.canonical_spec("openai/claude-opus-4-5-20251101"),
            Err(ModelCatalogError::Unavailable(_))
        ));
        assert!(matches!(
            catalog.canonical_spec("claude-opus-4-5o"),
            Err(ModelCatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn compatible_suffix_rejects_non_date_versions() {
        let catalog = ModelCatalog::from_specs(["openai/gpt-4".to_string()]);

        for spec in [
            "openai/gpt-4-0613",
            "openai/gpt-4-2025010",
            "openai/gpt-4-202501011",
            "openai/gpt-4-2025abcd",
            "openai/gpt-4-20250101-preview",
        ] {
            assert!(
                matches!(
                    catalog.canonical_spec(spec),
                    Err(ModelCatalogError::Unavailable(_))
                ),
                "unexpectedly accepted {spec}"
            );
        }
    }

    #[test]
    fn prefix_compatibility_requires_version_boundary() {
        let catalog = ModelCatalog::from_specs(["openai/gpt-4".to_string()]);

        assert_eq!(
            catalog.canonical_spec("openai/gpt-4-20250101").unwrap(),
            "openai/gpt-4-20250101"
        );
        assert!(matches!(
            catalog.canonical_spec("openai/gpt-4o"),
            Err(ModelCatalogError::Unavailable(_))
        ));
        assert!(matches!(
            catalog.canonical_spec("openai/gpt-4.1"),
            Err(ModelCatalogError::Unavailable(_))
        ));
        assert!(matches!(
            catalog.canonical_spec("openai/gpt-4/anything"),
            Err(ModelCatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn empty_static_manifest_accepts_live_discovered_model_specs() {
        let catalog = ModelCatalog::from_specs([]);

        assert!(is_arbitrary_model_provider_spec(
            "opencode/opencode/big-pickle"
        ));
        assert!(!is_arbitrary_model_provider_spec(
            "openai/not-in-static-catalog"
        ));
        assert!(!is_arbitrary_model_provider_spec("opencode/"));
        assert_eq!(
            catalog
                .canonical_spec("opencode/opencode/big-pickle")
                .unwrap(),
            "opencode/opencode/big-pickle"
        );
        assert!(matches!(
            catalog.canonical_spec("openai/not-in-static-catalog"),
            Err(ModelCatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn malformed_specs_never_enter_catalog() {
        let catalog = ModelCatalog::from_specs([
            "no-provider".to_string(),
            "provider/".to_string(),
            "/model".to_string(),
            "provider//model".to_string(),
            "provider/model/".to_string(),
            "provider/model".to_string(),
        ]);
        assert_eq!(catalog.specs(), &["provider/model".to_string()]);
    }

    #[test]
    fn nested_model_ids_enter_catalog() {
        let catalog = ModelCatalog::from_specs([
            "openrouter/vendor/model".to_string(),
            "opencode/vendor/model".to_string(),
        ]);
        assert_eq!(
            catalog.specs(),
            &[
                "opencode/vendor/model".to_string(),
                "openrouter/vendor/model".to_string(),
            ]
        );
    }
}

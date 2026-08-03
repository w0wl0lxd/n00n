use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Model;
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
        }
    }

    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>, spec: impl Into<String>) -> Self {
        let alias = alias.into();
        let spec = spec.into();
        if is_spec(&spec) && self.specs.iter().any(|candidate| candidate == &spec) {
            Arc::make_mut(&mut self.aliases).insert(alias, spec);
        }
        self
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
        Model::from_spec(&spec).map_err(|error| {
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
            .any(|candidate| input.starts_with(candidate))
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
                .any(|candidate| spec.starts_with(candidate))
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
            .with_alias("friendly", "test/canonical");
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

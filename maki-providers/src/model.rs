use std::fmt;
use std::ops::AddAssign;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::provider::ProviderKind;
use crate::providers::{anthropic, zai};

const PER_MILLION: f64 = 1_000_000.0;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model must be in 'provider/model' format (e.g. anthropic/claude-sonnet-4-20250514)")]
    InvalidFormat,
    #[error("unsupported provider '{0}'")]
    UnsupportedProvider(String),
    #[error("unknown model '{0}'")]
    UnknownModel(String),
    #[error("invalid model tier '{0}' (expected: strong, medium, weak)")]
    InvalidTier(String),
    #[error("no default model for {0}/{1}")]
    NoDefault(ProviderKind, ModelTier),
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Claude,
    Glm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelTier {
    Weak,
    Medium,
    Strong,
}

impl fmt::Display for ModelTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Weak => "weak",
            Self::Medium => "medium",
            Self::Strong => "strong",
        })
    }
}

impl FromStr for ModelTier {
    type Err = ModelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "weak" => Ok(Self::Weak),
            "medium" => Ok(Self::Medium),
            "strong" => Ok(Self::Strong),
            other => Err(ModelError::InvalidTier(other.to_string())),
        }
    }
}

pub(crate) struct ModelEntry {
    pub(crate) prefixes: &'static [&'static str],
    pub(crate) tier: ModelTier,
    pub(crate) family: ModelFamily,
    pub(crate) default: bool,
    pub(crate) pricing: ModelPricing,
    pub(crate) max_output_tokens: u32,
    pub(crate) context_window: u32,
}

fn lookup_entry<'a>(
    entries: &'a [ModelEntry],
    model_id: &str,
) -> Result<&'a ModelEntry, ModelError> {
    entries
        .iter()
        .find(|e| e.prefixes.iter().any(|p| model_id.starts_with(p)))
        .ok_or_else(|| ModelError::UnknownModel(model_id.to_string()))
}

pub(crate) fn models_for_provider(provider: ProviderKind) -> &'static [ModelEntry] {
    match provider {
        ProviderKind::Anthropic => anthropic::models(),
        ProviderKind::Zai | ProviderKind::ZaiCodingPlan => zai::models(),
    }
}

impl ModelFamily {
    pub fn supports_tool_examples(self) -> bool {
        match self {
            ModelFamily::Claude => true,
            ModelFamily::Glm => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub provider: ProviderKind,
    pub tier: ModelTier,
    pub family: ModelFamily,
    pub pricing: ModelPricing,
    pub max_output_tokens: u32,
    pub context_window: u32,
}

impl Model {
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn from_tier(provider: ProviderKind, tier: ModelTier) -> Result<Self, ModelError> {
        let entries = models_for_provider(provider);
        let entry = entries
            .iter()
            .find(|e| e.default && e.tier == tier)
            .ok_or(ModelError::NoDefault(provider, tier))?;
        let model_id = entry.prefixes[0];
        Self::from_spec(&format!("{provider}/{model_id}"))
    }

    pub fn from_spec(spec: &str) -> Result<Self, ModelError> {
        let (provider_str, model_id) = spec.split_once('/').ok_or(ModelError::InvalidFormat)?;
        let provider = ProviderKind::from_str(provider_str)
            .map_err(|_| ModelError::UnsupportedProvider(provider_str.to_string()))?;
        let entries = models_for_provider(provider);
        let entry = lookup_entry(entries, model_id)?;
        Ok(Self {
            id: model_id.to_string(),
            provider,
            tier: entry.tier,
            family: entry.family,
            pricing: entry.pricing.clone(),
            max_output_tokens: entry.max_output_tokens,
            context_window: entry.context_window,
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Non-cached input tokens. Total input = `input + cache_read + cache_creation`.
    #[serde(rename = "input_tokens")]
    pub input: u32,
    #[serde(rename = "output_tokens")]
    pub output: u32,
    #[serde(rename = "cache_creation_input_tokens")]
    pub cache_creation: u32,
    #[serde(rename = "cache_read_input_tokens")]
    pub cache_read: u32,
}

impl TokenUsage {
    pub fn total_input(&self) -> u32 {
        self.input + self.cache_read + self.cache_creation
    }

    pub fn context_tokens(&self) -> u32 {
        self.input + self.output + self.cache_creation + self.cache_read
    }

    pub fn cost(&self, pricing: &ModelPricing) -> f64 {
        self.input as f64 * pricing.input / PER_MILLION
            + self.output as f64 * pricing.output / PER_MILLION
            + self.cache_creation as f64 * pricing.cache_write / PER_MILLION
            + self.cache_read as f64 * pricing.cache_read / PER_MILLION
    }
}

impl AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input += rhs.input;
        self.output += rhs.output;
        self.cache_creation += rhs.cache_creation;
        self.cache_read += rhs.cache_read;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("anthropic/claude-3-5-haiku-20241022", 8192, 200_000 ; "anthropic_tier")]
    #[test_case("anthropic/claude-opus-4-6-20260101", 128000, 200_000 ; "anthropic_high_output_tier")]
    #[test_case("zai/glm-5", 131072, 200_000 ; "zai_200k_context")]
    #[test_case("zai/glm-4.5", 98304, 131_072 ; "zai_131k_context")]
    #[test_case("zai-coding-plan/glm-4.7", 131072, 200_000 ; "zai_coding_plan_alias")]
    fn from_spec_resolves_tier(spec: &str, expected_max: u32, expected_ctx: u32) {
        let model = Model::from_spec(spec).unwrap();
        assert_eq!(model.max_output_tokens, expected_max);
        assert_eq!(model.context_window, expected_ctx);
    }

    #[test]
    fn zai_free_tier_has_zero_pricing() {
        let model = Model::from_spec("zai/glm-4.7-flash").unwrap();
        assert_eq!(model.pricing.input, 0.0);
        assert_eq!(model.pricing.output, 0.0);
    }

    #[test_case("no-slash-here", ModelError::InvalidFormat ; "invalid_format")]
    #[test_case("openai/gpt-4", ModelError::UnsupportedProvider("openai".into()) ; "unsupported_provider")]
    #[test_case("anthropic/claude-99-turbo", ModelError::UnknownModel("claude-99-turbo".into()) ; "unknown_anthropic_model")]
    #[test_case("zai/glm-99", ModelError::UnknownModel("glm-99".into()) ; "unknown_zai_model")]
    fn from_spec_errors(spec: &str, expected: ModelError) {
        let err = Model::from_spec(spec).unwrap_err();
        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&expected)
        );
    }

    #[test]
    fn total_input_includes_cached_tokens() {
        let usage = TokenUsage {
            input: 5_000,
            output: 1_000,
            cache_creation: 10_000,
            cache_read: 150_000,
        };
        assert_eq!(usage.total_input(), 165_000);
    }

    #[test]
    fn cost_computes_all_token_types() {
        let pricing = ModelPricing {
            input: 3.00,
            output: 15.00,
            cache_write: 3.75,
            cache_read: 0.30,
        };
        let usage = TokenUsage {
            input: 1_000_000,
            output: 100_000,
            cache_creation: 200_000,
            cache_read: 500_000,
        };
        let cost = usage.cost(&pricing);
        let expected = 3.0 + 1.5 + 0.75 + 0.15;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn spec_roundtrips_through_from_spec() {
        let model = Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap();
        let spec = model.spec();
        let round = Model::from_spec(&spec).unwrap();
        assert_eq!(round.id, model.id);
        assert_eq!(round.max_output_tokens, model.max_output_tokens);
    }

    #[test_case("anthropic/claude-opus-4-6-20260101",    ModelTier::Strong ; "anthropic_opus_strong")]
    #[test_case("anthropic/claude-3-opus-20240229",      ModelTier::Strong ; "anthropic_opus3_strong")]
    #[test_case("anthropic/claude-sonnet-4-20250514",    ModelTier::Medium ; "anthropic_sonnet_medium")]
    #[test_case("anthropic/claude-3-7-sonnet-20250219",  ModelTier::Medium ; "anthropic_37sonnet_medium")]
    #[test_case("anthropic/claude-3-5-haiku-20241022",   ModelTier::Weak   ; "anthropic_35haiku_weak")]
    #[test_case("anthropic/claude-haiku-4-5-20250506",   ModelTier::Weak   ; "anthropic_haiku45_weak")]
    #[test_case("zai/glm-5-code",                       ModelTier::Strong ; "zai_glm5code_strong")]
    #[test_case("zai/glm-5",                            ModelTier::Strong ; "zai_glm5_strong")]
    #[test_case("zai/glm-4.7",                          ModelTier::Medium ; "zai_glm47_medium")]
    #[test_case("zai/glm-4.5",                          ModelTier::Medium ; "zai_glm45_medium")]
    #[test_case("zai/glm-4.7-flash",                    ModelTier::Weak   ; "zai_glm47flash_weak")]
    #[test_case("zai/glm-4.5-flash",                    ModelTier::Weak   ; "zai_glm45flash_weak")]
    #[test_case("zai/glm-4.5-air",                      ModelTier::Weak   ; "zai_glm45air_weak")]
    fn model_tier_classification(spec: &str, expected: ModelTier) {
        let model = Model::from_spec(spec).unwrap();
        assert_eq!(model.tier, expected);
    }

    #[test_case(ProviderKind::Anthropic,     ModelTier::Strong ; "anthropic_strong")]
    #[test_case(ProviderKind::Anthropic,     ModelTier::Medium ; "anthropic_medium")]
    #[test_case(ProviderKind::Anthropic,     ModelTier::Weak   ; "anthropic_weak")]
    #[test_case(ProviderKind::Zai,           ModelTier::Strong ; "zai_strong")]
    #[test_case(ProviderKind::Zai,           ModelTier::Medium ; "zai_medium")]
    #[test_case(ProviderKind::Zai,           ModelTier::Weak   ; "zai_weak")]
    #[test_case(ProviderKind::ZaiCodingPlan, ModelTier::Strong ; "zai_coding_plan_strong")]
    fn from_tier_produces_valid_model(provider: ProviderKind, tier: ModelTier) {
        let model = Model::from_tier(provider, tier).unwrap();
        assert_eq!(model.provider, provider);
        assert_eq!(model.tier, tier);
    }

    #[test_case("strong", ModelTier::Strong ; "parse_strong")]
    #[test_case("medium", ModelTier::Medium ; "parse_medium")]
    #[test_case("weak",   ModelTier::Weak   ; "parse_weak")]
    fn tier_parse_valid(input: &str, expected: ModelTier) {
        assert_eq!(input.parse::<ModelTier>().unwrap(), expected);
    }

    #[test]
    fn tier_parse_invalid() {
        assert!(matches!(
            "turbo".parse::<ModelTier>(),
            Err(ModelError::InvalidTier(_))
        ));
    }

    #[test]
    fn exactly_one_default_per_provider_tier() {
        for (provider, entries) in [
            (
                ProviderKind::Anthropic,
                models_for_provider(ProviderKind::Anthropic),
            ),
            (ProviderKind::Zai, models_for_provider(ProviderKind::Zai)),
        ] {
            for tier in [ModelTier::Weak, ModelTier::Medium, ModelTier::Strong] {
                let count = entries
                    .iter()
                    .filter(|e| e.tier == tier && e.default)
                    .count();
                assert_eq!(
                    count, 1,
                    "{provider}/{tier}: expected exactly 1 default, found {count}"
                );
            }
        }
    }
}

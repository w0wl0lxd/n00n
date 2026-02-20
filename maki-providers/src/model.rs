use std::ops::AddAssign;
use std::str::FromStr;

use serde::Serialize;

use crate::provider::ProviderKind;

const PER_MILLION: f64 = 1_000_000.0;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model must be in 'provider/model' format (e.g. anthropic/claude-sonnet-4-20250514)")]
    InvalidFormat,
    #[error("unsupported provider '{0}'")]
    UnsupportedProvider(String),
    #[error("unknown model '{0}'")]
    UnknownModel(String),
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

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub provider: ProviderKind,
    pub pricing: ModelPricing,
    pub max_output_tokens: u32,
    pub context_window: u32,
}

struct ModelTier {
    prefixes: &'static [&'static str],
    pricing: ModelPricing,
    max_output_tokens: u32,
    context_window: u32,
}

const ANTHROPIC_TIERS: &[ModelTier] = &[
    ModelTier {
        prefixes: &["claude-3-haiku"],
        pricing: ModelPricing {
            input: 0.25,
            output: 1.25,
            cache_write: 0.30,
            cache_read: 0.03,
        },
        max_output_tokens: 4096,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-3-5-haiku"],
        pricing: ModelPricing {
            input: 0.80,
            output: 4.00,
            cache_write: 1.00,
            cache_read: 0.08,
        },
        max_output_tokens: 8192,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-haiku-4-5"],
        pricing: ModelPricing {
            input: 1.00,
            output: 5.00,
            cache_write: 1.25,
            cache_read: 0.10,
        },
        max_output_tokens: 64000,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-3-sonnet"],
        pricing: ModelPricing {
            input: 3.00,
            output: 15.00,
            cache_write: 0.30,
            cache_read: 0.30,
        },
        max_output_tokens: 4096,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-3-5-sonnet"],
        pricing: ModelPricing {
            input: 3.00,
            output: 15.00,
            cache_write: 3.75,
            cache_read: 0.30,
        },
        max_output_tokens: 8192,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-3-7-sonnet", "claude-sonnet-4"],
        pricing: ModelPricing {
            input: 3.00,
            output: 15.00,
            cache_write: 3.75,
            cache_read: 0.30,
        },
        max_output_tokens: 64000,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-sonnet-4-5", "claude-sonnet-4-6"],
        pricing: ModelPricing {
            input: 3.00,
            output: 15.00,
            cache_write: 3.75,
            cache_read: 0.30,
        },
        max_output_tokens: 64000,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-opus-4-5"],
        pricing: ModelPricing {
            input: 5.00,
            output: 25.00,
            cache_write: 6.25,
            cache_read: 0.50,
        },
        max_output_tokens: 64000,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-opus-4-6"],
        pricing: ModelPricing {
            input: 5.00,
            output: 25.00,
            cache_write: 6.25,
            cache_read: 0.50,
        },
        max_output_tokens: 128000,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["claude-3-opus", "claude-opus-4-0", "claude-opus-4-1"],
        pricing: ModelPricing {
            input: 15.00,
            output: 75.00,
            cache_write: 18.75,
            cache_read: 1.50,
        },
        max_output_tokens: 32000,
        context_window: 200_000,
    },
];

const ZAI_TIERS: &[ModelTier] = &[
    ModelTier {
        prefixes: &["glm-5-code"],
        pricing: ModelPricing {
            input: 1.20,
            output: 5.00,
            cache_write: 0.00,
            cache_read: 0.30,
        },
        max_output_tokens: 131072,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["glm-5"],
        pricing: ModelPricing {
            input: 1.00,
            output: 3.20,
            cache_write: 0.00,
            cache_read: 0.20,
        },
        max_output_tokens: 131072,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["glm-4.7-flash"],
        pricing: ModelPricing {
            input: 0.00,
            output: 0.00,
            cache_write: 0.00,
            cache_read: 0.00,
        },
        max_output_tokens: 131072,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["glm-4.7", "glm-4.6"],
        pricing: ModelPricing {
            input: 0.60,
            output: 2.20,
            cache_write: 0.00,
            cache_read: 0.11,
        },
        max_output_tokens: 131072,
        context_window: 200_000,
    },
    ModelTier {
        prefixes: &["glm-4.5-flash"],
        pricing: ModelPricing {
            input: 0.00,
            output: 0.00,
            cache_write: 0.00,
            cache_read: 0.00,
        },
        max_output_tokens: 98304,
        context_window: 131_072,
    },
    ModelTier {
        prefixes: &["glm-4.5-air"],
        pricing: ModelPricing {
            input: 0.20,
            output: 1.10,
            cache_write: 0.00,
            cache_read: 0.03,
        },
        max_output_tokens: 98304,
        context_window: 131_072,
    },
    ModelTier {
        prefixes: &["glm-4.5"],
        pricing: ModelPricing {
            input: 0.60,
            output: 2.20,
            cache_write: 0.00,
            cache_read: 0.11,
        },
        max_output_tokens: 98304,
        context_window: 131_072,
    },
];

fn lookup_tier<'a>(tiers: &'a [ModelTier], model_id: &str) -> Result<&'a ModelTier, ModelError> {
    tiers
        .iter()
        .find(|t| t.prefixes.iter().any(|p| model_id.starts_with(p)))
        .ok_or_else(|| ModelError::UnknownModel(model_id.to_string()))
}

impl Model {
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn family(&self) -> ModelFamily {
        match self.provider {
            ProviderKind::Zai | ProviderKind::ZaiCodingPlan => ModelFamily::Glm,
            ProviderKind::Anthropic => ModelFamily::Claude,
        }
    }

    pub fn from_spec(spec: &str) -> Result<Self, ModelError> {
        let (provider_str, model_id) = spec.split_once('/').ok_or(ModelError::InvalidFormat)?;
        let provider = ProviderKind::from_str(provider_str)
            .map_err(|_| ModelError::UnsupportedProvider(provider_str.to_string()))?;
        let tiers = match provider {
            ProviderKind::Anthropic => ANTHROPIC_TIERS,
            ProviderKind::Zai | ProviderKind::ZaiCodingPlan => ZAI_TIERS,
        };
        let tier = lookup_tier(tiers, model_id)?;
        Ok(Self {
            id: model_id.to_string(),
            provider,
            pricing: tier.pricing.clone(),
            max_output_tokens: tier.max_output_tokens,
            context_window: tier.context_window,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct TokenUsage {
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
}

use flume::Sender;
use serde_json::Value;

use crate::model::{Model, ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, StreamResponse};

use super::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    api_key_env: "OPENAI_API_KEY",
    base_url: "https://api.openai.com/v1",
    max_tokens_field: "max_completion_tokens",
    include_stream_usage: true,
    provider_name: "OpenAI",
};

pub(crate) fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["gpt-5.4-nano"],
            tier: ModelTier::Weak,
            family: ModelFamily::Gpt,
            default: true,
            pricing: ModelPricing {
                input: 0.20,
                output: 1.25,
                cache_write: 0.00,
                cache_read: 0.02,
            },
            max_output_tokens: 128_000,
            context_window: 400_000,
        },
        ModelEntry {
            prefixes: &["gpt-5.4-mini"],
            tier: ModelTier::Weak,
            family: ModelFamily::Gpt,
            default: false,
            pricing: ModelPricing {
                input: 0.75,
                output: 4.50,
                cache_write: 0.00,
                cache_read: 0.075,
            },
            max_output_tokens: 128_000,
            context_window: 400_000,
        },
        ModelEntry {
            prefixes: &["gpt-4.1-nano"],
            tier: ModelTier::Weak,
            family: ModelFamily::Gpt,
            default: false,
            pricing: ModelPricing {
                input: 0.10,
                output: 0.40,
                cache_write: 0.00,
                cache_read: 0.025,
            },
            max_output_tokens: 32_768,
            context_window: 1_047_576,
        },
        ModelEntry {
            prefixes: &["gpt-4.1-mini"],
            tier: ModelTier::Medium,
            family: ModelFamily::Gpt,
            default: false,
            pricing: ModelPricing {
                input: 0.40,
                output: 1.60,
                cache_write: 0.00,
                cache_read: 0.10,
            },
            max_output_tokens: 32_768,
            context_window: 1_047_576,
        },
        ModelEntry {
            prefixes: &["gpt-4.1"],
            tier: ModelTier::Medium,
            family: ModelFamily::Gpt,
            default: true,
            pricing: ModelPricing {
                input: 2.00,
                output: 8.00,
                cache_write: 0.00,
                cache_read: 0.50,
            },
            max_output_tokens: 32_768,
            context_window: 1_047_576,
        },
        ModelEntry {
            prefixes: &["o4-mini"],
            tier: ModelTier::Medium,
            family: ModelFamily::Gpt,
            default: false,
            pricing: ModelPricing {
                input: 1.10,
                output: 4.40,
                cache_write: 0.00,
                cache_read: 0.275,
            },
            max_output_tokens: 100_000,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gpt-5.4"],
            tier: ModelTier::Strong,
            family: ModelFamily::Gpt,
            default: true,
            pricing: ModelPricing {
                input: 2.50,
                output: 15.00,
                cache_write: 0.00,
                cache_read: 0.25,
            },
            max_output_tokens: 128_000,
            context_window: 1_050_000,
        },
        ModelEntry {
            prefixes: &["o3"],
            tier: ModelTier::Strong,
            family: ModelFamily::Gpt,
            default: false,
            pricing: ModelPricing {
                input: 2.00,
                output: 8.00,
                cache_write: 0.00,
                cache_read: 1.00,
            },
            max_output_tokens: 100_000,
            context_window: 200_000,
        },
    ]
}

pub struct OpenAi(OpenAiCompatProvider);

impl OpenAi {
    pub fn new() -> Result<Self, AgentError> {
        Ok(Self(OpenAiCompatProvider::new(&CONFIG)?))
    }
}

impl Provider for OpenAi {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let body = self.0.build_body(model, messages, system, tools);
            self.0.do_stream(model, &body, event_tx).await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(self.0.do_list_models())
    }
}

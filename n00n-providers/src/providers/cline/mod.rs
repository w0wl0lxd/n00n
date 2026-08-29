//! Cline: usage-billing credits and the ClinePass subscription behind one
//! OpenAI-compatible gateway (`api.cline.bot`).
//!
//! Auth supports an API key (`CLINE_API_KEY`) and the WorkOS device-flow
//! account sign-in (`n00n auth login cline`). Model IDs follow Cline's
//! `provider/model` convention (`cline/anthropic/claude-sonnet-4-6`,
//! `cline/cline-pass/glm-5.3`).

use std::sync::{Arc, Mutex};

use flume::Sender;
use n00n_config::providers::{BuiltInProvider, Protocol, ProviderPlan};
use n00n_storage::StateDir;
use n00n_storage::id::SessionRef;
use serde_json::Value;

use crate::model::{Model, ModelEntry, ModelFamily, ModelInfo, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::providers::openai_compat::OpenAiCompatProvider;
use crate::providers::{KeyPool, ResolvedAuth};
use crate::types::{ProviderUsage, UsageLimit};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, System, dialect};

pub mod auth;

const CREDITS_LABEL: &str = "Credits";
const X_TASK_ID_HEADER: &str = "x-task-id";
const FREE_MODEL_SUFFIX: &str = ":free";
const USERS_ME_PATH: &str = "/users/me";

inventory::submit!(BuiltInProvider {
    slug: "cline",
    display_name: "Cline",
    protocol: Protocol::Openai,
    default_base_url: "https://api.cline.bot/api/v1",
    default_api_key_env: "CLINE_API_KEY",
    default_model: "cline/anthropic/claude-sonnet-4-6",
    plans: Some(&[
        (
            "usage-billing",
            ProviderPlan {
                display_name: "Usage billing (pay-as-you-go credits)",
                base_url: "https://api.cline.bot/api/v1",
                default_model: Some("cline/anthropic/claude-sonnet-4-6"),
                login_url: Some("https://app.cline.bot"),
            },
        ),
        (
            "clinepass",
            ProviderPlan {
                display_name: "ClinePass subscription",
                base_url: "https://api.cline.bot/api/v1",
                default_model: Some("cline/cline-pass/glm-5.3"),
                login_url: Some("https://app.cline.bot/dashboard/subscription?personal=true"),
            },
        ),
    ]),
    login_url: Some("https://app.cline.bot"),
    needs_url: false,
});

include!(concat!(env!("OUT_DIR"), "/provider_configs/cline.rs"));

/// Static catalog used for tier defaults and offline model metadata. Pricing
/// is Cline's documented per-1M-token reference pricing; usage-billing models
/// use standard API rates, ClinePass models the ClinePass reference table.
/// Anything not listed here is still usable (the provider accepts arbitrary
/// models) and is discovered live via `GET /api/v1/models`.
pub(crate) const fn models() -> &'static [ModelEntry] {
    const MODELS: &[ModelEntry] = &[
        // Usage-billing defaults (standard API rates).
        entry(
            &["anthropic/claude-sonnet-4-6"],
            ModelTier::Strong,
            true,
            3.00,
            15.00,
            0.30,
            3.75,
            64_000,
            200_000,
        ),
        entry(
            &["google/gemini-2.5-pro"],
            ModelTier::Strong,
            false,
            1.25,
            10.00,
            0.31,
            0.0,
            65_536,
            1_000_000,
        ),
        entry(
            &["openai/gpt-4o"],
            ModelTier::Medium,
            false,
            2.50,
            10.00,
            1.25,
            0.0,
            16_384,
            128_000,
        ),
        entry(
            &["deepseek/deepseek-chat"],
            ModelTier::Medium,
            true,
            0.28,
            0.42,
            0.028,
            0.0,
            8_192,
            128_000,
        ),
        entry(
            &["minimax/minimax-m2.5"],
            ModelTier::Weak,
            true,
            0.30,
            1.20,
            0.06,
            0.0,
            32_000,
            200_000,
        ),
        // ClinePass (reference pricing; subscription quotas apply instead).
        entry(
            &["cline-pass/glm-5.3", "cline-pass/glm-5.2"],
            ModelTier::Strong,
            true,
            1.40,
            4.40,
            0.26,
            0.0,
            128_000,
            200_000,
        ),
        entry(
            &["cline-pass/kimi-k3"],
            ModelTier::Strong,
            false,
            3.00,
            15.00,
            0.30,
            0.0,
            32_000,
            256_000,
        ),
        entry(
            &["cline-pass/kimi-k2.7-code"],
            ModelTier::Medium,
            true,
            0.95,
            4.00,
            0.19,
            0.0,
            32_000,
            256_000,
        ),
        entry(
            &["cline-pass/kimi-k2.6"],
            ModelTier::Medium,
            false,
            0.95,
            4.00,
            0.16,
            0.0,
            32_000,
            256_000,
        ),
        entry(
            &["cline-pass/deepseek-v4-pro"],
            ModelTier::Medium,
            false,
            1.32,
            3.96,
            0.044,
            0.0,
            32_000,
            131_072,
        ),
        entry(
            &["cline-pass/deepseek-v4-flash"],
            ModelTier::Weak,
            false,
            0.44,
            1.32,
            0.014,
            0.0,
            32_000,
            131_072,
        ),
        entry(
            &["cline-pass/mimo-v2.5"],
            ModelTier::Weak,
            true,
            0.14,
            0.28,
            0.0028,
            0.0,
            8_192,
            131_072,
        ),
        entry(
            &["cline-pass/mimo-v2.5-pro"],
            ModelTier::Medium,
            false,
            1.74,
            3.48,
            0.0145,
            0.0,
            32_000,
            131_072,
        ),
        entry(
            &["cline-pass/minimax-m3"],
            ModelTier::Weak,
            false,
            0.30,
            1.20,
            0.06,
            0.0,
            32_000,
            200_000,
        ),
        entry(
            &["cline-pass/qwen3.8-max"],
            ModelTier::Strong,
            false,
            2.00,
            6.00,
            0.25,
            2.50,
            65_536,
            262_144,
        ),
        entry(
            &["cline-pass/qwen3.7-max"],
            ModelTier::Strong,
            false,
            2.50,
            7.50,
            0.50,
            3.125,
            65_536,
            262_144,
        ),
        entry(
            &["cline-pass/qwen3.7-plus"],
            ModelTier::Compaction,
            true,
            0.40,
            1.60,
            0.04,
            0.50,
            65_536,
            262_144,
        ),
    ];
    MODELS
}

#[allow(clippy::too_many_arguments)]
const fn entry(
    prefixes: &'static [&'static str],
    tier: ModelTier,
    default: bool,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    max_output_tokens: u32,
    context_window: u32,
) -> ModelEntry {
    ModelEntry {
        prefixes,
        tier,
        family: ModelFamily::Generic,
        vision: false,
        files: false,
        default,
        pricing: ModelPricing {
            input,
            output,
            cache_write,
            cache_read,
            fast: None,
        },
        max_output_tokens,
        context_window,
    }
}

/// Cline gateway provider: OpenAI-compatible chat completions with API-key or
/// OAuth account authentication, live model discovery, and credit usage.
pub struct Cline {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    key_pool: Option<KeyPool>,
    system_prefix: Option<String>,
    refresh_lock: Arc<async_lock::Mutex<()>>,
}

impl Cline {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth: Arc::new(Mutex::new(auth::resolve_runtime_auth()?)),
            key_pool: KeyPool::resolve(auth::PROVIDER, auth::API_KEY_ENV).ok(),
            system_prefix: None,
            refresh_lock: Arc::new(async_lock::Mutex::new(())),
        })
    }

    pub(crate) fn with_auth(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            key_pool: None,
            system_prefix: None,
            refresh_lock: Arc::new(async_lock::Mutex::new(())),
        })
    }

    pub(crate) fn with_system_prefix(mut self, prefix: Option<String>) -> Self {
        self.system_prefix = prefix;
        self
    }

    fn current_auth(&self) -> ResolvedAuth {
        self.auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn parse_cline_model(m: &Value) -> Option<ModelInfo> {
    let id = m["id"].as_str()?;
    let mut info = ModelInfo::id_only(id.to_string());
    info.is_free = Some(id.ends_with(FREE_MODEL_SUFFIX));
    Some(info)
}

/// Extract the credit balance from the Cline balance endpoint. The response
/// shape is not publicly documented, so accepted shapes are: a bare number, or
/// an object (top level or under `data`) carrying a numeric or numeric-string
/// `credits`/`balance`/`amount`/`remaining` field.
fn parse_balance_credits(text: &str) -> Result<f64, AgentError> {
    let value: Value = serde_json::from_str(text)?;
    if let Some(number) = value.as_f64() {
        return Ok(number);
    }
    let mut seen: Vec<String> = Vec::new();
    for container in [&value, &value["data"]] {
        for key in ["credits", "balance", "amount", "remaining"] {
            if let Some(raw) = container.get(key) {
                seen.push(key.to_string());
                if let Some(number) = raw.as_f64() {
                    return Ok(number);
                }
                if let Some(number) = raw.as_str().and_then(|s| s.trim().parse::<f64>().ok()) {
                    return Ok(number);
                }
            }
        }
    }
    Err(AgentError::Config {
        message: format!(
            "Cline balance response shape not recognized (saw keys: {})",
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.join(", ")
            }
        ),
    })
}

fn config_error(message: String) -> AgentError {
    AgentError::Config { message }
}

impl Provider for Cline {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let auth = self.current_auth();
            let task_header: Option<(String, String)> =
                session_id.map(|s| (X_TASK_ID_HEADER.to_string(), s.as_str().to_string()));
            let extra_headers: Vec<(&str, &str)> = task_header
                .as_ref()
                .map(|(k, v)| vec![(k.as_str(), v.as_str())])
                .unwrap_or_default();
            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                session_id.map(SessionRef::as_str),
                self.system_prefix.as_deref(),
                opts.message_cache_breakpoints,
                opts.fast,
            );
            opts.thinking.apply_thinking(
                &mut body,
                model,
                &dialect::STANDARD,
                &super::reasoning_effort_fields(),
            );
            super::apply_body_overrides(&mut body, model, &[super::MESSAGES_FIELD]);
            self.compat
                .do_stream(model, &extra_headers, &body, event_tx, &auth, &opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async move {
            let auth = self.current_auth();
            self.compat
                .fetch_and_parse_models(&auth, parse_cline_model)
                .await
        })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async move {
            let auth = self.current_auth();
            let base = CONFIG.base_url;
            let me_text = self
                .compat
                .get_text(&auth, &format!("{base}{USERS_ME_PATH}"))
                .await?;
            let me: Value = serde_json::from_str(&me_text)?;
            let user_id = me["id"]
                .as_str()
                .ok_or_else(|| config_error("Cline usage API returned no user id".into()))?
                .to_string();
            let balance_text = self
                .compat
                .get_text(&auth, &format!("{base}/users/{user_id}/balance"))
                .await?;
            let credits = parse_balance_credits(&balance_text)?;
            Ok(Some(ProviderUsage {
                plan: None,
                limits: vec![UsageLimit {
                    label: CREDITS_LABEL.to_string(),
                    percentage: None,
                    reset_at: None,
                    detail: Some(format!("${credits:.2} remaining")),
                }],
            }))
        })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        if self.key_pool.is_some() {
            return Box::pin(async { Ok(()) });
        }
        let refresh_lock = Arc::clone(&self.refresh_lock);
        Box::pin(async move {
            let _guard = refresh_lock.lock().await;
            smol::unblock(move || -> Result<(), AgentError> {
                let dir = StateDir::resolve().map_err(|_| AgentError::Storage)?;
                // Re-reads tokens from disk under the lock, so a refresh done
                // by another process is adopted instead of duplicated.
                match auth::ensure_fresh_tokens(&dir) {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(AgentError::SetupRequired {
                        message: "not authenticated, run `n00n auth login cline`".into(),
                    }),
                    Err(err) => Err(err),
                }
            })
            .await
        })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        let current = Arc::clone(&self.auth);
        Box::pin(async move {
            let resolved = auth::resolve_runtime_auth()?;
            *current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = resolved;
            Ok(())
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async {
            Ok(self
                .key_pool
                .as_ref()
                .is_some_and(|p| p.rotate_auth(&self.auth, auth::resolved_auth)))
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelTier;

    #[test]
    fn catalog_has_a_default_for_every_tier() {
        for tier in [
            ModelTier::Weak,
            ModelTier::Medium,
            ModelTier::Strong,
            ModelTier::Compaction,
        ] {
            let default = models()
                .iter()
                .find(|e| e.default && e.tier == tier)
                .unwrap_or_else(|| panic!("no default Cline model for tier {tier}"));
            assert!(!default.prefixes.is_empty(), "tier {tier} default empty");
        }
    }

    #[test]
    fn catalog_model_ids_keep_cline_pass_prefix() {
        for model in models() {
            assert!(
                model.prefixes.iter().all(|p| !p.starts_with("cline/")),
                "prefixes are relative to the provider slug, found {:?}",
                model.prefixes,
            );
        }
    }

    #[test]
    fn spec_resolves_through_the_manifest() {
        let model = Model::from_spec("cline/anthropic/claude-sonnet-4-6").unwrap();
        assert_eq!(model.provider.to_string(), "cline");
        assert_eq!(model.id, "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn pass_spec_resolves_through_the_manifest() {
        let model = Model::from_spec("cline/cline-pass/glm-5.3").unwrap();
        assert_eq!(model.id, "cline-pass/glm-5.3");
        assert_eq!(model.tier, ModelTier::Strong);
    }

    #[test]
    fn model_list_marks_free_models() {
        let info = parse_cline_model(&serde_json::json!({"id": "minimax/minimax-m2.5:free"}))
            .unwrap_or_else(|| panic!("free model should parse"));
        assert_eq!(info.is_free, Some(true));

        let info = parse_cline_model(&serde_json::json!({"id": "z-ai/glm-5.3"}))
            .unwrap_or_else(|| panic!("paid model should parse"));
        assert_eq!(info.is_free, Some(false));
    }

    #[test]
    fn model_list_rejects_entries_without_an_id() {
        assert!(parse_cline_model(&serde_json::json!({"object": "model"})).is_none());
    }

    #[test_case("12.5" => 12.5 ; "bare_number")]
    #[test_case(r#"{"credits": 4.2}"# => 4.2 ; "credits_field")]
    #[test_case(r#"{"data": {"balance": "10.00"}}"# => 10.0 ; "nested_string_balance")]
    #[test_case(r#"{"amount": 0}"# => 0.0 ; "zero_amount")]
    fn balance_response_parses(body: &str) -> f64 {
        parse_balance_credits(body).unwrap()
    }

    #[test]
    fn balance_response_rejects_unknown_shape() {
        let err = parse_balance_credits(r#"{"foo": {"bar": 1}}"#).unwrap_err();
        assert!(err.to_string().contains("not recognized"));
    }

    #[test]
    fn balance_response_rejects_non_numeric_credits() {
        let err = parse_balance_credits(r#"{"credits": "lots"}"#).unwrap_err();
        assert!(err.to_string().contains("not recognized"));
    }
}

use std::sync::{Arc, Mutex};

use flume::Sender;
use serde_json::Value;

use n00n_config::providers::{
    Protocol, ProviderDef, ProvidersConfig, resolve_api_key_env, resolve_base_url, resolve_protocol,
};
use n00n_storage::id::SessionRef;

use super::ResolvedAuth;
use super::openai::responses;
use super::openai_compat::OpenAiCompatProvider;
use crate::manifest::ManifestRegistry;
use crate::model::{FastPricing, Model, ModelPricing, ModelTier, lookup_entry};
use crate::provider::{BoxFuture, Provider, ProviderKind};
use crate::providers::Timeouts;
use crate::types::System;
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, dialect};

include!(concat!(env!("OUT_DIR"), "/provider_configs/custom.rs"));

fn protocol_kind(protocol: Protocol) -> ProviderKind {
    match protocol {
        Protocol::Openai | Protocol::OpenaiResponses => ProviderKind::OpenAi,
        Protocol::Anthropic => ProviderKind::Anthropic,
        Protocol::Google => ProviderKind::Google,
        Protocol::Devin => ProviderKind::Devin,
        Protocol::Cursor => ProviderKind::Cursor,
    }
}

/// Builtins win their slug in `from_spec`/`create`, so every custom path skips
/// them. Key off the manifest (all 13 builtins), not `builtin_provider`, which
/// omits `openrouter`/`opencode` and would let them shadow the builtin.
fn is_builtin_slug(slug: &str) -> bool {
    ManifestRegistry::get(slug).is_some()
}

pub fn base_kind(slug: &str) -> Option<ProviderKind> {
    let config = ProvidersConfig::load();
    Some(protocol_kind(config.get(slug)?.protocol?))
}

fn resolve_custom_auth(slug: &str) -> Result<ResolvedAuth, AgentError> {
    let config = ProvidersConfig::load();
    let def = config.get(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown custom provider '{slug}'"),
    })?;

    let protocol = resolve_protocol(slug, Some(def)).unwrap_or_else(|| Protocol::Openai);

    let base_url = resolve_base_url(slug, Some(def));

    // Devin and Cursor use their own CLI credentials when no API key is provided
    if protocol == Protocol::Devin || protocol == Protocol::Cursor {
        let resolved_env = resolve_api_key_env(slug, Some(def));
        let env_var = def
            .api_key_env
            .as_deref()
            .unwrap_or_else(|| resolved_env.as_str());
        match super::KeyPool::resolve(slug, env_var) {
            Ok(pool) => {
                let mut auth = ResolvedAuth::bearer(pool.current());
                auth.base_url = base_url;
                Ok(auth)
            }
            Err(e) => {
                tracing::debug!(
                    slug,
                    error = %e,
                    "no API key configured; provider CLI will use its own credentials"
                );
                Ok(ResolvedAuth {
                    base_url,
                    headers: Vec::new(),
                })
            }
        }
    } else {
        let resolved_env = resolve_api_key_env(slug, Some(def));
        let env_var = def
            .api_key_env
            .as_deref()
            .unwrap_or_else(|| resolved_env.as_str());
        let pool = super::KeyPool::resolve(slug, env_var)?;

        let mut auth = ResolvedAuth::bearer(pool.current());
        auth.base_url = base_url;
        Ok(auth)
    }
}

pub fn create(slug: &str, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    let kind = base_kind(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown custom provider '{slug}'"),
    })?;
    let resolved = resolve_custom_auth(slug)?;
    let auth = Arc::new(Mutex::new(resolved));

    let config = ProvidersConfig::load();
    let protocol = resolve_protocol(slug, config.get(slug)).unwrap_or_else(|| Protocol::Openai);

    match kind {
        ProviderKind::Anthropic => Ok(Box::new(super::anthropic::Anthropic::with_auth(
            auth, timeouts,
        )?)),
        ProviderKind::OpenAi => Ok(Box::new(CustomOpenAiProvider {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            protocol,
        })),
        ProviderKind::Google => Ok(Box::new(super::google::Google::with_auth(auth, timeouts)?)),
        ProviderKind::Devin => Ok(Box::new(super::devin::Devin::with_auth(&auth, timeouts)?)),
        ProviderKind::Cursor => Ok(Box::new(super::cursor::Cursor::with_auth(&auth, timeouts)?)),
        _ => Err(AgentError::Config {
            message: format!(
                "unsupported protocol for custom provider '{slug}', only openai/anthropic/google/devin/cursor are supported"
            ),
        }),
    }
}

pub fn lookup_model(slug: &str, model_id: &str) -> Option<Model> {
    if is_builtin_slug(slug) {
        return None;
    }
    let config = ProvidersConfig::load();
    let def = config.get(slug)?;
    let kind = protocol_kind(def.protocol?);
    Some(model_from_def(def, kind, slug, model_id))
}

/// Build a model from an already-loaded provider definition so tier resolution
/// and id lookup can share one `providers.toml` read instead of loading twice.
///
/// When the definition does not declare a model, metadata is inherited from the
/// base protocol's manifest so custom providers (e.g. a second Devin account)
/// share the canonical model registry instead of duplicating it.
fn model_from_def(def: &ProviderDef, kind: ProviderKind, slug: &str, model_id: &str) -> Model {
    let declared = def.models.iter().find(|m| m.id == model_id);
    let base_manifest = ManifestRegistry::get(&kind.to_string());
    let base_entry = base_manifest.and_then(|m| lookup_entry(m.models, model_id).ok());

    let tier = declared
        .map(|m| ModelTier::from(m.tier))
        .or(base_entry.map(|e| e.tier))
        .unwrap_or_else(|| ModelTier::Medium);
    let max_output_tokens = declared
        .and_then(|m| m.max_output_tokens)
        .or(base_entry.map(|e| e.max_output_tokens))
        .or_else(|| kind.fallback_max_output());
    let context_window = declared
        .and_then(|m| m.context_window)
        .or(base_entry.map(|e| e.context_window))
        .unwrap_or_else(|| kind.fallback_context_window());
    let supports_tool_examples_override = declared
        .and_then(|m| m.supports_tool_examples)
        .or(base_entry.map(|e| e.family.supports_tool_examples()));
    let supports_thinking_override = declared
        .and_then(|m| m.supports_thinking)
        .or_else(|| base_manifest.map(|m| m.supports_thinking));
    let supports_vision_override = declared
        .and_then(|m| m.supports_vision)
        .or(base_entry.map(|e| e.vision));
    let pricing = declared.filter(|m| m.has_pricing()).map_or_else(
        || base_entry.map_or_else(ModelPricing::default, |e| e.pricing),
        |m| ModelPricing {
            input: m.pricing_input.unwrap_or_else(|| 0.0),
            output: m.pricing_output.unwrap_or_else(|| 0.0),
            cache_write: m.pricing_cache_write.unwrap_or_else(|| 0.0),
            cache_read: m.pricing_cache_read.unwrap_or_else(|| 0.0),
            fast: m.has_fast_pricing().then(|| FastPricing {
                input: m.pricing_fast_input.unwrap_or_else(|| 0.0),
                output: m.pricing_fast_output.unwrap_or_else(|| 0.0),
            }),
        },
    );
    Model {
        id: model_id.to_string(),
        provider: Arc::from(slug),
        tier,
        family: base_entry.map_or_else(|| kind.family(), |e| e.family),
        supports_tool_examples_override,
        supports_thinking_override,
        supports_vision_override,
        supports_files_override: None,
        pricing,
        max_output_tokens,
        context_window,
        thinking_dialect: declared.and_then(|m| m.thinking_dialect),
        thinking_fields: declared.and_then(|m| m.thinking_fields.clone()),
        body_override: declared.and_then(|m| m.body_override.clone()),
    }
}

/// Specs declared statically in `providers.toml` (no HTTP).
pub fn declared_model_specs() -> Vec<String> {
    declared_specs_from(&ProvidersConfig::load())
}

fn declared_specs_from(config: &ProvidersConfig) -> Vec<String> {
    let mut specs = Vec::new();
    for (slug, def) in &config.providers {
        if is_builtin_slug(slug) {
            continue;
        }
        let Some(protocol) = resolve_protocol(slug, Some(def)) else {
            continue;
        };
        let kind = protocol_kind(protocol);

        // When a custom provider doesn't declare any models and isn't doing
        // live discovery, share the base protocol's static registry instead of
        // leaving the provider empty.
        if def.models.is_empty()
            && !def.discover_models
            && let Some(manifest) = ManifestRegistry::get(&kind.to_string())
        {
            specs.extend(manifest.models.iter().flat_map(|entry| {
                entry
                    .prefixes
                    .iter()
                    .map(|&prefix| format!("{slug}/{prefix}"))
            }));
        }

        for m in &def.models {
            specs.push(format!("{slug}/{}", m.id));
        }
    }
    specs
}

/// Outcome of resolving a tier against `providers.toml` in a single read.
pub enum TierLookup {
    Model(Box<Model>),
    /// Provider exists but declares no model at this tier; carries the base kind
    /// so the caller can inherit the base protocol's default.
    NoModelForTier(ProviderKind),
    Unknown,
}

pub fn resolve_tier(slug: &str, tier: ModelTier) -> TierLookup {
    // Builtins are never overridden through providers.toml (from_spec/create
    // check builtin first); keep the tier path consistent with that.
    if is_builtin_slug(slug) {
        return TierLookup::Unknown;
    }
    let config = ProvidersConfig::load();
    let Some(def) = config.get(slug) else {
        return TierLookup::Unknown;
    };
    let Some(protocol) = def.protocol else {
        return TierLookup::Unknown;
    };
    let kind = protocol_kind(protocol);
    match def.models.iter().find(|m| ModelTier::from(m.tier) == tier) {
        Some(declared) => {
            TierLookup::Model(Box::new(model_from_def(def, kind, slug, &declared.id)))
        }
        None => TierLookup::NoModelForTier(kind),
    }
}

/// Skip definitions handled by [`declared_model_specs`]; only HTTP `/models`
/// goes through here, so an empty `discover_models = false` provider returns
/// nothing and never hits the network.
pub fn discover_models(timeouts: Timeouts) -> Vec<String> {
    let config = ProvidersConfig::load();
    let mut all_specs = Vec::new();
    for slug in config.providers.keys() {
        if is_builtin_slug(slug) {
            continue;
        }
        let Some(def) = config.get(slug) else {
            continue;
        };
        if !def.discover_models {
            continue;
        }
        if resolve_protocol(slug, Some(def)).is_none() {
            continue;
        }
        match create(slug, timeouts) {
            Ok(provider) => {
                let slug_c = slug.clone();
                let result = smol::block_on(provider.list_models());
                match result {
                    Ok(models) => {
                        for m in models {
                            all_specs.push(format!("{slug_c}/{}", m.id));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(slug, error = %e, "failed to list models for custom provider");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(slug, error = %e, "failed to create custom provider");
            }
        }
    }
    all_specs
}

struct CustomOpenAiProvider {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    protocol: Protocol,
}

impl Provider for CustomOpenAiProvider {
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
            let auth = self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();

            if self.protocol == Protocol::OpenaiResponses {
                let body = responses::build_body(
                    model,
                    messages,
                    system,
                    tools,
                    None,
                    None,
                    false,
                    &opts,
                    self.compat.config().supports_parallel_tool_calls,
                );
                return responses::do_stream(
                    self.compat.client(),
                    model,
                    &body,
                    event_tx,
                    &auth,
                    self.compat.stream_timeout(),
                    &opts,
                )
                .await
                .map(|(_, response)| response);
            }

            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                session_id.map(n00n_storage::id::SessionRef::as_str),
                None,
                opts.message_cache_breakpoints,
                opts.fast,
            );
            // Custom OpenAI-compatible endpoints get the plain
            // `reasoning_effort` field; anything else a model needs is
            // declared per model in `providers.toml`.
            opts.thinking.apply_thinking(
                &mut body,
                model,
                &dialect::PREFER_HIGH,
                &super::reasoning_effort_fields(),
            );
            super::apply_body_overrides(&mut body, model, &[super::MESSAGES_FIELD]);
            self.compat
                .do_stream(model, &[], &body, event_tx, &auth, &opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        let auth = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move { self.compat.do_list_models(&auth).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai_def(model_id: &str) -> ProviderDef {
        serde_json::from_str(&format!(
            r#"{{"protocol":"openai","models":[{{"id":"{model_id}"}}]}}"#
        ))
        .unwrap()
    }

    // `openrouter` is a builtin whose slug is absent from the `builtin_provider`
    // inventory; the old guard leaked it into the picker, where it then resolved
    // as the builtin and silently dropped the custom model. Listing must skip
    // every builtin slug so a providers.toml entry can never shadow one.
    #[test]
    fn custom_openai_protocol_omits_parallel_tool_calls() {
        let provider = OpenAiCompatProvider::new(&CONFIG, Timeouts::default()).unwrap();
        let model = Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = serde_json::json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &tools,
            None,
            None,
            0,
            false,
        );

        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn custom_openai_responses_protocol_omits_parallel_tool_calls() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let tools = serde_json::json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);

        let opts = crate::RequestOptions::default();
        let body = responses::build_body(
            &model,
            &[],
            &System::from("system"),
            &tools,
            None,
            None,
            false,
            &opts,
            CONFIG.supports_parallel_tool_calls,
        );

        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn declared_specs_skip_builtin_named_entries_but_keep_custom() {
        let mut config = ProvidersConfig::default();
        config.upsert("openrouter".to_string(), openai_def("shadow-model"));
        config.upsert("my-custom".to_string(), openai_def("real-model"));

        let specs = declared_specs_from(&config);
        assert!(
            !specs.iter().any(|s| s.starts_with("openrouter/")),
            "builtin slug must be skipped in custom listing: {specs:?}"
        );
        assert!(specs.contains(&"my-custom/real-model".to_string()));

        // Resolution owns the builtin slug regardless of the providers.toml entry.
        let model = Model::from_spec("openrouter/shadow-model").unwrap();
        assert_eq!(model.provider.as_ref(), "openrouter");
    }
}

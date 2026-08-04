use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use flume::Sender;
use n00n_config::providers::ProvidersConfig;
use n00n_storage::id::SessionRef;
use n00n_storage::sessions::{BodyOverride, EffortDialectId, ThinkingFieldConfig};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use strum::IntoEnumIterator;
use tracing::{debug, warn};

use crate::manifest::ManifestRegistry;
use crate::model::{Model, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider, ProviderKind};
use crate::{
    AgentError, Message, ProviderEvent, ProviderUsage, RequestOptions, StreamResponse, System,
};

use super::ResolvedAuth;
use super::anthropic::Anthropic;
use super::copilot::Copilot;
use super::cursor::Cursor;
use super::deepseek::DeepSeek;
use super::devin::Devin;
use super::google::Google;
use super::local::{LLAMACPP, LocalEndpoint, OLLAMA};
use super::mistral::Mistral;
use super::openai::OpenAi;
use super::opencode::Opencode;
use super::openrouter::OpenRouter;
use super::synthetic::Synthetic;
use super::tensorx::TensorX;
use super::zai::Zai;

const INFO_TIMEOUT: Duration = Duration::from_secs(5);
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDERS_DIR: &str = "providers";

struct DynamicProviderMeta {
    slug: String,
    display_name: String,
    base: ProviderKind,
    system_prefix: Option<String>,
    has_auth: bool,
    script_path: PathBuf,
    models: Vec<ScriptModel>,
    model_filters: Vec<ScriptModelFilter>,
}

#[derive(Deserialize)]
struct ScriptInfo {
    display_name: String,
    base: String,
    #[serde(default)]
    system_prefix: Option<String>,
    has_auth: bool,
    /// Provider-wide body shaping. Each entry contributes its `body_override`
    /// to every model whose id matches its `match` glob; a model's own
    /// `body_override` is applied first and wins on conflicting keys.
    #[serde(default)]
    model_filters: Vec<ScriptModelFilter>,
}

#[derive(Deserialize)]
struct ScriptModelFilter {
    #[serde(rename = "match")]
    match_pattern: String,
    #[serde(default)]
    body_override: Option<BodyOverride>,
}

#[derive(Deserialize)]
struct ScriptModel {
    id: String,
    #[serde(default = "default_tier")]
    tier: ModelTier,
    #[serde(default)]
    supports_tool_examples: Option<bool>,
    #[serde(default)]
    supports_thinking: Option<bool>,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default = "default_max_output_tokens")]
    max_output_tokens: u32,
    #[serde(default = "default_context_window")]
    context_window: u32,
    #[serde(default)]
    pricing: Option<ModelPricing>,
    #[serde(default)]
    thinking_dialect: Option<EffortDialectId>,
    #[serde(default)]
    thinking_fields: Option<ThinkingFieldConfig>,
    #[serde(default)]
    body_override: Option<BodyOverride>,
}

impl ScriptModel {
    fn to_model(
        &self,
        slug: &str,
        base: ProviderKind,
        id: String,
        tier: ModelTier,
        model_filters: &[ScriptModelFilter],
    ) -> Model {
        let body_override = resolve_overrides(&id, model_filters, self.body_override.as_ref());
        Model {
            id,
            provider: Arc::from(slug),
            tier,
            family: base.family(),
            supports_tool_examples_override: self.supports_tool_examples,
            supports_thinking_override: self.supports_thinking,
            supports_vision_override: self.supports_vision,
            supports_files_override: None,
            pricing: self.pricing.unwrap_or_else(Default::default),
            max_output_tokens: Some(self.max_output_tokens),
            context_window: self.context_window,
            thinking_dialect: self.thinking_dialect,
            thinking_fields: self.thinking_fields.clone(),
            body_override,
        }
    }
}

/// Shell-style glob over the whole id: `*` matches any run (including empty),
/// `?` matches one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut pattern_idx, mut text_idx) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while text_idx < text.len() {
        let matches_here = pattern_idx < pattern.len()
            && (pattern[pattern_idx] == b'?' || pattern[pattern_idx] == text[text_idx]);
        if pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
            star = Some((pattern_idx, text_idx));
            pattern_idx += 1;
        } else if matches_here {
            pattern_idx += 1;
            text_idx += 1;
        } else if let Some((star_idx, star_text_idx)) = star {
            pattern_idx = star_idx + 1;
            text_idx = star_text_idx + 1;
            star = Some((star_idx, text_idx));
        } else {
            return false;
        }
    }
    pattern[pattern_idx..].iter().all(|b| *b == b'*')
}

/// Effective body override for one model id: the per-model value first, then
/// every matching provider-wide filter in declaration order. Objects
/// deep-merge with earlier contributions winning; `filter` unions.
fn resolve_overrides(
    model_id: &str,
    filters: &[ScriptModelFilter],
    per_model: Option<&BodyOverride>,
) -> Option<BodyOverride> {
    let mut result = per_model.cloned();
    for entry in filters
        .iter()
        .filter(|entry| glob_match(&entry.match_pattern, model_id))
    {
        let Some(entry_override) = &entry.body_override else {
            continue;
        };
        let merged = result.get_or_insert_with(BodyOverride::default);
        if let Some(defaults) = &entry_override.defaults {
            merge_keeping_existing(merged.defaults.get_or_insert_with(|| json!({})), defaults);
        }
        if let Some(replace) = &entry_override.replace {
            merge_keeping_existing(merged.replace.get_or_insert_with(|| json!({})), replace);
        }
        for key in &entry_override.filter {
            if !merged.filter.contains(key) {
                merged.filter.push(key.clone());
            }
        }
    }
    result
}

/// Deep-merge `src` into `target`, keeping every key `target` already has so
/// the more specific per-model override wins over provider-wide filters.
fn merge_keeping_existing(target: &mut Value, src: &Value) {
    if let (Some(target), Some(src)) = (target.as_object_mut(), src.as_object()) {
        merge_maps_keeping_existing(target, src);
    }
}

fn merge_maps_keeping_existing(target: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (key, value) in src {
        match target.get_mut(key) {
            Some(existing) => {
                if let (Some(existing), Some(value)) = (existing.as_object_mut(), value.as_object())
                {
                    merge_maps_keeping_existing(existing, value);
                }
            }
            None => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn default_tier() -> ModelTier {
    ModelTier::Medium
}

fn default_max_output_tokens() -> u32 {
    16384
}

fn default_context_window() -> u32 {
    128_000
}

#[derive(Deserialize)]
struct ScriptResolvedAuth {
    base_url: Option<String>,
    headers: HashMap<String, String>,
}

impl From<ScriptResolvedAuth> for ResolvedAuth {
    fn from(s: ScriptResolvedAuth) -> Self {
        Self {
            base_url: s.base_url,
            headers: s.headers.into_iter().collect(),
        }
    }
}

fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn builtin_slugs() -> Vec<String> {
    ProviderKind::iter().map(|k| k.to_string()).collect()
}

fn providers_dir() -> Option<PathBuf> {
    n00n_storage::paths::config_dir()
        .ok()
        .map(|d| d.join(PROVIDERS_DIR))
}

fn run_script(path: &Path, subcommand: &str, timeout: Duration) -> Result<String, AgentError> {
    let mut child = Command::new(path)
        .arg(subcommand)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AgentError::Config {
            message: format!("failed to run {} {subcommand}: {e}", path.display()),
        })?;

    let output = match wait_timeout::ChildExt::wait_timeout(&mut child, timeout) {
        Ok(Some(_)) => child.wait_with_output().map_err(|e| AgentError::Config {
            message: format!(
                "failed to read output of {} {subcommand}: {e}",
                path.display()
            ),
        })?,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AgentError::Config {
                message: format!(
                    "{} {subcommand} timed out after {}s",
                    path.display(),
                    timeout.as_secs()
                ),
            });
        }
        Err(e) => {
            return Err(AgentError::Config {
                message: format!("failed to wait on {} {subcommand}: {e}", path.display()),
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AgentError::Config {
            message: if stderr.is_empty() {
                format!(
                    "{} {subcommand} exited with {}",
                    path.display(),
                    output.status
                )
            } else {
                stderr
            },
        });
    }

    String::from_utf8(output.stdout).map_err(|_| AgentError::Config {
        message: format!("{} {subcommand}: stdout is not valid UTF-8", path.display()),
    })
}

fn run_script_interactive(path: &Path, subcommand: &str) -> Result<(), AgentError> {
    let status = Command::new(path)
        .arg(subcommand)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| AgentError::Config {
            message: format!("failed to run {} {subcommand}: {e}", path.display()),
        })?;

    if !status.success() {
        return Err(AgentError::Config {
            message: format!("{} {subcommand} exited with {status}", path.display()),
        });
    }
    Ok(())
}

fn resolve_auth(meta: &DynamicProviderMeta) -> Result<ResolvedAuth, AgentError> {
    let stdout = run_script(&meta.script_path, "resolve", SCRIPT_TIMEOUT)?;
    let parsed: ScriptResolvedAuth =
        serde_json::from_str(&stdout).map_err(|e| AgentError::Config {
            message: format!("{} resolve: invalid JSON: {e}", meta.script_path.display()),
        })?;
    Ok(parsed.into())
}

fn discover_in(dir: &Path) -> Vec<DynamicProviderMeta> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let builtins = builtin_slugs();
    let mut result = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = path.metadata()
                && meta.permissions().mode() & 0o111 == 0
            {
                debug!(path = %path.display(), "skipping non-executable file");
                continue;
            }
        }

        #[cfg(windows)]
        {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_ascii_lowercase();
                if !matches!(ext.as_str(), "exe" | "bat" | "cmd" | "ps1") {
                    debug!(path = %path.display(), "skipping non-executable file");
                    continue;
                }
            } else {
                debug!(path = %path.display(), "skipping file without extension");
                continue;
            }
        }

        let name_part = if cfg!(windows) {
            path.file_stem()
        } else {
            path.file_name()
        };
        let slug = match name_part.and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        if !is_valid_slug(&slug) {
            warn!(slug, "invalid provider slug, skipping");
            continue;
        }

        if builtins.iter().any(|b| b == &slug) {
            warn!(slug, "slug collides with built-in provider, skipping");
            continue;
        }

        let stdout = match run_script(&path, "info", INFO_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                warn!(slug, error = %e, "failed to get provider info, skipping");
                continue;
            }
        };

        let info: ScriptInfo = match serde_json::from_str(&stdout) {
            Ok(i) => i,
            Err(e) => {
                warn!(slug, error = %e, "invalid info JSON, skipping");
                continue;
            }
        };

        let Ok(base) = ProviderKind::from_str(&info.base) else {
            warn!(slug, base = info.base, "unknown base provider, skipping");
            continue;
        };

        let models = match run_script(&path, "models", INFO_TIMEOUT) {
            Ok(s) => serde_json::from_str::<Vec<ScriptModel>>(&s).unwrap_or_else(|e| {
                warn!(slug, error = %e, "invalid models JSON, falling back to base models");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };

        result.push(DynamicProviderMeta {
            slug,
            display_name: info.display_name,
            base,
            system_prefix: info.system_prefix.filter(|s| !s.is_empty()),
            has_auth: info.has_auth,
            script_path: path,
            models,
            model_filters: info.model_filters,
        });
    }

    result
}

static DISCOVERED: OnceLock<Vec<DynamicProviderMeta>> = OnceLock::new();

fn discover() -> &'static [DynamicProviderMeta] {
    DISCOVERED.get_or_init(|| {
        // Load config first: it hard-exits on malformed providers.toml, so fail
        // before spawning every provider script.
        let custom = ProvidersConfig::load();
        let mut metas = providers_dir().map_or_else(Vec::new, |d| discover_in(&d));
        // A script and a providers.toml entry must not share a slug. The script
        // loses, the same way it already loses to a builtin, and we say so
        // instead of silently picking a winner.
        metas.retain(|m| {
            if custom.get(&m.slug).is_some() {
                warn!(
                    slug = %m.slug,
                    "provider slug also defined in providers.toml, skipping script"
                );
                false
            } else {
                true
            }
        });
        metas
    })
}

fn find_meta(slug: &str) -> Option<&'static DynamicProviderMeta> {
    discover().iter().find(|m| m.slug == slug)
}

/// Log in to a dynamic script-based provider.
///
/// # Errors
///
/// Returns an `AgentError` if the provider is unknown, does not support login, or the script fails.
pub fn login(slug: &str) -> Result<(), AgentError> {
    let meta = find_meta(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown provider '{slug}'"),
    })?;
    if !meta.has_auth {
        return Err(AgentError::Config {
            message: format!("provider '{slug}' does not support login (uses API key)"),
        });
    }
    run_script_interactive(&meta.script_path, "login")
}

/// Log out of a dynamic script-based provider.
///
/// # Errors
///
/// Returns an `AgentError` if the provider is unknown, does not support logout, or the script fails.
pub fn logout(slug: &str) -> Result<(), AgentError> {
    let meta = find_meta(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown provider '{slug}'"),
    })?;
    if !meta.has_auth {
        return Err(AgentError::Config {
            message: format!("provider '{slug}' does not support logout (uses API key)"),
        });
    }
    run_script_interactive(&meta.script_path, "logout")
}

#[must_use]
pub fn auth_providers() -> Vec<(&'static str, &'static str)> {
    discover()
        .iter()
        .filter(|m| m.has_auth)
        .map(|m| (m.slug.as_str(), m.display_name.as_str()))
        .collect()
}

/// Create a dynamic provider instance by slug.
///
/// # Errors
///
/// Returns an `AgentError` if the provider is unknown, auth resolution fails, or the base provider cannot be created.
pub fn create(slug: &str, timeouts: super::Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    let meta = find_meta(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown dynamic provider '{slug}'"),
    })?;
    let resolved = resolve_auth(meta)?;
    let auth = Arc::new(Mutex::new(resolved));

    let inner: Box<dyn Provider> = match meta.base {
        ProviderKind::Anthropic => Box::new(
            Anthropic::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::OpenAi => Box::new(
            OpenAi::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Codex => Box::new(
            OpenAi::with_auth_options(Arc::clone(&auth), timeouts, crate::OpenAiOptions::codex())?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Google => Box::new(Google::with_auth(Arc::clone(&auth), timeouts)?),
        ProviderKind::Copilot => Box::new(
            Copilot::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Ollama => Box::new(
            LocalEndpoint::with_auth(&OLLAMA, Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::LlamaCpp => Box::new(
            LocalEndpoint::with_auth(&LLAMACPP, Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Mistral => Box::new(
            Mistral::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Zai => Box::new(
            Zai::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Synthetic => Box::new(
            Synthetic::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::DeepSeek => Box::new(
            DeepSeek::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::OpenRouter => Box::new(
            OpenRouter::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::TensorX => Box::new(
            TensorX::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Opencode => Box::new(
            Opencode::with_auth(Arc::clone(&auth), timeouts)?
                .with_system_prefix(meta.system_prefix.clone()),
        ),
        ProviderKind::Devin => Box::new(Devin::with_auth(&auth, timeouts)?),
        ProviderKind::Cursor => Box::new(Cursor::with_auth(&auth, timeouts)?),
    };

    Ok(Box::new(DynamicProvider {
        script_path: &meta.script_path,
        inner,
        auth,
        models: &meta.models,
    }))
}

#[must_use]
pub fn display_name(slug: &str) -> Option<&'static str> {
    find_meta(slug).map(|m| m.display_name.as_str())
}

#[must_use]
pub fn dynamic_model_specs_for(slug: &str) -> Vec<String> {
    let Some(meta) = find_meta(slug) else {
        return Vec::new();
    };
    if meta.models.is_empty() {
        let base_slug = meta.base.to_string();
        ManifestRegistry::get(&base_slug)
            .map_or(&[] as &[crate::model::ModelEntry], |m| m.models)
            .iter()
            .flat_map(|entry| entry.prefixes.iter())
            .map(|prefix| format!("{slug}/{prefix}"))
            .collect()
    } else {
        meta.models
            .iter()
            .map(|m| format!("{slug}/{}", m.id))
            .collect()
    }
}

#[must_use]
pub fn discovered_slugs() -> Vec<&'static str> {
    discover().iter().map(|m| m.slug.as_str()).collect()
}

#[must_use]
pub fn base_for_slug(slug: &str) -> Option<ProviderKind> {
    find_meta(slug).map(|m| m.base)
}

#[must_use]
pub fn lookup_model(slug: &str, model_id: &str) -> Option<Model> {
    let meta = find_meta(slug)?;
    let script_model = meta
        .models
        .iter()
        .filter(|m| model_id.starts_with(&m.id))
        .max_by_key(|m| m.id.len())?;
    Some(script_model.to_model(
        slug,
        meta.base,
        model_id.to_string(),
        script_model.tier,
        &meta.model_filters,
    ))
}

#[must_use]
pub fn find_model_for_tier(slug: &str, tier: ModelTier) -> Option<Model> {
    let meta = find_meta(slug)?;
    let script_model = meta.models.iter().find(|m| m.tier == tier)?;
    Some(script_model.to_model(
        slug,
        meta.base,
        script_model.id.clone(),
        tier,
        &meta.model_filters,
    ))
}

struct DynamicProvider {
    script_path: &'static Path,
    inner: Box<dyn Provider>,
    auth: Arc<Mutex<ResolvedAuth>>,
    models: &'static [ScriptModel],
}

impl DynamicProvider {
    fn run_auth_script(&self, subcommand: &'static str) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let script_path = self.script_path;
            let auth = Arc::clone(&self.auth);
            smol::unblock(move || {
                let stdout = run_script(script_path, subcommand, SCRIPT_TIMEOUT)?;
                let parsed: ScriptResolvedAuth =
                    serde_json::from_str(&stdout).map_err(|e| AgentError::Config {
                        message: format!(
                            "{} {subcommand}: invalid JSON: {e}",
                            script_path.display()
                        ),
                    })?;
                *auth
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = parsed.into();
                Ok(())
            })
            .await
        })
    }
}

impl Provider for DynamicProvider {
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
        self.inner
            .stream_message(model, messages, system, tools, event_tx, opts, session_id)
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        if self.models.is_empty() {
            return self.inner.list_models();
        }
        Box::pin(async {
            Ok(self
                .models
                .iter()
                .map(|m| crate::model::ModelInfo {
                    id: m.id.clone(),
                    name: None,
                    context_window: Some(m.context_window),
                    max_output_tokens: Some(m.max_output_tokens),
                    pricing: m.pricing,
                    supports_thinking: None,
                    supports_vision: m.supports_vision,
                    tier: None,
                    is_free: None,
                    is_promo: None,
                    provider_info: None,
                })
                .collect())
        })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        self.run_auth_script("refresh")
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        self.run_auth_script("reload")
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        self.inner.fetch_usage()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::{self, File};
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use tempfile::TempDir;
    use test_case::test_case;

    #[test_case("myslug", true ; "valid_simple")]
    #[test_case("my-slug", true ; "valid_hyphen")]
    #[test_case("my_slug", true ; "valid_underscore")]
    #[test_case("A1", true ; "valid_upper")]
    #[test_case("", false ; "empty")]
    #[test_case("-bad", false ; "leading_hyphen")]
    #[test_case("has.dot", false ; "has_dot")]
    #[test_case("has/slash", false ; "has_slash")]
    #[test_case("has space", false ; "has_space")]
    fn slug_validation(input: &str, expected: bool) {
        assert_eq!(is_valid_slug(input), expected);
    }

    #[test]
    fn script_resolved_auth_deserialization() {
        let with_base =
            r#"{"base_url": "https://example.com", "headers": {"authorization": "Bearer tok"}}"#;
        let resolved: ResolvedAuth = serde_json::from_str::<ScriptResolvedAuth>(with_base)
            .unwrap()
            .into();
        assert_eq!(resolved.base_url.as_deref(), Some("https://example.com"));
        assert_eq!(resolved.headers[0].1, "Bearer tok");

        let without_base = r#"{"headers": {"authorization": "Bearer x"}}"#;
        let resolved: ResolvedAuth = serde_json::from_str::<ScriptResolvedAuth>(without_base)
            .unwrap()
            .into();
        assert!(resolved.base_url.is_none());
    }

    #[test]
    fn script_info_deserialization() {
        let minimal = r#"{"display_name": "Test", "base": "anthropic", "has_auth": true}"#;
        let info: ScriptInfo = serde_json::from_str(minimal).unwrap();
        assert_eq!(info.display_name, "Test");
        assert_eq!(info.base, "anthropic");
        assert!(info.has_auth);
        assert!(info.system_prefix.is_none());

        let with_prefix = r#"{"display_name": "T", "base": "openai", "has_auth": false, "system_prefix": "You are X."}"#;
        let info: ScriptInfo = serde_json::from_str(with_prefix).unwrap();
        assert_eq!(info.system_prefix.as_deref(), Some("You are X."));
    }

    #[test]
    fn script_model_deserialization() {
        let full = r#"{"id": "my-model", "tier": "strong", "supports_tool_examples": true, "max_output_tokens": 32000, "context_window": 200000, "pricing": {"input": 3.0, "output": 15.0, "cache_write": 3.75, "cache_read": 0.30}}"#;
        let model: ScriptModel = serde_json::from_str(full).unwrap();
        assert_eq!(model.id, "my-model");
        assert_eq!(model.tier, ModelTier::Strong);
        assert_eq!(model.supports_tool_examples, Some(true));
        assert!(model.pricing.is_some());

        let minimal: ScriptModel = serde_json::from_str(r#"{"id": "custom-v1"}"#).unwrap();
        assert_eq!(minimal.tier, ModelTier::Medium);
        assert_eq!(minimal.supports_tool_examples, None);
        assert_eq!(minimal.max_output_tokens, 16384);
        assert_eq!(minimal.context_window, 128_000);
        assert!(minimal.pricing.is_none());
        assert!(minimal.thinking_dialect.is_none());
        assert!(minimal.thinking_fields.is_none());
        assert!(minimal.body_override.is_none());
    }

    #[test]
    fn script_model_deserializes_thinking_and_body_config() {
        let declared = r#"{
            "id": "my-model",
            "thinking_dialect": "glm",
            "thinking_fields": {"effort_path": "reasoning.effort", "toggles": [{"path": "thinking", "on": true}]},
            "body_override": {"defaults": {"chat_template_kwargs": {"enable_thinking": true}}, "filter": ["min_tokens"]}
        }"#;
        let model: ScriptModel = serde_json::from_str(declared).unwrap();
        assert_eq!(model.thinking_dialect, Some(EffortDialectId::Glm));
        let fields = model.thinking_fields.as_ref().unwrap();
        assert_eq!(fields.effort_path.as_deref(), Some("reasoning.effort"));
        assert_eq!(fields.toggles[0].on, Some(json!(true)));
        let override_config = model.body_override.as_ref().unwrap();
        assert_eq!(
            override_config.defaults.as_ref().unwrap()["chat_template_kwargs"]["enable_thinking"],
            true
        );
        assert_eq!(override_config.filter, vec!["min_tokens".to_string()]);
    }

    #[test_case("hint*", "hint-mod-3", true ; "trailing_star")]
    #[test_case("hint*", "hint", true ; "star_matches_empty")]
    #[test_case("*-mod", "hint-mod", true ; "leading_star")]
    #[test_case("hint?mod", "hint-mod", true ; "question_matches_one")]
    #[test_case("hint?mod", "hint--mod", false ; "question_needs_exactly_one")]
    #[test_case("hint*", "other-hint", false ; "anchored_at_start")]
    #[test_case("exact", "exact", true ; "literal")]
    #[test_case("exact", "exactly", false ; "literal_is_full_match")]
    #[test_case("*", "anything/goes", true ; "star_spans_slashes")]
    fn glob_match_patterns(pattern: &str, text: &str, expected: bool) {
        assert_eq!(glob_match(pattern, text), expected);
    }

    fn filter(pattern: &str, body_override: BodyOverride) -> ScriptModelFilter {
        ScriptModelFilter {
            match_pattern: pattern.into(),
            body_override: Some(body_override),
        }
    }

    /// Matching filters accumulate; non-matching ones contribute nothing.
    #[test]
    fn resolve_overrides_accumulates_matching_filters_only() {
        let filters = vec![
            filter(
                "hint*",
                BodyOverride {
                    defaults: Some(json!({"chat_template_kwargs": {"enable_thinking": true}})),
                    filter: vec!["min_tokens".into()],
                    ..Default::default()
                },
            ),
            filter(
                "unrelated-*",
                BodyOverride {
                    defaults: Some(json!({"poison": true})),
                    filter: vec!["poison_field".into()],
                    ..Default::default()
                },
            ),
        ];
        let per_model = BodyOverride {
            defaults: Some(json!({"temperature": 0.1})),
            filter: vec!["always_strip".into()],
            ..Default::default()
        };

        let resolved = resolve_overrides("hint-mod-3", &filters, Some(&per_model)).unwrap();

        let defaults = resolved.defaults.as_ref().unwrap();
        assert_eq!(defaults["temperature"], 0.1);
        assert_eq!(defaults["chat_template_kwargs"]["enable_thinking"], true);
        assert!(defaults.get("poison").is_none());
        assert_eq!(resolved.filter, vec!["always_strip", "min_tokens"]);
    }

    /// The per-model value is the more specific one, so it wins conflicts.
    #[test]
    fn resolve_overrides_keeps_per_model_value_on_conflict() {
        let filters = vec![filter(
            "*",
            BodyOverride {
                defaults: Some(json!({"temperature": 0.9, "top_p": 0.5})),
                ..Default::default()
            },
        )];
        let per_model = BodyOverride {
            defaults: Some(json!({"temperature": 0.1})),
            ..Default::default()
        };

        let resolved = resolve_overrides("any-model", &filters, Some(&per_model)).unwrap();

        let defaults = resolved.defaults.as_ref().unwrap();
        assert_eq!(defaults["temperature"], 0.1);
        assert_eq!(defaults["top_p"], 0.5);
    }

    #[test]
    fn resolve_overrides_without_matches_stays_none() {
        let filters = vec![filter("other-*", BodyOverride::default())];
        assert!(resolve_overrides("model-x", &filters, None).is_none());
        assert!(resolve_overrides("model-x", &[], None).is_none());
    }

    /// A provider-wide filter reaches models that declare nothing themselves.
    #[test]
    fn script_model_inherits_provider_wide_filter() {
        let filters = vec![filter(
            "gpt-*",
            BodyOverride {
                filter: vec!["context_management".into()],
                ..Default::default()
            },
        )];
        let script_model: ScriptModel = serde_json::from_str(r#"{"id": "gpt-oss"}"#).unwrap();
        let model = script_model.to_model(
            "myproxy",
            ProviderKind::OpenAi,
            "gpt-oss-120b".into(),
            ModelTier::Medium,
            &filters,
        );
        assert_eq!(
            model.body_override.unwrap().filter,
            vec!["context_management".to_string()]
        );
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, info_json: &str) -> PathBuf {
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  info) echo '{info_json}' ;;\n  resolve) echo '{{\"headers\": {{\"authorization\": \"Bearer test\"}}}}' ;;\n  refresh) echo '{{\"headers\": {{\"authorization\": \"Bearer refreshed\"}}}}' ;;\n  *) exit 1 ;;\nesac\n"
        );
        let mut file = File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn discover_finds_valid_script() {
        let tmp = TempDir::new().unwrap();
        write_script(
            tmp.path(),
            "test-provider",
            r#"{"display_name": "Test", "base": "anthropic", "has_auth": true}"#,
        );
        let providers = discover_in(tmp.path());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].slug, "test-provider");
        assert_eq!(providers[0].display_name, "Test");
        assert_eq!(providers[0].base, ProviderKind::Anthropic);
        assert!(providers[0].has_auth);
        assert!(providers[0].models.is_empty());
    }

    #[cfg(unix)]
    #[test_case("anthropic", r#"{"display_name": "Fake", "base": "anthropic", "has_auth": false}"# ; "builtin_collision")]
    #[test_case("has.dot", r#"{"display_name": "Bad", "base": "anthropic", "has_auth": false}"# ; "invalid_slug")]
    #[test_case("weird", r#"{"display_name": "Weird", "base": "unknown-provider", "has_auth": false}"# ; "unknown_base")]
    fn discover_skips_invalid(name: &str, info_json: &str) {
        let tmp = TempDir::new().unwrap();
        write_script(tmp.path(), name, info_json);
        assert!(discover_in(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn discover_parses_models_subcommand() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("custom-llm");
        let script = r#"#!/bin/sh
case "$1" in
  info) echo '{"display_name": "Custom", "base": "openai", "has_auth": false}' ;;
  models) echo '[{"id": "custom-v1", "tier": "strong", "max_output_tokens": 32000, "context_window": 200000}]' ;;
  resolve) echo '{"headers": {"authorization": "Bearer test"}}' ;;
  *) exit 1 ;;
esac
"#;
        let mut file = File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let providers = discover_in(tmp.path());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].models.len(), 1);
        assert_eq!(providers[0].models[0].id, "custom-v1");
        assert_eq!(providers[0].models[0].tier, ModelTier::Strong);
    }

    #[cfg(unix)]
    #[test]
    fn run_script_error_on_bad_subcommand() {
        let tmp = TempDir::new().unwrap();
        let path = write_script(
            tmp.path(),
            "test-err",
            r#"{"display_name": "T", "base": "anthropic", "has_auth": false}"#,
        );
        assert!(matches!(
            run_script(&path, "nonexistent", SCRIPT_TIMEOUT).unwrap_err(),
            AgentError::Config { .. }
        ));
    }

    #[cfg(unix)]
    #[test_case("ollama", ProviderKind::Ollama ; "base_ollama")]
    #[test_case("llama-cpp", ProviderKind::LlamaCpp ; "base_llama_cpp")]
    #[test_case("mistral", ProviderKind::Mistral ; "base_mistral")]
    #[test_case("zai", ProviderKind::Zai ; "base_zai")]
    #[test_case("synthetic", ProviderKind::Synthetic ; "base_synthetic")]
    #[test_case("deepseek", ProviderKind::DeepSeek ; "base_deepseek")]
    #[test_case("opencode", ProviderKind::Opencode ; "base_opencode")]
    fn discover_accepts_all_bases(base: &str, expected: ProviderKind) {
        let tmp = TempDir::new().unwrap();
        let info = format!(r#"{{"display_name": "Test", "base": "{base}", "has_auth": false}}"#);
        write_script(tmp.path(), "custom-test", &info);
        let providers = discover_in(tmp.path());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].base, expected);
    }
}

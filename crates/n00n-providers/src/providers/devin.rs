//! Native Devin provider using Connect protocol over gRPC-Web.
//!
//! Protocol:
//! 1. Read `~/.local/share/devin/credentials.toml` or env for session token
//! 2. Call `POST /exa.auth_pb.AuthService/GetUserJwt` (application/proto) to get user JWT
//! 3. Call `POST /exa.api_server_pb.ApiServerService/GetChatMessage` (application/connect+proto)
//!    with gzip-framed request, stream of gzip-framed responses
//! 4. Parse Connect frames, gunzip, decode protobuf, emit `ProviderEvents`

use std::collections::HashMap;
use std::io::{ErrorKind, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_lite::io::{AsyncReadExt, BufReader};

use flate2::Compression;
use flate2::write::GzEncoder;
use flume::Sender;
use isahc::error::ErrorKind as HttpErrorKind;
use isahc::{AsyncReadResponseExt, HttpClient};
use serde::Deserialize;
use serde_json::{Map, Value};
use tracing::{debug, warn};
use url::Url;

use crate::model::ModelEntry;
use crate::provider::{BoxFuture, Provider};
use crate::types::{ContentBlock, Role, System};
use crate::{
    AgentError, Message, ProviderEvent, RequestDeliveryMetadata, RequestDeliveryPhase,
    RequestOptions, StopReason, StreamResponse, TokenUsage,
};

use super::ResolvedAuth;
use super::devin_connect::{
    CONNECT_COMPRESSED_FLAG, FrameBuffer, decode_frame_payload, encode_frame,
};
use super::devin_proto::{
    CHAT_MESSAGE_SOURCE_SYSTEM, CHAT_MESSAGE_SOURCE_TOOL, CHAT_MESSAGE_SOURCE_USER,
    ChatMessagePromptInput, ChatToolCall, ChatToolDefinition, ImageData, ModelUsageStats,
    STOP_REASON_MAX_TOKENS, STOP_REASON_TOOL_USE, STOP_REASON_UNSPECIFIED,
    decode_cli_model_configs, decode_get_chat_message_response, decode_get_user_jwt_response,
    encode_chat_message_prompt, encode_chat_tool_definition, encode_get_chat_message_request,
    encode_get_cli_model_configs_request, encode_get_user_jwt_request,
};

use n00n_config::providers::{
    Protocol, ProviderDef, ProvidersConfig, base_url_override, resolve_api_key_env,
    resolve_base_url, slugify,
};
use n00n_redact::sanitize_text;
use n00n_storage::StateDir;
use n00n_storage::auth::load_provider_credentials;
use n00n_storage::id::n00nId;

const DEVIN_API_URL: &str = "https://server.codeium.com";
const DEVIN_AUTH_PATH: &str = "/exa.auth_pb.AuthService/GetUserJwt";
const DEVIN_CHAT_PATH: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
const DEVIN_CLI_MODEL_CONFIGS_PATH: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";
const DEVIN_SESSION_TOKEN_PREFIX: &str = "devin-session-token$";
const HTTP_SCHEME: &str = "http";
const HTTPS_SCHEME: &str = "https";
const DEFAULT_TEMPERATURE: f64 = 0.4;
const DEFAULT_TOP_P: f64 = 1.0;
const DEFAULT_MAX_TOKENS: u32 = 64_000;
const MAX_TRAILER_CODE_LEN: usize = 64;
const MAX_TRAILER_MESSAGE_CHARS: usize = 240;

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "devin",
    display_name: "Devin",
    protocol: n00n_config::providers::Protocol::Devin,
    default_base_url: DEVIN_API_URL,
    default_api_key_env: "DEVIN_API_KEY",
    default_model: "devin/swe-1-7",
    plans: None,
    login_url: None,
    needs_url: false,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    crate::providers::devin_models::models()
}

#[derive(Debug, Clone)]
struct DevinCredentials {
    session_token: String,
    api_server_url: String,
}

#[derive(Deserialize)]
struct TomlCredentials {
    windsurf_api_key: Option<String>,
    api_key: Option<String>,
    api_server_url: Option<String>,
}

impl DevinCredentials {
    fn from_env() -> Result<Option<Self>, AgentError> {
        let session_token = match optional_env_token("WINDSURF_API_KEY")? {
            Some(token) => Some(token),
            None => optional_env_token("DEVIN_API_KEY")?,
        };
        Ok(session_token.map(|token| Self {
            session_token: normalize_session_token(&token),
            api_server_url: DEVIN_API_URL.to_string(),
        }))
    }

    fn from_file() -> Result<Option<Self>, AgentError> {
        let data_home = if let Some(path) = optional_env("XDG_DATA_HOME")? {
            PathBuf::from(path)
        } else {
            let Some(home) = optional_env("HOME")? else {
                return Ok(None);
            };
            PathBuf::from(home).join(".local/share")
        };
        Self::from_path(&data_home.join("devin/credentials.toml"))
    }

    fn from_path(creds_path: &Path) -> Result<Option<Self>, AgentError> {
        let content = match std::fs::read_to_string(creds_path) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AgentError::Config {
                    message: format!(
                        "failed to read Devin credentials at {}: {error}",
                        creds_path.display()
                    ),
                });
            }
        };
        let creds: TomlCredentials =
            toml::from_str(&content).map_err(|error| AgentError::Config {
                message: format!(
                    "failed to parse Devin credentials at {}: {error}",
                    creds_path.display()
                ),
            })?;
        let session_token = creds
            .windsurf_api_key
            .filter(|token| !token.trim().is_empty())
            .or_else(|| creds.api_key.filter(|token| !token.trim().is_empty()))
            .ok_or_else(|| AgentError::Config {
                message: format!(
                    "Devin credentials at {} are missing a non-empty windsurf_api_key or api_key",
                    creds_path.display()
                ),
            })?;
        Ok(Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url: resolve_api_server_url(
                DEVIN_API_URL.to_string(),
                creds.api_server_url.as_deref(),
            )?,
        }))
    }
}

fn optional_env(name: &str) -> Result<Option<String>, AgentError> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error @ std::env::VarError::NotUnicode(_)) => Err(AgentError::Config {
            message: format!("environment variable {name} is not valid Unicode: {error}"),
        }),
    }
}

fn optional_env_token(name: &str) -> Result<Option<String>, AgentError> {
    Ok(optional_env(name)?.and_then(|value| {
        value
            .split(',')
            .map(str::trim)
            .find(|token| !token.is_empty())
            .map(str::to_string)
    }))
}

/// API paths are appended to this value by string concatenation, so a query or
/// fragment would swallow the path. `Url` also normalizes the scheme, which a
/// prefix match does not: schemes are case-insensitive.
fn is_valid_api_server_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), HTTP_SCHEME | HTTPS_SCHEME)
        && parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

/// Picks the API host, preferring an operator-supplied `explicit` value.
///
/// A malformed value is an error, not a fallback. Silently substituting
/// `DEVIN_API_URL` would post the session token to the public host while the
/// operator believed it went to their own endpoint.
///
/// Only a blank candidate — no override configured, no host stored yet —
/// falls back to `DEVIN_API_URL`.
fn resolve_api_server_url(
    configured: String,
    explicit: Option<&str>,
) -> Result<String, AgentError> {
    let candidate = explicit
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map_or(configured, ToString::to_string);
    let candidate = candidate.trim().trim_end_matches('/');
    if candidate.is_empty() {
        return Ok(DEVIN_API_URL.to_string());
    }
    if !is_valid_api_server_url(candidate) {
        return Err(AgentError::Config {
            message: format!(
                "Devin base_url must be an http(s) URL with a host and no query or fragment, \
                 got {candidate:?}"
            ),
        });
    }
    Ok(candidate.to_string())
}

/// Applies a host override that Devin's own auth response supplied.
///
/// Unlike operator configuration this value is provider output, so a
/// malformed one is dropped with a warning instead of failing the login: the
/// request already reached `configured`, and staying there discloses nothing
/// new.
fn apply_provider_api_server_url(configured: &str, provider_supplied: &str) -> String {
    let candidate = provider_supplied.trim().trim_end_matches('/');
    if candidate.is_empty() {
        return configured.to_string();
    }
    if is_valid_api_server_url(candidate) {
        return candidate.to_string();
    }
    warn!(
        "Devin auth response supplied an unusable custom_api_server_url; keeping the current host"
    );
    configured.to_string()
}

fn stored_credentials(
    storage: &StateDir,
    namespace: &str,
    api_server_url: &str,
) -> Option<DevinCredentials> {
    load_provider_credentials(storage, namespace).and_then(|credentials| {
        (!credentials.api_key.trim().is_empty()).then(|| DevinCredentials {
            session_token: normalize_session_token(&credentials.api_key),
            api_server_url: api_server_url.to_string(),
        })
    })
}

/// `resolve_base_url` is an unvalidated passthrough; route it through
/// `resolve_api_server_url` so an invalid `devin.base_url` is rejected instead
/// of reaching URI construction.
fn discovered_base_url(config: &ProvidersConfig) -> Result<String, AgentError> {
    resolve_api_server_url(
        DEVIN_API_URL.to_string(),
        resolve_base_url("devin", config.get("devin")).as_deref(),
    )
}

fn discover_credentials() -> Result<Option<DevinCredentials>, AgentError> {
    let config = ProvidersConfig::load()?;
    let base_url = discovered_base_url(&config)?;
    if let Some(mut credentials) = DevinCredentials::from_env()? {
        credentials.api_server_url = base_url;
        return Ok(Some(credentials));
    }
    if let Ok(storage) = StateDir::resolve()
        && let Some(credentials) = stored_credentials(&storage, "devin", &base_url)
    {
        return Ok(Some(credentials));
    }
    let mut credentials = DevinCredentials::from_file()?;
    if let Some(credentials) = credentials.as_mut()
        && config
            .get("devin")
            .is_some_and(|definition| definition.base_url.is_some())
    {
        credentials.api_server_url = base_url;
    }
    Ok(credentials)
}
#[must_use]
pub fn legacy_account_name(slug: &str) -> Option<&str> {
    let suffix = slug.strip_prefix("devin")?;
    (!suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit()))
        .then_some(suffix)
}

fn legacy_account_definition<'a>(
    config: &'a ProvidersConfig,
    account: &str,
) -> Option<(&'a str, &'a ProviderDef)> {
    let slug = config.providers.keys().find(|slug| {
        legacy_account_name(slug).is_some_and(|legacy_account| legacy_account == account)
    })?;
    let definition = config.get(slug)?;
    (definition.protocol == Some(Protocol::Devin)).then_some((slug, definition))
}

fn is_valid_account_name(account: &str) -> bool {
    !account.is_empty() && slugify(account) == account
}

pub fn configured_account_names(config: &ProvidersConfig) -> Vec<String> {
    let mut accounts = config.get("devin").map_or_else(Vec::new, |definition| {
        definition
            .accounts
            .keys()
            .filter(|account| is_valid_account_name(account))
            .cloned()
            .collect::<Vec<_>>()
    });
    accounts.extend(config.providers.iter().filter_map(|(slug, definition)| {
        (definition.protocol == Some(Protocol::Devin))
            .then(|| legacy_account_name(slug).map(str::to_string))
            .flatten()
            .filter(|account| is_valid_account_name(account))
    }));
    accounts.sort();
    accounts.dedup();
    accounts
}

fn legacy_account_for_provider_slug_in(config: &ProvidersConfig, slug: &str) -> Option<String> {
    let account = legacy_account_name(slug)?;
    config
        .get(slug)
        .is_some_and(|definition| definition.protocol == Some(Protocol::Devin))
        .then(|| account.to_string())
}

pub(crate) fn legacy_account_for_provider_slug(slug: &str) -> Option<String> {
    legacy_account_name(slug)?;
    let config = match ProvidersConfig::load() {
        Ok(config) => config,
        Err(error) => {
            warn!(%error, slug, "cannot load provider configuration for Devin account alias");
            return None;
        }
    };
    legacy_account_for_provider_slug_in(&config, slug)
}

fn expand_home(path: &Path) -> Result<PathBuf, AgentError> {
    let Some(path_text) = path.to_str() else {
        return Err(AgentError::Config {
            message: "Devin account credential path is not valid Unicode".to_string(),
        });
    };
    if path_text == "~" || path_text.starts_with("~/") {
        let home = optional_env("HOME")?.ok_or_else(|| AgentError::Config {
            message: "HOME is required for a ~/ Devin credential path".to_string(),
        })?;
        let home = PathBuf::from(home);
        return Ok(match path_text.strip_prefix("~/") {
            Some(relative) => home.join(relative),
            None => home,
        });
    }
    Ok(path.to_path_buf())
}

fn inferred_account_credential_path(account: &str) -> Result<PathBuf, AgentError> {
    let data_home = if let Some(path) = optional_env("XDG_DATA_HOME")? {
        PathBuf::from(format!("{path}/devin{account}"))
    } else {
        let home = optional_env("HOME")?.ok_or_else(|| AgentError::Config {
            message: "HOME is required to locate Devin account credentials".to_string(),
        })?;
        PathBuf::from(home).join(format!(".local/share/devin{account}"))
    };
    Ok(data_home.join("devin/credentials.toml"))
}

fn account_api_server_url_override(
    config: &ProvidersConfig,
    legacy: Option<(&str, &ProviderDef)>,
) -> Option<String> {
    legacy
        .and_then(|(slug, definition)| {
            base_url_override(slug).or_else(|| definition.base_url.clone())
        })
        .or_else(|| base_url_override("devin"))
        .or_else(|| {
            config
                .get("devin")
                .and_then(|definition| definition.base_url.clone())
        })
}

fn apply_account_url_override(
    mut credentials: DevinCredentials,
    account_url_override: Option<&str>,
) -> Result<DevinCredentials, AgentError> {
    credentials.api_server_url =
        resolve_api_server_url(credentials.api_server_url, account_url_override)?;
    Ok(credentials)
}

fn load_explicit_account_credentials(
    path: &Path,
    account_url_override: Option<&str>,
) -> Result<Option<DevinCredentials>, AgentError> {
    DevinCredentials::from_path(path)?
        .map(|credentials| apply_account_url_override(credentials, account_url_override))
        .transpose()
}

fn discover_account_credentials(account: &str) -> Result<Option<DevinCredentials>, AgentError> {
    if !is_valid_account_name(account) {
        return Ok(None);
    }
    let config = ProvidersConfig::load()?;
    let legacy = legacy_account_definition(&config, account);
    let account_url_override = account_api_server_url_override(&config, legacy);
    let account_base_url =
        resolve_api_server_url(DEVIN_API_URL.to_string(), account_url_override.as_deref())?;

    let explicit_path = config
        .get("devin")
        .and_then(|definition| definition.accounts.get(account))
        .and_then(|definition| definition.credential_path.as_deref());
    if let Some(path) = explicit_path {
        return load_explicit_account_credentials(
            &expand_home(path)?,
            account_url_override.as_deref(),
        );
    }

    if let Some((slug, definition)) = legacy {
        let env_name = resolve_api_key_env(slug, Some(definition));
        if let Some(token) = optional_env_token(&env_name)? {
            return Ok(Some(DevinCredentials {
                session_token: normalize_session_token(&token),
                api_server_url: account_base_url,
            }));
        }
    }

    if let Ok(storage) = StateDir::resolve() {
        if let Some(credentials) =
            stored_credentials(&storage, &format!("devin@{account}"), &account_base_url)
        {
            return Ok(Some(credentials));
        }
        if let Some((slug, _)) = legacy
            && let Some(credentials) = stored_credentials(&storage, slug, &account_base_url)
        {
            return Ok(Some(credentials));
        }
    }

    if let Some((_, definition)) = legacy
        && let Some(token) = definition
            .api_key
            .as_deref()
            .filter(|token| !token.trim().is_empty())
    {
        return Ok(Some(DevinCredentials {
            session_token: normalize_session_token(token),
            api_server_url: account_base_url,
        }));
    }

    if account.chars().all(|character| character.is_ascii_digit())
        && let Some(credentials) =
            DevinCredentials::from_path(&inferred_account_credential_path(account)?)?
    {
        return Ok(Some(apply_account_url_override(
            credentials,
            account_url_override.as_deref(),
        )?));
    }
    Ok(None)
}

fn account_and_model(model_id: &str) -> (Option<&str>, &str) {
    let Some((account, model)) = model_id.split_once("::") else {
        return (None, model_id);
    };
    if is_valid_account_name(account) && !model.is_empty() {
        (Some(account), model)
    } else {
        (None, model_id)
    }
}

pub(crate) fn model_id_without_account(model_id: &str) -> &str {
    account_and_model(model_id).1
}

#[must_use]
pub fn account_has_credentials(account: &str) -> bool {
    discover_account_credentials(account).is_ok_and(|credentials| credentials.is_some())
}

fn normalize_session_token(token: &str) -> String {
    if token.starts_with(DEVIN_SESSION_TOKEN_PREFIX) {
        token.to_string()
    } else {
        format!("{DEVIN_SESSION_TOKEN_PREFIX}{token}")
    }
}

fn chat_message_id(cascade_id: &str, message_index: usize, role: &str) -> String {
    if role == "assistant" {
        format!("bot-{cascade_id}-{message_index}-{role}")
    } else {
        format!("{cascade_id}-{message_index}-{role}")
    }
}

fn max_tokens_for_model(max_output_tokens: Option<u32>) -> u64 {
    u64::from(match max_output_tokens {
        Some(max_output_tokens) => max_output_tokens,
        None => DEFAULT_MAX_TOKENS,
    })
}

fn clamp_tokens(field: &'static str, value: u64) -> u32 {
    if let Ok(value) = u32::try_from(value) {
        value
    } else {
        warn!(
            field,
            value, "Devin usage token count out of range; clamping"
        );
        u32::MAX
    }
}

fn devin_usage_to_token_usage(u: &ModelUsageStats) -> TokenUsage {
    // Devin gRPC usage reports input_tokens as the total prompt tokens
    // (including cache reads and writes), with cache fields as details.
    // TokenUsage.input must be the non-cached portion so that total_input()
    // and cost() are consistent with the rest of the providers. Some
    // responses already report input_tokens as the non-cached remainder;
    // keep that reported value instead of saturating to zero.
    let cached = u.cache_read_tokens.saturating_add(u.cache_write_tokens);
    let (input, cache_read, cache_creation) = if u.input_tokens >= cached {
        (
            u.input_tokens.saturating_sub(cached),
            u.cache_read_tokens,
            u.cache_write_tokens,
        )
    } else {
        debug!(
            input_tokens = u.input_tokens,
            cache_read_tokens = u.cache_read_tokens,
            cache_write_tokens = u.cache_write_tokens,
            "Devin input_tokens is less than cached tokens; treating as non-cached"
        );
        (u.input_tokens, u.cache_read_tokens, u.cache_write_tokens)
    };
    TokenUsage {
        input: clamp_tokens("input", input),
        output: clamp_tokens("output", u.output_tokens),
        cache_creation: clamp_tokens("cache_write", cache_creation),
        cache_read: clamp_tokens("cache_read", cache_read),
    }
}

fn sanitize_trailer_code(code: &str) -> &str {
    if !code.is_empty()
        && code.len() <= MAX_TRAILER_CODE_LEN
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        code
    } else {
        "invalid"
    }
}

/// Maps a Devin stream read failure to a retryable `AgentError::Io`.
async fn read_stream_chunk(
    reader: &mut (impl futures_lite::io::AsyncRead + Unpin),
    buffer: &mut [u8],
) -> Result<usize, AgentError> {
    reader.read(buffer).await.map_err(AgentError::Io)
}

fn map_chat_send_error(error: isahc::Error) -> AgentError {
    if matches!(
        error.kind(),
        HttpErrorKind::BadClientCertificate
            | HttpErrorKind::BadServerCertificate
            | HttpErrorKind::ClientInitialization
            | HttpErrorKind::ConnectionFailed
            | HttpErrorKind::InvalidRequest
            | HttpErrorKind::NameResolution
            | HttpErrorKind::TlsEngine
    ) {
        AgentError::Http(error)
    } else {
        AgentError::RequestSent {
            message: format!("Devin chat transport failed: {error}"),
            metadata: Some(RequestDeliveryMetadata::new(
                RequestDeliveryPhase::SentAwaitingAcceptance,
            )),
        }
    }
}

fn accepted_stream_error(error: &AgentError, emitted_event: bool) -> AgentError {
    let detail = if error.is_server_overloaded() {
        match error {
            AgentError::Api { status, .. } => {
                format!("API error ({status}): provider reported overload")
            }
            _ => "provider reported overload".to_string(),
        }
    } else {
        error.to_string()
    };
    let mut metadata = RequestDeliveryMetadata::new(RequestDeliveryPhase::Accepted);
    metadata.emitted_event = emitted_event;
    AgentError::RequestSent {
        message: format!("Devin request failed after acceptance: {detail}"),
        metadata: Some(metadata),
    }
}

fn accepted_protocol_error(error: AgentError, emitted_event: bool) -> AgentError {
    if emitted_event || error.is_retryable() {
        accepted_stream_error(&error, emitted_event)
    } else {
        error
    }
}

fn connect_code_status(code: &str) -> u16 {
    match code {
        "invalid_argument" | "failed_precondition" | "out_of_range" => 400,
        "unauthenticated" => 401,
        "permission_denied" => 403,
        "not_found" => 404,
        "already_exists" | "aborted" => 409,
        "resource_exhausted" => 429,
        "canceled" => 499,
        "unimplemented" => 501,
        "unavailable" => 503,
        "deadline_exceeded" => 504,
        _ => 500,
    }
}

fn parse_devin_trailer(payload: &[u8]) -> Result<Option<String>, AgentError> {
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Err(AgentError::api(502, "empty Devin end-stream trailer"));
    }
    let trailer = std::str::from_utf8(payload).map_err(|_| AgentError::Api {
        status: 502,
        message: "invalid Devin end-stream trailer encoding".to_string(),
    })?;
    let value: Value = serde_json::from_str(trailer).map_err(|_| AgentError::Api {
        status: 502,
        message: "invalid Devin end-stream trailer JSON".to_string(),
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| AgentError::api(502, "Devin end-stream trailer must be a JSON object"))?;
    let error = object.get("error");
    let code_value = match error {
        Some(error) => error.get("code"),
        None => object.get("code"),
    };
    let code = match code_value {
        Some(value) => Some(value.as_str().ok_or_else(|| {
            AgentError::api(502, "Devin end-stream trailer code must be a string")
        })?),
        None => None,
    }
    .map(sanitize_trailer_code);
    match code {
        Some("ok") if error.is_none() => Ok(Some("ok".to_string())),
        None if error.is_none() => Ok(None),
        None => Err(AgentError::api(
            502,
            "Devin end-stream trailer contained an error without a valid code",
        )),
        Some("ok") => Err(AgentError::api(
            502,
            "Devin end-stream trailer contained contradictory error status",
        )),
        Some(code) => {
            let detail = value
                .get("error")
                .and_then(|error| error.get("message"))
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(|message| sanitize_text(message, MAX_TRAILER_MESSAGE_CHARS))
                .filter(|message| !message.is_empty());
            let message = detail.map_or_else(
                || format!("Devin stream failed with trailer code {code}"),
                |detail| format!("Devin stream failed: {detail} (code {code})"),
            );
            Err(AgentError::api(connect_code_status(code), message))
        }
    }
}

fn encode_devin_tools(tools: &Value) -> Result<Vec<Vec<u8>>, AgentError> {
    let arr = tools.as_array().ok_or_else(|| AgentError::Config {
        message: "Devin tools must be an array".to_string(),
    })?;
    let mut encoded = Vec::with_capacity(arr.len());
    for tool in arr {
        let function = match tool.get("function") {
            Some(v) => v,
            None => tool,
        };
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::Config {
                message: "tool missing name".to_string(),
            })?;
        let description = function.get("description").and_then(Value::as_str);
        let schema_string = match function.get("input_schema") {
            Some(v) => serde_json::to_string(v).map_err(|e| AgentError::Config {
                message: format!("failed to serialize tool schema: {e}"),
            })?,
            None => "{}".to_string(),
        };
        let strict = tool
            .get("strict")
            .or_else(|| function.get("strict"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| false);
        encoded.push(encode_chat_tool_definition(&ChatToolDefinition {
            name: name.to_string(),
            description: description.map_or(String::new(), std::string::ToString::to_string),
            json_schema_string: schema_string.clone(),
            strict,
        }));
    }
    Ok(encoded)
}

fn merge_tool_call(
    tool_calls: &mut HashMap<String, (String, String)>,
    tool_call: ChatToolCall,
) -> bool {
    if let Some((name, arguments_json)) = tool_calls.get_mut(&tool_call.id) {
        if !tool_call.name.is_empty() {
            *name = tool_call.name;
        }
        arguments_json.push_str(&tool_call.arguments_json);
        false
    } else {
        tool_calls.insert(tool_call.id, (tool_call.name, tool_call.arguments_json));
        true
    }
}

fn ordered_tool_call_blocks(
    mut tool_calls: HashMap<String, (String, String)>,
    tool_call_order: Vec<String>,
) -> Result<Vec<ContentBlock>, AgentError> {
    let mut blocks = Vec::with_capacity(tool_call_order.len());
    for id in tool_call_order {
        let (name, arguments_json) = tool_calls.remove(&id).ok_or_else(|| AgentError::Api {
            status: 0,
            message: "Devin tool-call ordering state is inconsistent".to_string(),
        })?;
        let input = if arguments_json.is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&arguments_json).map_err(|error| AgentError::Api {
                status: 0,
                message: format!("invalid Devin tool arguments for {name}: {error}"),
            })?
        };
        blocks.push(ContentBlock::ToolUse { id, name, input });
    }
    Ok(blocks)
}

fn encode_devin_chat_message_prompts(
    messages: &[Message],
    cascade_id: &str,
) -> Result<Vec<Vec<u8>>, AgentError> {
    let mut prompts = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::User => {
                let mut prompt_text = String::new();
                let mut images = Vec::new();
                let mut user_part = 0usize;
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => prompt_text.push_str(text),
                        ContentBlock::Image { source } => images.push(ImageData {
                            base64_data: source.data.to_string(),
                            mime_type: source.media_type.mime().to_string(),
                            caption: String::new(),
                        }),
                        ContentBlock::File { source } => {
                            let identifier = source.identifier().unwrap_or_else(|| "unknown");
                            prompt_text.push_str("[file omitted: ");
                            prompt_text.push_str(identifier);
                            prompt_text.push(']');
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            if !prompt_text.is_empty() || !images.is_empty() {
                                prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                    message_id: &chat_message_id(
                                        cascade_id,
                                        index,
                                        &format!("user-{user_part}"),
                                    ),
                                    source: CHAT_MESSAGE_SOURCE_USER,
                                    prompt: &prompt_text,
                                    images: &images,
                                    ..ChatMessagePromptInput::default()
                                }));
                                prompt_text.clear();
                                images.clear();
                                user_part += 1;
                            }
                            prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                message_id: &chat_message_id(
                                    cascade_id,
                                    index,
                                    &format!("tool-{tool_use_id}"),
                                ),
                                source: CHAT_MESSAGE_SOURCE_TOOL,
                                prompt: content,
                                tool_call_id: tool_use_id,
                                tool_result_is_error: *is_error,
                                ..ChatMessagePromptInput::default()
                            }));
                        }
                        _ => {}
                    }
                }
                if !prompt_text.is_empty() || !images.is_empty() {
                    prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                        message_id: &chat_message_id(
                            cascade_id,
                            index,
                            &format!("user-{user_part}"),
                        ),
                        source: CHAT_MESSAGE_SOURCE_USER,
                        prompt: &prompt_text,
                        images: &images,
                        ..ChatMessagePromptInput::default()
                    }));
                }
            }
            Role::Assistant => {
                let mut prompt_text = String::new();
                let mut thinking = String::new();
                let mut signature = String::new();
                let mut tool_calls = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => prompt_text.push_str(text),
                        ContentBlock::Thinking {
                            thinking: t,
                            signature: sig,
                        } => {
                            thinking.push_str(t);
                            if signature.is_empty()
                                && let Some(s) = sig
                            {
                                signature.clone_from(s);
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let arguments_json = serde_json::to_string(input).map_err(|error| {
                                AgentError::Config {
                                    message: format!(
                                        "failed to serialize Devin tool arguments: {error}"
                                    ),
                                }
                            })?;
                            tool_calls.push(ChatToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                arguments_json,
                            });
                        }
                        _ => {}
                    }
                }
                if !prompt_text.is_empty()
                    || !thinking.is_empty()
                    || !signature.is_empty()
                    || !tool_calls.is_empty()
                {
                    prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                        message_id: &chat_message_id(cascade_id, index, "assistant"),
                        source: CHAT_MESSAGE_SOURCE_SYSTEM,
                        prompt: &prompt_text,
                        thinking: &thinking,
                        signature: &signature,
                        tool_calls: &tool_calls,
                        ..ChatMessagePromptInput::default()
                    }));
                }
            }
        }
    }
    Ok(prompts)
}

pub struct Devin {
    credentials: Option<DevinCredentials>,
    client: HttpClient,
    client_model_configs: Mutex<HashMap<Option<String>, HashMap<String, String>>>,
    timeouts: super::Timeouts,
}

/// Guards the warning below. Provider listing runs often, so an
/// unconditional warning would repeat on every refresh.
static REPORTED_UNUSABLE_PRIMARY_CONFIG: AtomicBool = AtomicBool::new(false);

#[must_use]
pub fn has_primary_credentials() -> bool {
    match discover_credentials() {
        Ok(credentials) => credentials.is_some(),
        // A broken `devin.base_url` must not read as "not configured". The
        // error still stops the turn when a Devin model is picked, but the
        // listing is where a user looks first, so say so once here too.
        Err(error) => {
            if !REPORTED_UNUSABLE_PRIMARY_CONFIG.swap(true, Ordering::Relaxed) {
                warn!("Devin is configured but unusable: {error}");
            }
            false
        }
    }
}

pub(crate) fn has_credentials() -> bool {
    if has_primary_credentials() {
        return true;
    }
    let Ok(config) = ProvidersConfig::load() else {
        return false;
    };
    configured_account_names(&config)
        .iter()
        .any(|account| account_has_credentials(account))
}

impl Devin {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        let credentials = match discover_credentials()? {
            Some(mut credentials) => {
                credentials.api_server_url =
                    resolve_api_server_url(credentials.api_server_url, None)?;
                Some(credentials)
            }
            None => None,
        };
        Ok(Self {
            credentials,
            client: super::http_client(timeouts)?,
            client_model_configs: Mutex::new(HashMap::new()),
            timeouts,
        })
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn with_auth(
        auth: &Arc<Mutex<ResolvedAuth>>,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        let resolved = match auth.lock() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };

        let resolved_base_url = resolved.base_url.clone();

        let session_token = resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_session_token);

        let credentials = match session_token {
            Some(token) => Some(DevinCredentials {
                session_token: token,
                api_server_url: DEVIN_API_URL.to_string(),
            }),
            None => discover_credentials()?,
        };
        let credentials = match credentials {
            Some(mut credentials) => {
                credentials.api_server_url = resolve_api_server_url(
                    credentials.api_server_url,
                    resolved_base_url.as_deref(),
                )?;
                Some(credentials)
            }
            None => None,
        };

        Ok(Self {
            credentials,
            client: super::http_client(timeouts)?,
            client_model_configs: Mutex::new(HashMap::new()),
            timeouts,
        })
    }

    fn http_client(&self) -> &HttpClient {
        &self.client
    }

    async fn get_user_jwt(
        &self,
        credentials: &DevinCredentials,
    ) -> Result<(String, String), AgentError> {
        let request_bytes = encode_get_user_jwt_request(&credentials.session_token);

        let url = format!("{}{}", credentials.api_server_url, DEVIN_AUTH_PATH);

        let request = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build auth request: {e}"),
            })?;
        let mut response = self
            .http_client()
            .send_async(request)
            .await
            .map_err(AgentError::Http)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::api(status, format!("auth failed: {body}")));
        }

        let response_bytes = response.bytes().await.map_err(AgentError::Io)?;

        let auth_response =
            decode_get_user_jwt_response(&response_bytes).map_err(|e| AgentError::Api {
                status: 502,
                message: format!("failed to decode auth response: {e}"),
            })?;

        if auth_response.user_jwt.is_empty() {
            return Err(AgentError::Api {
                status: 502,
                message: "auth response missing user_jwt".to_string(),
            });
        }

        let base_url = apply_provider_api_server_url(
            &credentials.api_server_url,
            &auth_response.custom_api_server_url,
        );

        Ok((auth_response.user_jwt, base_url))
    }

    async fn get_cli_model_configs(
        &self,
        credentials: &DevinCredentials,
        account: Option<&str>,
        base_url: &str,
    ) -> Result<HashMap<String, String>, AgentError> {
        let cache_key = account.map(str::to_string);
        if let Ok(guard) = self.client_model_configs.lock()
            && let Some(cache) = guard.get(&cache_key)
        {
            return Ok(cache.clone());
        }

        let request_bytes = encode_get_cli_model_configs_request(&credentials.session_token);

        let url = format!("{base_url}{DEVIN_CLI_MODEL_CONFIGS_PATH}");
        let request = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build model configs request: {e}"),
            })?;
        let mut response = self
            .http_client()
            .send_async(request)
            .await
            .map_err(AgentError::Http)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::api(
                status,
                format!("model configs failed: {body}"),
            ));
        }

        let response_bytes = response.bytes().await.map_err(AgentError::Io)?;

        let configs = decode_cli_model_configs(&response_bytes).map_err(|e| AgentError::Api {
            status: 502,
            message: format!("failed to decode model configs response: {e}"),
        })?;

        if let Ok(mut guard) = self.client_model_configs.lock() {
            guard.insert(cache_key, configs.clone());
        }

        Ok(configs)
    }

    async fn stream_chat_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
    ) -> Result<StreamResponse, AgentError> {
        // Devin cannot express thinking, fast-mode, or cache/history replay options.
        let _ = opts;
        let (account, model_router_uid) = if model.provider.as_ref() == "devin" {
            account_and_model(&model.id)
        } else {
            (None, model.id.as_str())
        };
        let account_credentials = match account {
            Some(account) => Some(
                discover_account_credentials(account)?.ok_or_else(|| AgentError::SetupRequired {
                    message: format!(
                        "no credentials found for Devin account '{account}'; configure [devin.accounts.{account}].credential_path or run `n00n auth login devin@{account}`"
                    ),
                })?,
            ),
            None => None,
        };
        let credentials = match account_credentials.as_ref() {
            Some(credentials) => credentials,
            None => self
                .credentials
                .as_ref()
                .ok_or_else(|| AgentError::SetupRequired {
                    message: "no Devin credentials found".to_string(),
                })?,
        };
        let (user_jwt, base_url) = self.get_user_jwt(credentials).await?;
        // Resolve aliases (e.g. "opus") to the canonical model uid before
        // looking up the server-side wire id.
        let canonical_id =
            crate::model::lookup_entry(crate::providers::devin::models(), model_router_uid)
                .map_or(model_router_uid, |entry| entry.prefixes[0]);
        let cli_configs = self
            .get_cli_model_configs(credentials, account, &base_url)
            .await?;
        let chat_model_uid = cli_configs
            .get(canonical_id)
            .map_or(canonical_id, |wire| wire.as_str());

        let cascade_id = n00nId::generate().to_string();
        let execution_id = n00nId::generate().to_string();

        let prompt = system
            .blocks()
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        let chat_message_prompts = encode_devin_chat_message_prompts(messages, &cascade_id)?;
        let chat_tools = encode_devin_tools(tools)?;

        let max_tokens = max_tokens_for_model(model.max_output_tokens);
        let request_bytes = encode_get_chat_message_request(
            &credentials.session_token,
            &user_jwt,
            &prompt,
            chat_model_uid,
            &cascade_id,
            &execution_id,
            &chat_message_prompts,
            &chat_tools,
            max_tokens,
            DEFAULT_TEMPERATURE,
            DEFAULT_TOP_P,
        );

        let gzipped = {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(&request_bytes)
                .map_err(|e| AgentError::Config {
                    message: format!("gzip compression failed: {e}"),
                })?;
            encoder.finish().map_err(|e| AgentError::Config {
                message: format!("gzip finish failed: {e}"),
            })?
        };

        let frame =
            encode_frame(CONNECT_COMPRESSED_FLAG, &gzipped).map_err(|e| AgentError::Config {
                message: format!("failed to encode connect frame: {e}"),
            })?;

        let url = format!("{base_url}{DEVIN_CHAT_PATH}");

        let request = isahc::Request::post(&url)
            .header("content-type", "application/connect+proto")
            .header("connect-protocol-version", "1")
            .header("connect-content-encoding", "gzip")
            .header("accept-encoding", "identity")
            .header("user-agent", "connect-go/1.18.1 (go1.26.3)")
            .header("connect-accept-encoding", "gzip")
            .body(frame)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build chat request: {e}"),
            })?;
        let mut response = self
            .http_client()
            .send_async(request)
            .await
            .map_err(map_chat_send_error)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::api(status, format!("chat failed: {body}")));
        }

        let mut reader = BufReader::new(response.into_body());

        let mut frame_buffer = FrameBuffer::default();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut signature = String::new();
        let mut usage = TokenUsage::default();
        let mut stream_deadline = Instant::now() + self.timeouts.stream;
        let mut stop_reason = StopReason::EndTurn;
        let mut tool_calls: HashMap<String, (String, String)> = HashMap::new();
        let mut tool_call_order = Vec::new();
        let mut emitted_event = false;

        let mut buffer = vec![0u8; 8192];

        'stream: loop {
            let n = futures_lite::future::or(read_stream_chunk(&mut reader, &mut buffer), async {
                smol::Timer::after(stream_deadline.saturating_duration_since(Instant::now())).await;
                Err(AgentError::Timeout {
                    secs: self.timeouts.stream.as_secs(),
                })
            })
            .await
            .map_err(|error| accepted_stream_error(&error, emitted_event))?;

            if n == 0 {
                let message = if frame_buffer.is_empty() {
                    "Devin stream ended before the end-stream trailer"
                } else {
                    "truncated Devin Connect frame at end of stream"
                };
                return Err(accepted_stream_error(
                    &AgentError::api(502, message),
                    emitted_event,
                ));
            }
            stream_deadline = Instant::now() + self.timeouts.stream;

            frame_buffer.push(&buffer[..n]);
            let mut successful_trailer = false;

            while let Some(frame_result) = frame_buffer.next_frame() {
                let frame = frame_result.map_err(|error| {
                    accepted_stream_error(
                        &AgentError::api(502, format!("invalid connect frame: {error}")),
                        emitted_event,
                    )
                })?;

                if successful_trailer && !frame.end_stream {
                    return Err(accepted_stream_error(
                        &AgentError::api(502, "data followed Devin end-stream trailer"),
                        emitted_event,
                    ));
                }

                if frame.end_stream {
                    let payload = decode_frame_payload(&frame).map_err(|error| {
                        accepted_stream_error(
                            &AgentError::api(502, format!("failed to decode trailer: {error}")),
                            emitted_event,
                        )
                    })?;
                    if let Some(code) = parse_devin_trailer(&payload)
                        .map_err(|error| accepted_protocol_error(error, emitted_event))?
                    {
                        debug!(
                            trailer_code = code,
                            trailer_bytes = payload.len(),
                            "Devin end-stream trailer received"
                        );
                    } else {
                        debug!(
                            trailer_bytes = payload.len(),
                            "Devin end-stream trailer received"
                        );
                    }
                    successful_trailer = true;
                    continue;
                }

                let payload = decode_frame_payload(&frame).map_err(|error| {
                    accepted_stream_error(
                        &AgentError::api(502, format!("failed to decode frame payload: {error}")),
                        emitted_event,
                    )
                })?;

                let response = decode_get_chat_message_response(&payload).map_err(|error| {
                    accepted_stream_error(
                        &AgentError::api(502, format!("failed to decode chat response: {error}")),
                        emitted_event,
                    )
                })?;

                if !response.delta_text.is_empty() {
                    let delta = response.delta_text;
                    text.push_str(&delta);
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text: delta })
                        .await
                        .map_err(|_| {
                            debug!("Devin event receiver closed; ending stream");
                            AgentError::Channel
                        })?;
                    emitted_event = true;
                }

                if !response.delta_thinking.is_empty() {
                    let delta = response.delta_thinking;
                    thinking.push_str(&delta);
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text: delta })
                        .await
                        .map_err(|_| {
                            debug!("Devin event receiver closed; ending stream");
                            AgentError::Channel
                        })?;
                    emitted_event = true;
                }
                signature.push_str(&response.delta_signature);

                for tc in response.delta_tool_calls {
                    let id = tc.id.clone();
                    let name = tc.name.clone();
                    if merge_tool_call(&mut tool_calls, tc) {
                        tool_call_order.push(id.clone());
                        event_tx
                            .send_async(ProviderEvent::ToolUseStart { id, name })
                            .await
                            .map_err(|_| {
                                debug!("Devin event receiver closed; ending stream");
                                AgentError::Channel
                            })?;
                        emitted_event = true;
                    }
                }

                if response.stop_reason != STOP_REASON_UNSPECIFIED {
                    stop_reason = match response.stop_reason {
                        STOP_REASON_MAX_TOKENS => StopReason::MaxTokens,
                        STOP_REASON_TOOL_USE => StopReason::ToolUse,
                        unknown => {
                            debug!(stop_reason = unknown, "unknown Devin stop reason");
                            StopReason::EndTurn
                        }
                    };
                }

                if let Some(u) = response.usage {
                    usage = devin_usage_to_token_usage(&u);
                }
            }

            if successful_trailer {
                break 'stream;
            }
        }

        let mut content_blocks = Vec::new();
        if !thinking.is_empty() || !signature.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Thinking {
                thinking,
                signature: (!signature.is_empty()).then_some(signature),
            });
        }
        if !text.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text { text });
        }
        content_blocks.extend(ordered_tool_call_blocks(tool_calls, tool_call_order)?);
        if content_blocks.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text {
                text: String::new(),
            });
        }

        let message = Message {
            role: Role::Assistant,
            content: content_blocks,
            display_text: None,
            control: false,
        };

        Ok(StreamResponse {
            message,
            usage,
            stop_reason: Some(stop_reason),
        })
    }
}

impl Provider for Devin {
    fn stream_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a n00n_storage::id::SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            self.stream_chat_message(model, messages, system, tools, event_tx, opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async move {
            let models = models()
                .iter()
                .map(|e| crate::model::ModelInfo {
                    id: e.prefixes[0].to_string(),
                    name: None,
                    context_window: Some(e.context_window),
                    max_output_tokens: Some(e.max_output_tokens),
                    pricing: Some(e.pricing),
                    supports_thinking: None,
                    supports_vision: Some(e.vision),
                    supports_files: None,
                    tier: Some(e.tier),
                    is_free: None,
                    is_promo: None,
                    provider_info: None,
                })
                .collect();
            Ok(models)
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_storage::auth::{ProviderCredentials, save_provider_credentials};
    use prost::Message as ProstMessage;

    struct FailingReader;

    impl futures_lite::io::AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                ErrorKind::ConnectionReset,
                "connection reset",
            )))
        }
    }

    #[test]
    fn read_stream_chunk_maps_io_error_to_retryable_agent_error() {
        smol::block_on(async {
            let mut reader = FailingReader;
            let mut buffer = [0u8; 8];
            let err = read_stream_chunk(&mut reader, &mut buffer)
                .await
                .unwrap_err();
            assert!(err.is_retryable());
            assert!(matches!(err, AgentError::Io(_)));
        });
    }

    #[test]
    fn known_preconnect_chat_failure_remains_retryable() {
        let error = isahc::Error::from(std::io::Error::new(
            ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let error = map_chat_send_error(error);

        assert!(error.is_retryable());
        assert!(matches!(error, AgentError::Http(_)));
    }

    #[test]
    fn ambiguous_chat_send_io_failure_requires_explicit_replay() {
        let error = map_chat_send_error(isahc::Error::from(HttpErrorKind::Io));

        assert!(!error.is_retryable());
        assert!(matches!(
            error,
            AgentError::RequestSent {
                metadata: Some(RequestDeliveryMetadata {
                    phase: RequestDeliveryPhase::SentAwaitingAcceptance,
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn accepted_stream_failures_require_explicit_replay() {
        let error = accepted_stream_error(
            &AgentError::Io(std::io::Error::new(
                ErrorKind::ConnectionReset,
                "connection reset",
            )),
            false,
        );

        assert!(!error.is_retryable());
        assert!(matches!(
            error,
            AgentError::RequestSent {
                ref message,
                metadata: Some(RequestDeliveryMetadata {
                    phase: RequestDeliveryPhase::Accepted,
                    emitted_event: false,
                    ..
                }),
            } if message.contains("connection reset")
        ));
    }

    #[test]
    fn accepted_overload_failure_is_not_retryable() {
        let error = accepted_stream_error(&AgentError::api(503, "server_is_overloaded"), false);

        assert!(!error.is_retryable());
        assert!(matches!(
            error,
            AgentError::RequestSent { ref message, .. }
                if message.contains("provider reported overload")
                    && !message.contains("server_is_overloaded")
        ));
    }

    #[test]
    fn accepted_server_and_protocol_failures_require_explicit_replay() {
        let error = accepted_protocol_error(
            AgentError::api(
                403,
                "Devin stream failed with trailer code permission_denied",
            ),
            true,
        );

        assert!(!error.is_retryable());
        assert!(matches!(
            error,
            AgentError::RequestSent {
                metadata: Some(RequestDeliveryMetadata {
                    phase: RequestDeliveryPhase::Accepted,
                    emitted_event: true,
                    ..
                }),
                ..
            }
        ));

        let rejection = accepted_protocol_error(
            AgentError::api(401, "Devin stream failed with trailer code unauthenticated"),
            false,
        );
        assert!(matches!(rejection, AgentError::Api { status: 401, .. }));
        assert!(rejection.is_auth_error());
    }

    #[test]
    fn stored_primary_credentials_are_available_to_builtin_devin() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let storage = StateDir::from_path(temp_dir.path().to_path_buf());
        save_provider_credentials(
            &storage,
            "devin",
            &ProviderCredentials {
                api_key: "stored-primary-token".to_string(),
                host: None,
            },
        )
        .expect("save credentials");

        let credentials = stored_credentials(&storage, "devin", DEVIN_API_URL)
            .expect("stored credentials present");
        assert_eq!(
            credentials.session_token,
            "devin-session-token$stored-primary-token"
        );
        assert_eq!(credentials.api_server_url, DEVIN_API_URL);
    }

    #[test]
    fn normalize_session_token_adds_prefix() {
        assert_eq!(
            normalize_session_token("abc123"),
            "devin-session-token$abc123"
        );
    }

    #[test]
    fn normalize_session_token_preserves_prefix() {
        assert_eq!(
            normalize_session_token("devin-session-token$abc123"),
            "devin-session-token$abc123"
        );
    }

    #[test]
    fn devin_usage_maps_total_input_to_non_cached() {
        let stats = ModelUsageStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 10,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 80);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, 10);
        assert_eq!(usage.cache_creation, 10);
        assert_eq!(usage.total_input(), 100);
    }

    #[test]
    fn devin_usage_preserves_input_when_cache_exceeds_total() {
        // Some responses report input_tokens as the non-cached remainder.
        // Keep the reported value instead of saturating to zero.
        let stats = ModelUsageStats {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 100,
            cache_write_tokens: 50,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 10);
        assert_eq!(usage.cache_read, 100);
        assert_eq!(usage.cache_creation, 50);
        assert_eq!(usage.total_input(), 160);
    }

    #[test]
    fn devin_usage_handles_cache_equal_to_total_input() {
        let stats = ModelUsageStats {
            input_tokens: 50,
            output_tokens: 10,
            cache_read_tokens: 30,
            cache_write_tokens: 20,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 0);
        assert_eq!(usage.cache_read, 30);
        assert_eq!(usage.cache_creation, 20);
        assert_eq!(usage.total_input(), 50);
    }

    #[test]
    fn devin_usage_with_no_cache() {
        let stats = ModelUsageStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.cache_creation, 0);
        assert_eq!(usage.total_input(), 100);
    }

    #[test]
    fn encode_devin_tools_uses_input_schema() {
        let tools = serde_json::json!([{
            "name": "read",
            "description": "Read a file",
            "input_schema": {"type": "object"}
        }]);

        let encoded = encode_devin_tools(&tools).expect("encode tools");
        assert_eq!(encoded.len(), 1);
        assert!(
            encoded[0]
                .windows(br#"{"type":"object"}"#.len())
                .any(|window| window == br#"{"type":"object"}"#)
        );
    }

    #[test]
    fn merge_tool_call_appends_argument_deltas() {
        let mut tool_calls = HashMap::new();
        assert!(merge_tool_call(
            &mut tool_calls,
            ChatToolCall {
                id: "call-1".to_string(),
                name: "read".to_string(),
                arguments_json: "{\"path\":\"".to_string(),
            },
        ));
        assert!(!merge_tool_call(
            &mut tool_calls,
            ChatToolCall {
                id: "call-1".to_string(),
                name: String::new(),
                arguments_json: "src/lib.rs\"}".to_string(),
            },
        ));

        assert_eq!(
            tool_calls.get("call-1"),
            Some(&(
                String::from("read"),
                String::from("{\"path\":\"src/lib.rs\"}")
            ))
        );
    }

    const CASCADE_ID: &str = "cascade-1";
    const TRAILER_JSON_ERROR: &str = "invalid Devin end-stream trailer JSON";

    fn prompt_string_field(prompt: &[u8], field_number: u64) -> Option<String> {
        let msg = crate::providers::devin_proto::ChatMessagePrompt::decode(prompt).ok()?;
        match field_number {
            1 => Some(msg.message_id),
            3 => Some(msg.prompt),
            7 => Some(msg.tool_call_id),
            _ => None,
        }
    }

    #[test]
    fn credentials_file_distinguishes_absent_unreadable_and_malformed() {
        let temp_dir = tempfile::tempdir().expect("temp directory");
        let absent = temp_dir.path().join("absent.toml");
        assert!(
            DevinCredentials::from_path(&absent)
                .expect("absent credentials are optional")
                .is_none()
        );

        let current_schema = temp_dir.path().join("current.toml");
        std::fs::write(
            &current_schema,
            "windsurf_api_key = \"   \"\napi_key = \"current-token\"",
        )
        .expect("write current credentials");
        let credentials = DevinCredentials::from_path(&current_schema)
            .expect("parse current credentials")
            .expect("credentials present");
        assert_eq!(
            credentials.session_token,
            "devin-session-token$current-token"
        );

        let malformed = temp_dir.path().join("malformed.toml");
        std::fs::write(&malformed, "windsurf_api_key = [").expect("write malformed credentials");
        assert!(matches!(
            DevinCredentials::from_path(&malformed),
            Err(AgentError::Config { message }) if message.contains("failed to parse Devin credentials")
        ));

        assert!(matches!(
            DevinCredentials::from_path(temp_dir.path()),
            Err(AgentError::Config { message }) if message.contains("failed to read Devin credentials")
        ));
    }

    #[test]
    fn explicit_account_credential_path_is_authoritative() {
        let temp_dir = tempfile::tempdir().expect("temp directory");
        let absent = temp_dir.path().join("absent.toml");
        assert!(
            load_explicit_account_credentials(&absent, Some("https://private.example"))
                .expect("absent explicit credentials are reported")
                .is_none()
        );

        let credentials_path = temp_dir.path().join("credentials.toml");
        std::fs::write(
            &credentials_path,
            "api_key = \"synthetic-token\"\napi_server_url = \"https://file.example\"",
        )
        .expect("write credentials");
        let credentials =
            load_explicit_account_credentials(&credentials_path, Some("https://private.example"))
                .expect("load explicit credentials")
                .expect("credentials present");
        assert_eq!(credentials.api_server_url, "https://private.example");
    }

    #[test]
    fn configured_accounts_only_migrate_devin_protocol_aliases() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin2".to_string(),
            ProviderDef {
                protocol: Some(Protocol::Devin),
                ..ProviderDef::default()
            },
        );
        config.upsert(
            "devin3".to_string(),
            ProviderDef {
                protocol: Some(Protocol::Openai),
                ..ProviderDef::default()
            },
        );
        assert_eq!(configured_account_names(&config), vec!["2".to_string()]);
        assert!(legacy_account_definition(&config, "2").is_some());
        assert!(legacy_account_definition(&config, "3").is_none());
    }

    #[test]
    fn canonical_accounts_do_not_hijack_non_devin_numeric_provider_slugs() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                accounts: HashMap::from([(
                    "2".to_string(),
                    n00n_config::providers::ProviderAccountDef::default(),
                )]),
                ..ProviderDef::default()
            },
        );
        config.upsert(
            "devin2".to_string(),
            ProviderDef {
                protocol: Some(Protocol::Openai),
                ..ProviderDef::default()
            },
        );

        assert_eq!(legacy_account_for_provider_slug_in(&config, "devin2"), None);
        config
            .providers
            .get_mut("devin2")
            .expect("provider exists")
            .protocol = Some(Protocol::Devin);
        assert_eq!(
            legacy_account_for_provider_slug_in(&config, "devin2"),
            Some("2".to_string())
        );
    }

    #[test]
    fn account_model_routing_uses_an_unambiguous_separator() {
        assert_eq!(account_and_model("swe-1-7-max"), (None, "swe-1-7-max"));
        assert_eq!(
            account_and_model("2::swe-1-7-max"),
            (Some("2"), "swe-1-7-max")
        );
        assert_eq!(
            account_and_model("org/custom-model"),
            (None, "org/custom-model")
        );
        assert_eq!(
            account_and_model("unknown::custom-model"),
            (Some("unknown"), "custom-model")
        );
        assert_eq!(account_and_model("work::"), (None, "work::"));
        assert_eq!(legacy_account_name("devin2"), Some("2"));
        assert_eq!(legacy_account_name("devin-work"), None);
        assert_eq!(legacy_account_name("devin"), None);
    }

    #[test]
    fn invalid_account_names_are_not_advertised() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                accounts: HashMap::from([
                    (
                        "work".to_string(),
                        n00n_config::providers::ProviderAccountDef::default(),
                    ),
                    (
                        "work/team".to_string(),
                        n00n_config::providers::ProviderAccountDef::default(),
                    ),
                ]),
                ..ProviderDef::default()
            },
        );

        assert_eq!(configured_account_names(&config), vec!["work".to_string()]);
    }

    #[test]
    fn configured_plan_without_resolved_endpoint_does_not_override_credentials() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                plan: Some("unknown".to_string()),
                ..ProviderDef::default()
            },
        );

        assert_eq!(account_api_server_url_override(&config, None), None);
    }

    #[test]
    fn canonical_account_uses_configured_devin_base_url() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                base_url: Some("https://private.example".to_string()),
                ..ProviderDef::default()
            },
        );

        assert_eq!(
            account_api_server_url_override(&config, None).as_deref(),
            Some("https://private.example")
        );
    }

    #[test]
    fn home_expansion_handles_home_and_one_prefix() {
        let home = optional_env("HOME")
            .expect("read HOME")
            .expect("HOME is configured for tests");
        assert_eq!(
            expand_home(Path::new("~")).expect("expand home"),
            PathBuf::from(&home)
        );
        assert_eq!(
            expand_home(Path::new("~/~/credentials.toml")).expect("expand relative path"),
            PathBuf::from(home).join("~/credentials.toml")
        );
    }

    #[test]
    fn account_url_override_is_validated() {
        let credentials = DevinCredentials {
            session_token: "token".to_string(),
            api_server_url: "https://configured.example".to_string(),
        };
        assert_eq!(
            apply_account_url_override(credentials.clone(), Some("https://account.example/path/"))
                .unwrap()
                .api_server_url,
            "https://account.example/path"
        );
        // A query would swallow the appended API path, so the override is
        // unusable — and rerouting to the default would send this account's
        // token somewhere it was never meant to go.
        assert!(
            apply_account_url_override(credentials, Some("https://account.example/?ignored=true"))
                .is_err()
        );
    }

    #[test]
    fn explicit_api_server_url_takes_precedence() {
        assert_eq!(
            resolve_api_server_url(
                "https://configured.example".to_string(),
                Some("https://explicit.example")
            )
            .unwrap(),
            "https://explicit.example"
        );
    }

    #[test]
    fn configured_api_server_url_is_preserved_without_explicit_url() {
        assert_eq!(
            resolve_api_server_url("https://configured.example".to_string(), None).unwrap(),
            "https://configured.example"
        );
    }

    /// Replacing an unusable endpoint with the default posts the session
    /// token to a host the operator never configured. Reject it instead.
    #[test]
    fn an_invalid_explicit_url_is_rejected_not_rerouted() {
        let rejected =
            resolve_api_server_url("https://configured.example".to_string(), Some("not-a-url"));
        assert!(
            matches!(rejected, Err(AgentError::Config { .. })),
            "{rejected:?}"
        );
    }

    #[test]
    fn an_invalid_stored_url_is_rejected_too() {
        assert!(resolve_api_server_url("devin".to_string(), None).is_err());
    }

    /// Only the absence of any candidate falls back.
    #[test]
    fn a_blank_candidate_falls_back_to_the_default() {
        assert_eq!(
            resolve_api_server_url(String::new(), None).unwrap(),
            DEVIN_API_URL
        );
        assert_eq!(
            resolve_api_server_url(String::new(), Some("   ")).unwrap(),
            DEVIN_API_URL
        );
    }

    /// A host override in Devin's own auth response is provider output, not
    /// operator configuration: an unusable one is dropped, since the request
    /// already reached the configured host.
    #[test]
    fn a_provider_supplied_override_is_dropped_when_unusable() {
        assert_eq!(
            apply_provider_api_server_url("https://configured.example", "not-a-url"),
            "https://configured.example"
        );
        assert_eq!(
            apply_provider_api_server_url("https://configured.example", ""),
            "https://configured.example"
        );
        assert_eq!(
            apply_provider_api_server_url("https://configured.example", "https://custom.example/"),
            "https://custom.example"
        );
    }

    #[test]
    fn discovered_base_url_rejects_a_schemeless_config_value() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                base_url: Some("devin".to_string()),
                ..ProviderDef::default()
            },
        );
        assert!(discovered_base_url(&config).is_err());
    }

    #[test]
    fn discovered_base_url_preserves_valid_https_config_value() {
        let mut config = ProvidersConfig::default();
        config.upsert(
            "devin".to_string(),
            ProviderDef {
                base_url: Some("https://configured.example".to_string()),
                ..ProviderDef::default()
            },
        );
        assert_eq!(
            discovered_base_url(&config).unwrap(),
            "https://configured.example"
        );
    }

    #[test]
    fn discovered_base_url_defaults_when_unconfigured() {
        let config = ProvidersConfig::default();
        assert_eq!(discovered_base_url(&config).unwrap(), DEVIN_API_URL);
    }

    /// URI schemes are case-insensitive. Rejecting `HTTPS://` sent the auth
    /// request and session token to the default service instead of the
    /// configured endpoint.
    #[test]
    fn url_scheme_comparison_is_case_insensitive() {
        for url in ["HTTPS://devin.example", "Http://devin.example"] {
            assert_eq!(
                resolve_api_server_url("https://configured.example".to_string(), Some(url))
                    .unwrap(),
                url
            );
        }
        assert!(!is_valid_api_server_url("https://"));
        assert!(!is_valid_api_server_url("ftp://devin.example"));
    }

    /// API paths are appended by concatenation, so a query or fragment would
    /// place the path after `?` or `#` and the request would never reach it.
    #[test]
    fn urls_carrying_a_query_or_fragment_are_rejected() {
        for url in [
            "https://devin.example?token=abc",
            "https://devin.example#frag",
            "https://devin.example/path?x=1",
            "not-a-url",
            "",
        ] {
            assert!(!is_valid_api_server_url(url), "should reject: {url}");
        }
        assert!(is_valid_api_server_url("https://devin.example"));
        assert!(is_valid_api_server_url("https://devin.example/base"));
        // The WHATWG parser skips the surplus slash, so this is `https://nohost/`
        // with a real host rather than a host-less URL.
        assert!(is_valid_api_server_url("https:///nohost"));
    }

    #[test]
    fn whitespace_padded_configured_url_is_trimmed_not_discarded() {
        assert_eq!(
            resolve_api_server_url("  https://configured.example/  ".to_string(), None).unwrap(),
            "https://configured.example"
        );
    }

    #[test]
    fn invalid_configured_and_explicit_urls_are_both_rejected() {
        assert!(resolve_api_server_url("not-a-url".to_string(), Some("also-not-a-url")).is_err());
    }

    #[test]
    fn chat_message_ids_are_stable_and_keep_bot_prefix() {
        assert_eq!(
            chat_message_id(CASCADE_ID, 2, "assistant"),
            "bot-cascade-1-2-assistant"
        );
        assert_eq!(
            chat_message_id(CASCADE_ID, 2, "user-0"),
            chat_message_id(CASCADE_ID, 2, "user-0")
        );
    }

    #[test]
    fn encode_devin_tools_rejects_non_array() {
        assert!(matches!(
            encode_devin_tools(&serde_json::json!({"name": "read"})),
            Err(AgentError::Config { message }) if message == "Devin tools must be an array"
        ));
    }

    #[test]
    fn model_max_tokens_are_not_capped_by_fallback() {
        assert_eq!(max_tokens_for_model(Some(128_000)), 128_000);
        assert_eq!(max_tokens_for_model(None), u64::from(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn trailer_parser_accepts_success_and_maps_sanitized_errors() {
        assert_eq!(
            parse_devin_trailer(br#"{"code":"ok","message":"private"}"#)
                .expect("successful trailer"),
            Some("ok".to_string())
        );
        let error = parse_devin_trailer(br#"{"code":"unavailable","message":"private"}"#)
            .expect_err("error trailer");
        assert!(matches!(
            error,
            AgentError::Api { status: 503, message }
                if message == "Devin stream failed: private (code unavailable)"
        ));

        let permission = parse_devin_trailer(
            br#"{"error":{"code":"permission_denied","message":"Authorization: Bearer secret-token"}}"#,
        )
        .expect_err("permission trailer");
        assert!(matches!(
            permission,
            AgentError::Api { status: 403, message }
                if message == "Devin stream failed: Authorization:[redacted] (code permission_denied)"
        ));

        let contradictory = parse_devin_trailer(br#"{"error":{"message":"private"},"code":"ok"}"#)
            .expect_err("error cannot fall back to a success code");
        assert!(matches!(contradictory, AgentError::Api { status: 502, .. }));

        let missing_code = parse_devin_trailer(br#"{"error":{"message":"private"}}"#)
            .expect_err("error without code is rejected");
        assert!(matches!(
            missing_code,
            AgentError::Api { status: 502, message }
                if message == "Devin end-stream trailer contained an error without a valid code"
        ));

        let malicious = parse_devin_trailer(br#"{"code":"bad token: secret"}"#)
            .expect_err("invalid code is rejected");
        assert!(matches!(
            malicious,
            AgentError::Api { status: 500, message }
                if message == "Devin stream failed with trailer code invalid"
        ));
    }

    #[test]
    fn later_error_trailer_takes_precedence_in_buffered_frames() {
        let success = encode_frame(
            super::super::devin_connect::CONNECT_END_STREAM_FLAG,
            br#"{"code":"ok"}"#,
        )
        .expect("encode success trailer");
        let failure = encode_frame(
            super::super::devin_connect::CONNECT_END_STREAM_FLAG,
            br#"{"code":"unavailable","message":"retry later"}"#,
        )
        .expect("encode failure trailer");
        let mut frame_buffer = FrameBuffer::default();
        frame_buffer.push(&success);
        frame_buffer.push(&failure);
        let mut successful_trailer = false;
        assert!(!successful_trailer);

        let first = frame_buffer
            .next_frame()
            .expect("success frame present")
            .expect("success frame valid");
        assert_eq!(
            parse_devin_trailer(&first.payload).expect("success trailer"),
            Some("ok".to_string())
        );
        successful_trailer = true;

        let second = frame_buffer
            .next_frame()
            .expect("failure frame present")
            .expect("failure frame valid");
        let error = parse_devin_trailer(&second.payload).expect_err("later error trailer");
        assert!(successful_trailer);
        assert!(matches!(error, AgentError::Api { status: 503, .. }));
    }

    #[test]
    fn trailer_parser_rejects_malformed_json_without_echoing_payload() {
        let empty = parse_devin_trailer(b"   ").expect_err("empty trailer");
        assert!(matches!(
            empty,
            AgentError::Api { status: 502, message } if message == "empty Devin end-stream trailer"
        ));

        let error = parse_devin_trailer(b"secret raw payload").expect_err("malformed trailer");
        assert!(matches!(
            error,
            AgentError::Api { status: 502, message } if message == TRAILER_JSON_ERROR
        ));

        for payload in [
            b"null".as_slice(),
            b"[]".as_slice(),
            br#""garbage""#,
            br#"{"code":42}"#,
        ] {
            assert!(matches!(
                parse_devin_trailer(payload),
                Err(AgentError::Api { status: 502, .. })
            ));
        }
    }

    #[test]
    fn tool_result_splits_surrounding_user_text_into_stable_prompts() {
        let messages = [Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "before".to_string(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "result".to_string(),
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "after".to_string(),
                },
            ],
            display_text: None,
            control: false,
        }];

        let prompts = encode_devin_chat_message_prompts(&messages, CASCADE_ID)
            .expect("encode message prompts");
        assert_eq!(prompts.len(), 3);
        assert_eq!(
            prompt_string_field(&prompts[0], 1).as_deref(),
            Some("cascade-1-0-user-0")
        );
        assert_eq!(
            prompt_string_field(&prompts[0], 3).as_deref(),
            Some("before")
        );
        assert_eq!(
            prompt_string_field(&prompts[1], 1).as_deref(),
            Some("cascade-1-0-tool-call-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[1], 7).as_deref(),
            Some("call-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[2], 1).as_deref(),
            Some("cascade-1-0-user-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[2], 3).as_deref(),
            Some("after")
        );
    }

    #[test]
    fn file_reference_is_rendered_as_omitted_marker() {
        let messages = [Message {
            role: Role::User,
            content: vec![ContentBlock::File {
                source: crate::types::FileSource::file_id("file-123", None),
            }],
            ..Default::default()
        }];

        let prompts = encode_devin_chat_message_prompts(&messages, CASCADE_ID)
            .expect("encode message prompts");
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompt_string_field(&prompts[0], 3).as_deref(),
            Some("[file omitted: file-123]")
        );
    }

    #[test]
    fn ordered_tool_call_blocks_follow_first_arrival_order() {
        let tool_calls = HashMap::from([
            (
                "second".to_string(),
                ("write".to_string(), "{}".to_string()),
            ),
            ("first".to_string(), ("read".to_string(), "{}".to_string())),
        ]);
        let blocks =
            ordered_tool_call_blocks(tool_calls, vec!["first".to_string(), "second".to_string()])
                .expect("ordered tool blocks");

        assert!(matches!(&blocks[0], ContentBlock::ToolUse { id, .. } if id == "first"));
        assert!(matches!(&blocks[1], ContentBlock::ToolUse { id, .. } if id == "second"));
    }

    #[test]
    fn ordered_tool_call_blocks_treats_empty_args_as_empty_object() {
        let mut tool_calls = HashMap::new();
        tool_calls.insert("call-1".to_string(), ("bash".to_string(), String::new()));
        let blocks = ordered_tool_call_blocks(tool_calls, vec!["call-1".to_string()])
            .expect("empty args parse");

        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::ToolUse {
                id,
                name,
                input: Value::Object(map),
            } if id == "call-1" && name == "bash" && map.is_empty()
        ));
    }
}

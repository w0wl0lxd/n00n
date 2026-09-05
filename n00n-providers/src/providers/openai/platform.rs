use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use flume::Sender;
use n00n_storage::StateDir;
use n00n_storage::id::{SessionRef, n00nId};
use n00n_storage::now_epoch;
use n00n_storage::sessions::{
    OPENAI_RESPONSE_CHAIN_TTL_SECONDS, OpenAiResponseChainLock, StoredOpenAiResponseChain,
    delete_openai_response_chain, load_openai_response_chain, openai_response_chain_parent_exists,
    save_openai_response_chain, try_lock_openai_response_chain,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::model::Model;
use crate::provider::{BoxFuture, Provider};
use crate::{
    AgentError, CacheControl, CacheHealth, CacheKind, CodingPlanAdmissionTransport,
    HistoryReplayReason, Message, OpenAiPromptCacheMode, ProviderEvent, ProviderUsage,
    RequestDeliveryMetadata, RequestDeliveryPhase, RequestOptions, StreamResponse, System,
    UsageLimit, dialect,
};

use super::auth;
use crate::providers::ResolvedAuth;
use crate::providers::openai_compat::OpenAiCompatProvider;

include!(concat!(env!("OUT_DIR"), "/provider_configs/openai.rs"));
include!(concat!(env!("OUT_DIR"), "/provider_configs/codex.rs"));

pub(crate) const CODING_PLAN_CONTEXT_WINDOW: u32 = 272_000;
const SESSION_STATE_TTL: Duration = Duration::from_hours(1);
const FIVE_MINUTES_MILLIS: u64 = 5 * 60 * 1_000;
const THIRTY_MINUTES_MILLIS: u64 = 30 * 60 * 1_000;
const CODING_PLAN_DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Ceiling for the locally computed exponential backoff between admission
/// retries.
///
/// The schedule does not reach it today: `coding_plan_backoff` only ever sees
/// counts below [`CODING_PLAN_ADMISSION_MAX_RETRIES`], which tops the ceiling
/// out at 4s. This binds only if that cap is raised, and is what stops the
/// shift running away when it is.
const CODING_PLAN_MAX_RETRY_DELAY: Duration = Duration::from_secs(8);
/// Ceiling for a server-directed `Retry-After`. A malformed header already
/// resolves to a one-minute conservative delay, which would otherwise stall an
/// interactive turn for the whole minute.
const CODING_PLAN_MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// A wait at or above this is reported at `warn`, not `debug`: it is long
/// enough that a user watching the turn needs to know why it stopped.
const CODING_PLAN_LOUD_RETRY_DELAY: Duration = Duration::from_secs(2);
const CODING_PLAN_ADMISSION_MAX_RETRIES: u8 = 5;
const CODING_PLAN_MAX_SLOTS: u8 = 8;
const RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const USAGE_WINDOW_TOLERANCE: f64 = 0.05;
const USAGE_WINDOW_5HOURS_SECONDS: i64 = 18_000;
const USAGE_WINDOW_1DAY_SECONDS: i64 = 86_400;
const USAGE_WINDOW_1WEEK_SECONDS: i64 = 604_800;
const USAGE_WINDOW_1MONTH_SECONDS: i64 = 2_592_000;
const USAGE_WINDOW_1YEAR_SECONDS: i64 = 31_536_000;
const RESPONSE_CHAIN_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const PROMPT_CACHE_SHARDS: u8 = 16;
const CODEX_ORIGINATOR: &str = "n00n";
const CODEX_ORIGINATOR_HEADER: &str = "originator";
const CODEX_SESSION_HEADERS: [&str; 3] = ["session-id", "thread-id", "x-client-request-id"];

static PROCESS_INSTANCE_NONCE: OnceLock<u64> = OnceLock::new();
static RESPONSE_OPERATIONS: OnceLock<ResponseOperationRegistry> = OnceLock::new();

fn coding_plan_slot_count(slots: u64) -> u8 {
    match u8::try_from(slots.clamp(1, u64::from(CODING_PLAN_MAX_SLOTS))) {
        Ok(slots) => slots,
        Err(_) => CODING_PLAN_MAX_SLOTS,
    }
}

type ResponseOperationSlot = Arc<AsyncMutex<()>>;
type ResponseOperationKey = (PathBuf, n00nId);
type ResponseOperationRegistry = Mutex<HashMap<ResponseOperationKey, Weak<AsyncMutex<()>>>>;
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CodexCacheCapabilities {
    pub accepts_prompt_cache_options_implicit: bool,
    pub accepts_prompt_cache_options_explicit: bool,
    pub accepts_prompt_cache_breakpoints: bool,
}

impl CodexCacheCapabilities {
    fn apply_to_request_options(&self, mut opts: RequestOptions) -> RequestOptions {
        let requested_breakpoints = opts.message_cache_breakpoints;
        opts.message_cache_breakpoints = 0;
        opts.openai_prompt_cache_mode = None;

        if self.accepts_prompt_cache_breakpoints && requested_breakpoints > 0 {
            opts.message_cache_breakpoints = requested_breakpoints;
        }
        if self.accepts_prompt_cache_options_explicit && opts.message_cache_breakpoints > 0 {
            opts.openai_prompt_cache_mode = Some(OpenAiPromptCacheMode::Explicit);
        } else if self.accepts_prompt_cache_options_implicit {
            opts.openai_prompt_cache_mode = Some(OpenAiPromptCacheMode::Implicit);
        }

        opts
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseChainResetReason {
    RequestPrefixScopeChanged,
    MessagePrefixChanged,
    NoNewInputAfterResponse,
    SocketNotReusable,
    AttemptFailed,
    PreviousResponseNotFound,
}

impl ResponseChainResetReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RequestPrefixScopeChanged => "request_prefix_scope_changed",
            Self::MessagePrefixChanged => "message_prefix_changed",
            Self::NoNewInputAfterResponse => "no_new_input_after_response",
            Self::SocketNotReusable => "socket_not_reusable",
            Self::AttemptFailed => "attempt_failed",
            Self::PreviousResponseNotFound => "previous_response_not_found",
        }
    }
}

fn clamp_responses_cache_breakpoints(model: &Model, mut opts: RequestOptions) -> RequestOptions {
    if !model.supports_prompt_cache_breakpoint() {
        opts.message_cache_breakpoints = 0;
    }
    opts
}

fn log_response_chain_reset(reason: ResponseChainResetReason, durable_chain: Option<bool>) {
    if let Some(durable_chain) = durable_chain {
        debug!(
            chain_reset = true,
            chain_reset_reason = reason.as_str(),
            durable_chain,
            "resetting OpenAI response chain"
        );
    } else {
        debug!(
            chain_reset = true,
            chain_reset_reason = reason.as_str(),
            "resetting OpenAI response chain"
        );
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiOptions {
    coding_plan_slots: u8,
    codex_provider: bool,
    codex_cache_capabilities: CodexCacheCapabilities,
}

impl OpenAiOptions {
    #[must_use]
    pub fn with_coding_plan_slots(slots: u64) -> Self {
        Self {
            coding_plan_slots: coding_plan_slot_count(slots),
            codex_provider: false,
            codex_cache_capabilities: CodexCacheCapabilities::default(),
        }
    }

    #[must_use]
    pub const fn with_codex(mut self) -> Self {
        self.codex_provider = true;
        self
    }

    #[must_use]
    pub const fn with_codex_cache_capabilities(
        mut self,
        capabilities: CodexCacheCapabilities,
    ) -> Self {
        self.codex_cache_capabilities = capabilities;
        self
    }

    #[must_use]
    pub fn codex() -> Self {
        Self::with_coding_plan_slots(u64::from(CODING_PLAN_MAX_SLOTS)).with_codex()
    }
}

impl Default for OpenAiOptions {
    fn default() -> Self {
        Self::with_coding_plan_slots(u64::from(CODING_PLAN_MAX_SLOTS))
    }
}

impl From<&n00n_config::ProviderConfig> for OpenAiOptions {
    fn from(config: &n00n_config::ProviderConfig) -> Self {
        Self::with_coding_plan_slots(config.openai_coding_plan_slots).with_codex_cache_capabilities(
            CodexCacheCapabilities {
                accepts_prompt_cache_options_implicit: config
                    .openai_codex_accepts_prompt_cache_options_implicit,
                accepts_prompt_cache_options_explicit: config
                    .openai_codex_accepts_prompt_cache_options_explicit,
                accepts_prompt_cache_breakpoints: config
                    .openai_codex_accepts_prompt_cache_breakpoints,
            },
        )
    }
}

struct ScopedResponsesWebSocket {
    socket: super::websocket::ResponsesWebSocket,
    credential_hash: String,
    auth_generation: u64,
}

type ResponseConnectionSlot = Arc<AsyncMutex<Option<ScopedResponsesWebSocket>>>;

struct CodingPlanAuth {
    resolved: ResolvedAuth,
    oauth_tokens: Option<n00n_storage::auth::OAuthTokens>,
}

struct PreSendAuth {
    resolved: ResolvedAuth,
    credential_hash: String,
    generation: u64,
}

struct CodexAttempt {
    previous_response_id: Option<String>,
    emitted_event: bool,
    definitive_rejection: bool,
    delivery: Option<RequestDeliveryMetadata>,
    result: Result<StreamResponse, AgentError>,
}

impl CodexAttempt {
    fn from_websocket_error(
        previous_response_id: Option<String>,
        error: super::websocket::WebSocketAttemptError,
    ) -> Self {
        let emitted_event = error.delivery.emitted_event;
        let definitive_rejection = error.definitive_rejection();
        let delivery = Some((*error.delivery).clone());
        let provider_error = error.into_agent_error();
        Self {
            previous_response_id,
            emitted_event,
            definitive_rejection,
            delivery,
            result: Err(provider_error),
        }
    }

    fn should_reacquire_admission(&self) -> bool {
        !self.emitted_event
            && matches!(
                &self.result,
                Err(AgentError::CodingPlanAdmissionScopeChanged)
            )
    }

    fn should_retry_after_oauth_refresh(&self) -> bool {
        !self.emitted_event
            && self.definitive_rejection
            && matches!(&self.result, Err(error) if error.is_auth_error())
            && matches!(
                &self.delivery,
                Some(delivery)
                    if delivery.phase == RequestDeliveryPhase::NotSent
                        && delivery.response_id.is_none()
            )
    }
}

#[derive(Debug)]
struct OpenAiSessionState {
    last_response_id: Option<String>,
    last_message_count: usize,
    model_id: Option<String>,
    system_hash: Option<String>,
    tools_hash: Option<String>,
    messages_hash: Option<String>,
    auth_scope_hash: Option<String>,
    expires_at: u64,
    last_used: Instant,
}

impl Default for OpenAiSessionState {
    fn default() -> Self {
        Self {
            last_response_id: None,
            last_message_count: 0,
            model_id: None,
            system_hash: None,
            tools_hash: None,
            messages_hash: None,
            auth_scope_hash: None,
            expires_at: 0,
            last_used: Instant::now(),
        }
    }
}

impl OpenAiSessionState {
    fn from_stored(stored: StoredOpenAiResponseChain) -> Self {
        Self {
            last_response_id: Some(stored.response_id),
            last_message_count: stored.message_count,
            model_id: stored.model_id,
            system_hash: stored.system_hash,
            tools_hash: Some(stored.tools_hash),
            messages_hash: Some(stored.messages_hash),
            auth_scope_hash: Some(stored.auth_scope_hash),
            expires_at: stored.expires_at,
            last_used: Instant::now(),
        }
    }

    fn to_stored(&self) -> Option<StoredOpenAiResponseChain> {
        Some(StoredOpenAiResponseChain {
            response_id: self.last_response_id.clone()?,
            message_count: self.last_message_count,
            model_id: self.model_id.clone(),
            system_hash: Some(self.system_hash.clone()?),
            tools_hash: self.tools_hash.clone()?,
            messages_hash: self.messages_hash.clone()?,
            auth_scope_hash: self.auth_scope_hash.clone()?,
            expires_at: self.expires_at,
        })
    }
}

fn stable_json_hash<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn system_hash(system: &System) -> String {
    let mut digest = Sha256::new();
    for block in system
        .blocks()
        .iter()
        .filter(|block| block.cache != CacheControl::Dynamic)
    {
        digest.update(block.text.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn cacheable_system_prefix(system: &System) -> String {
    system
        .cacheable_prefix_blocks()
        .iter()
        .map(|block| block.text.as_str())
        .collect()
}

fn system_with_prefix(prefix: Option<&str>, system: &System) -> System {
    let Some(prefix) = prefix else {
        return system.clone();
    };
    let mut prefixed = System::new();
    prefixed.push_static(format!("{prefix}\n\n"));
    let dynamic_boundary = system.dynamic_boundary();
    for (index, block) in system.blocks().iter().enumerate() {
        if dynamic_boundary == Some(index) {
            prefixed.mark_dynamic_boundary();
        }
        prefixed.push(block.clone());
    }
    if dynamic_boundary == Some(system.blocks().len()) {
        prefixed.mark_dynamic_boundary();
    }
    prefixed
}

fn short_hash(hash: &str) -> &str {
    match hash.get(..12) {
        Some(short) => short,
        None => hash,
    }
}

#[derive(Clone, PartialEq, Eq)]
struct CachePrefixFingerprint {
    model_id: String,
    prefix_hash: String,
    system_hash: String,
    tools_hash: String,
}

impl CachePrefixFingerprint {
    fn new(model_id: &str, system: &System, tools_hash: &str) -> Self {
        let system_prefix = cacheable_system_prefix(system);
        let mut digest = Sha256::new();
        digest.update(model_id.len().to_le_bytes());
        digest.update(model_id.as_bytes());
        digest.update(system_prefix.len().to_le_bytes());
        digest.update(system_prefix.as_bytes());
        digest.update(tools_hash.as_bytes());
        Self {
            model_id: model_id.to_owned(),
            prefix_hash: format!("{:x}", digest.finalize()),
            system_hash: system_hash(system),
            tools_hash: tools_hash.to_owned(),
        }
    }

    fn prefix_hash(&self) -> &str {
        &self.prefix_hash
    }

    fn prompt_cache_key(&self, session_id: Option<&SessionRef>) -> String {
        let prefix_hash = self.prefix_hash();
        let shard = session_id.map_or(0, |session_id| {
            Sha256::digest(canonical_session_key(session_id).to_string().as_bytes())[0]
                % PROMPT_CACHE_SHARDS
        });
        format!("n00n-{prefix_hash}-s{shard}")
    }
}

impl fmt::Debug for CachePrefixFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachePrefixFingerprint")
            .field("model_id", &self.model_id)
            .field("prefix_hash", &short_hash(self.prefix_hash()))
            .field("system_hash", &short_hash(&self.system_hash))
            .field("tools_hash", &short_hash(&self.tools_hash))
            .finish()
    }
}

fn request_tools_hash(tools: &Value, opts: &RequestOptions) -> Result<String, serde_json::Error> {
    match opts.hosted_tool_search.as_ref() {
        Some(tool_search) => stable_json_hash(&(tools, tool_search)),
        None => stable_json_hash(tools),
    }
}

fn credential_hash(auth: &ResolvedAuth) -> String {
    let mut headers = auth.headers.iter().collect::<Vec<_>>();
    headers.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut digest = Sha256::new();
    if let Some(base_url) = auth.base_url.as_deref() {
        digest.update(base_url.len().to_le_bytes());
        digest.update(base_url.as_bytes());
    }
    for (name, value) in headers {
        digest.update(name.len().to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(value.len().to_le_bytes());
        digest.update(value.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn with_codex_session_headers(mut auth: ResolvedAuth, session_id: Option<n00nId>) -> ResolvedAuth {
    if auth.base_url.as_deref() != Some(auth::CODING_PLAN_BASE_URL) {
        return auth;
    }
    auth.headers.retain(|(name, _)| {
        !CODEX_SESSION_HEADERS
            .iter()
            .any(|header| name.eq_ignore_ascii_case(header))
            && !name.eq_ignore_ascii_case(CODEX_ORIGINATOR_HEADER)
    });
    if let Some(session_id) = session_id {
        let session_id = session_id.to_string();
        auth.headers.extend(
            CODEX_SESSION_HEADERS
                .into_iter()
                .map(|name| (name.to_owned(), session_id.clone())),
        );
    }
    auth.headers
        .push((CODEX_ORIGINATOR_HEADER.into(), CODEX_ORIGINATOR.into()));
    auth
}

fn response_state_scope_hash(auth: &ResolvedAuth) -> String {
    if auth.base_url.as_deref() != Some(auth::CODING_PLAN_BASE_URL) {
        return credential_hash(auth);
    }
    let Some((_, account_id)) = auth
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("chatgpt-account-id"))
    else {
        return credential_hash(auth);
    };
    let mut digest = Sha256::new();
    digest.update(auth::CODING_PLAN_BASE_URL.as_bytes());
    digest.update(account_id.len().to_le_bytes());
    digest.update(account_id.as_bytes());
    format!("{:x}", digest.finalize())
}

/// A WebSocket-origin admission 403 still falls back to HTTP: live probes show
/// the Coding Plan HTTP endpoint accepting the same credentials and headers that
/// the WebSocket upgrade rejects, so the turn can still be served. An
/// HTTP-origin admission 403 is excluded because there is no further transport
/// to try.
fn should_fallback_to_http(error: &super::websocket::WebSocketAttemptError) -> bool {
    error.transport_failure
        && !error.delivery.emitted_event
        && !error.error.is_auth_error()
        && !matches!(
            error.error.as_ref(),
            AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::Http,
                ..
            }
        )
        && error.delivery.phase == RequestDeliveryPhase::NotSent
}

fn full_history_replay_required(
    previous_response_id: Option<&str>,
    message_count: usize,
    protect_history_replay: bool,
    allow_history_replay: bool,
) -> bool {
    protect_history_replay
        && previous_response_id.is_none()
        && message_count > 1
        && !allow_history_replay
}

fn not_sent_websocket_error(error: AgentError) -> super::websocket::WebSocketAttemptError {
    super::websocket::WebSocketAttemptError::transport(
        error,
        false,
        RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent),
    )
}

/// Builds the attempt record for a failed HTTP fallback.
///
/// `do_stream` raises `CodingPlanAdmission` from the response status, before
/// `parse_sse` runs: nothing was emitted and nothing was accepted. Reporting
/// that as sent hides it from the admission retry loop, so the turn would
/// fail on the first 403 with neither `Retry-After` nor backoff applied.
/// Every other error may have arrived mid-stream, so it stays conservative.
///
/// `inherited_retry_after` is the delay the WebSocket rejection asked for. The
/// HTTP probe often answers with an empty 403 and no header of its own, and
/// without this the retry loop would discard the server's delay and restart on
/// the short local schedule. A `Retry-After` on the HTTP response is more
/// recent, so it wins.
fn http_fallback_attempt(
    previous_response_id: Option<String>,
    error: AgentError,
    inherited_retry_after: Option<Duration>,
) -> CodexAttempt {
    if let AgentError::CodingPlanAdmission {
        transport,
        retry_after,
    } = error
    {
        return CodexAttempt {
            previous_response_id,
            emitted_event: false,
            definitive_rejection: true,
            delivery: Some(RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent)),
            result: Err(AgentError::CodingPlanAdmission {
                transport,
                retry_after: retry_after.or(inherited_retry_after),
            }),
        };
    }
    CodexAttempt {
        previous_response_id,
        emitted_event: true,
        definitive_rejection: false,
        delivery: None,
        result: Err(suppress_retry_after_send(error)),
    }
}

/// The delay an admission rejection asked for, if the error is one.
fn admission_retry_after(error: &AgentError) -> Option<Duration> {
    match error {
        AgentError::CodingPlanAdmission { retry_after, .. } => *retry_after,
        _ => None,
    }
}

fn suppress_retry_after_send(error: AgentError) -> AgentError {
    error.suppress_retry_after_send(Some(RequestDeliveryMetadata::new(
        RequestDeliveryPhase::SentAwaitingAcceptance,
    )))
}

fn canonical_session_key(session_id: &SessionRef) -> n00nId {
    session_id.id()
}

fn log_responses_request(
    transport: &'static str,
    body: &Value,
    history_message_count: usize,
    sent_message_count: usize,
    chain_hit: bool,
    full_history_fallback: bool,
) {
    if tracing::enabled!(tracing::Level::DEBUG) {
        let diagnostics = super::responses::request_diagnostics(body);
        debug!(
            transport,
            request_kind = if chain_hit {
                "incremental"
            } else {
                "full_history"
            },
            chain_hit,
            full_history_fallback,
            history_message_count,
            message_count = sent_message_count,
            input_item_count = diagnostics.input_items,
            request_bytes = diagnostics.request_bytes,
            text_item_count = diagnostics.text_items,
            text_bytes = diagnostics.text_bytes,
            tool_item_count = diagnostics.tool_items,
            tool_bytes = diagnostics.tool_bytes,
            image_item_count = diagnostics.image_items,
            image_bytes = diagnostics.image_bytes,
            reasoning_item_count = diagnostics.reasoning_items,
            reasoning_bytes = diagnostics.reasoning_bytes,
            "sending OpenAI Responses request"
        );
    }
}

fn process_instance_nonce() -> u64 {
    *PROCESS_INSTANCE_NONCE.get_or_init(|| fastrand::u64(..))
}

fn copy_oauth_tokens(tokens: &n00n_storage::auth::OAuthTokens) -> n00n_storage::auth::OAuthTokens {
    n00n_storage::auth::OAuthTokens {
        access: tokens.access.clone(),
        refresh: tokens.refresh.clone(),
        expires: tokens.expires,
        account_id: tokens.account_id.clone(),
    }
}

fn auth_expiry_bucket(tokens: &n00n_storage::auth::OAuthTokens) -> &'static str {
    let remaining = tokens
        .expires
        .saturating_sub(n00n_storage::auth::now_millis());
    if tokens.is_hard_expired() {
        "expired"
    } else if tokens.is_expired() {
        "refresh_buffer"
    } else if remaining < FIVE_MINUTES_MILLIS {
        "under_5m"
    } else if remaining < THIRTY_MINUTES_MILLIS {
        "under_30m"
    } else {
        "over_30m"
    }
}

fn incremental_for_state<'a>(
    state: &mut OpenAiSessionState,
    fingerprint: &CachePrefixFingerprint,
    auth_scope_hash: &str,
    messages: &'a [Message],
) -> Result<(Option<String>, &'a [Message]), serde_json::Error> {
    if state.model_id.as_deref() != Some(fingerprint.model_id.as_str())
        || state.system_hash.as_deref() != Some(fingerprint.system_hash.as_str())
        || state.tools_hash.as_deref() != Some(fingerprint.tools_hash.as_str())
        || state.auth_scope_hash.as_deref() != Some(auth_scope_hash)
        || messages.len() < state.last_message_count
    {
        if state.last_response_id.is_some() {
            log_response_chain_reset(ResponseChainResetReason::RequestPrefixScopeChanged, None);
        }
        *state = OpenAiSessionState {
            model_id: Some(fingerprint.model_id.clone()),
            system_hash: Some(fingerprint.system_hash.clone()),
            tools_hash: Some(fingerprint.tools_hash.clone()),
            auth_scope_hash: Some(auth_scope_hash.to_owned()),
            ..Default::default()
        };
    }

    if state.last_message_count > 0 {
        let current_hash = stable_json_hash(&messages[..state.last_message_count])?;
        if state.messages_hash.as_deref() != Some(current_hash.as_str()) {
            log_response_chain_reset(ResponseChainResetReason::MessagePrefixChanged, None);
            *state = OpenAiSessionState {
                model_id: Some(fingerprint.model_id.clone()),
                system_hash: Some(fingerprint.system_hash.clone()),
                tools_hash: Some(fingerprint.tools_hash.clone()),
                messages_hash: Some(current_hash),
                auth_scope_hash: Some(auth_scope_hash.to_owned()),
                ..Default::default()
            };
        }
    }

    if let Some(previous_response_id) = state.last_response_id.clone() {
        if messages.len() > state.last_message_count + 1 {
            return Ok((
                Some(previous_response_id),
                &messages[state.last_message_count + 1..],
            ));
        }
        log_response_chain_reset(ResponseChainResetReason::NoNewInputAfterResponse, None);
        *state = OpenAiSessionState {
            model_id: Some(fingerprint.model_id.clone()),
            system_hash: Some(fingerprint.system_hash.clone()),
            tools_hash: Some(fingerprint.tools_hash.clone()),
            auth_scope_hash: Some(auth_scope_hash.to_owned()),
            ..Default::default()
        };
    }

    Ok((None, &messages[state.last_message_count..]))
}

fn record_in_state(
    state: &mut OpenAiSessionState,
    response_id: Option<String>,
    fingerprint: &CachePrefixFingerprint,
    auth_scope_hash: &str,
    messages: &[Message],
) -> Result<(), serde_json::Error> {
    if let Some(response_id) = response_id {
        *state = OpenAiSessionState {
            last_response_id: Some(response_id),
            last_message_count: messages.len(),
            model_id: Some(fingerprint.model_id.clone()),
            system_hash: Some(fingerprint.system_hash.clone()),
            tools_hash: Some(fingerprint.tools_hash.clone()),
            messages_hash: Some(stable_json_hash(messages)?),
            auth_scope_hash: Some(auth_scope_hash.to_owned()),
            expires_at: now_epoch().saturating_add(OPENAI_RESPONSE_CHAIN_TTL_SECONDS),
            last_used: Instant::now(),
        };
    } else {
        *state = OpenAiSessionState::default();
    }
    Ok(())
}

pub struct OpenAi {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    auth_refresh: AsyncMutex<()>,
    auth_generation: AtomicU64,
    auth_managed: bool,
    codex: bool,
    storage: Option<StateDir>,
    response_state_storage: Option<StateDir>,
    websocket_connect_timeout: Duration,
    coding_plan_slots: u8,
    codex_cache_capabilities: CodexCacheCapabilities,
    system_prefix: Option<String>,
    session_state: Arc<Mutex<HashMap<n00nId, OpenAiSessionState>>>,
    response_connections: Arc<Mutex<HashMap<n00nId, ResponseConnectionSlot>>>,
}

impl OpenAi {
    pub fn new_with_options(
        timeouts: crate::providers::Timeouts,
        options: OpenAiOptions,
    ) -> Result<Self, AgentError> {
        let storage = StateDir::resolve()?;
        // Authentication refresh is deferred to the first request. Token files
        // are atomically replaced, so startup can safely read the cached copy
        // without waiting behind another process's network refresh.
        let resolved = if options.codex_provider {
            auth::resolve_coding_plan(&storage)?
        } else {
            auth::resolve_api_key(&storage)?
        };
        let config = if options.codex_provider {
            &CODEX_CONFIG
        } else {
            &CONFIG
        };
        let compat = OpenAiCompatProvider::new(config, timeouts)?;
        Ok(Self {
            compat,
            auth: Arc::new(Mutex::new(resolved)),
            auth_refresh: AsyncMutex::new(()),
            auth_generation: AtomicU64::new(0),
            auth_managed: true,
            codex: options.codex_provider,
            storage: Some(storage.clone()),
            response_state_storage: Some(storage),
            websocket_connect_timeout: timeouts.connect,
            coding_plan_slots: options.coding_plan_slots,
            codex_cache_capabilities: options.codex_cache_capabilities,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
            response_connections: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn with_auth(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: crate::providers::Timeouts,
    ) -> Result<Self, AgentError> {
        Self::with_auth_options(auth, timeouts, OpenAiOptions::default())
    }

    pub(crate) fn with_auth_options(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: crate::providers::Timeouts,
        options: OpenAiOptions,
    ) -> Result<Self, AgentError> {
        let config = if options.codex_provider {
            &CODEX_CONFIG
        } else {
            &CONFIG
        };
        Ok(Self {
            compat: OpenAiCompatProvider::new(config, timeouts)?,
            auth,
            auth_refresh: AsyncMutex::new(()),
            auth_generation: AtomicU64::new(0),
            auth_managed: false,
            codex: options.codex_provider,
            storage: None,
            response_state_storage: None,
            websocket_connect_timeout: timeouts.connect,
            coding_plan_slots: options.coding_plan_slots,
            codex_cache_capabilities: options.codex_cache_capabilities,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
            response_connections: Arc::new(Mutex::new(HashMap::new())),
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

    fn is_oauth(&self) -> bool {
        self.auth_managed && self.codex && self.storage.as_ref().is_some_and(auth::is_oauth)
    }

    async fn lock_response_chain(
        &self,
        session_id: Option<&SessionRef>,
    ) -> Result<Option<OpenAiResponseChainLock>, AgentError> {
        let (Some(storage), Some(session_id)) = (self.response_state_storage.clone(), session_id)
        else {
            return Ok(None);
        };
        let session_id = canonical_session_key(session_id);
        let started = Instant::now();
        loop {
            let storage = storage.clone();
            let (parent_exists, lock) =
                smol::unblock(move || -> Result<_, n00n_storage::StorageError> {
                    if !openai_response_chain_parent_exists(&storage, session_id)? {
                        return Ok((false, None));
                    }
                    let lock = try_lock_openai_response_chain(&storage, session_id)?;
                    if lock.is_some() && !openai_response_chain_parent_exists(&storage, session_id)?
                    {
                        return Ok((false, None));
                    }
                    Ok((true, lock))
                })
                .await?;
            if !parent_exists || lock.is_some() {
                return Ok(lock);
            }
            if started.elapsed() >= RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT {
                #[allow(clippy::manual_unwrap_or)]
                let millis = match u64::try_from(RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT.as_millis()) {
                    Ok(millis) => millis,
                    Err(_) => u64::MAX,
                };
                return Err(AgentError::ResponseChainBusy { millis });
            }
            let remaining = RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT.saturating_sub(started.elapsed());
            smol::Timer::after(RESPONSE_CHAIN_LOCK_RETRY_INTERVAL.min(remaining)).await;
        }
    }

    fn adopt_oauth_tokens(&self, tokens: &n00n_storage::auth::OAuthTokens) {
        let resolved = auth::build_oauth_resolved(tokens);
        let mut current = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if credential_hash(&current) != credential_hash(&resolved) {
            *current = resolved;
            self.auth_generation.fetch_add(1, Ordering::Release);
        }
    }

    async fn synchronize_oauth_tokens(
        &self,
        observed: &n00n_storage::auth::OAuthTokens,
        force_refresh: bool,
        attempt_nonce: u64,
    ) -> Result<n00n_storage::auth::OAuthTokens, AgentError> {
        let storage = self.storage.clone().ok_or_else(|| AgentError::Config {
            message: "OAuth refresh not available for externally-managed auth".into(),
        })?;
        let observed = copy_oauth_tokens(observed);
        let local_wait_started = Instant::now();
        let _refresh_guard = self.auth_refresh.lock().await;
        let local_lock_wait = local_wait_started.elapsed();
        let expiry_bucket = auth_expiry_bucket(&observed);
        let result = smol::unblock(move || {
            auth::synchronize_tokens(&storage, &observed, force_refresh, auth::refresh_tokens)
        })
        .await;
        match result {
            Ok(sync) => {
                debug!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = "auth_refresh",
                    auth_expiry_bucket = expiry_bucket,
                    local_lock_wait_ms = local_lock_wait.as_millis(),
                    refresh_lock_wait_ms = sync.lock_wait.as_millis(),
                    outcome = ?sync.outcome,
                    same_account = sync.same_account,
                    force_refresh,
                    "OpenAI OAuth credential transaction completed"
                );
                self.adopt_oauth_tokens(&sync.tokens);
                Ok(sync.tokens)
            }
            Err(error) => {
                warn!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = "auth_refresh",
                    auth_expiry_bucket = expiry_bucket,
                    local_lock_wait_ms = local_lock_wait.as_millis(),
                    outcome = "failed_preserved",
                    force_refresh,
                    retryable = error.is_retryable(),
                    auth_rejection = error.is_auth_error(),
                    "OpenAI OAuth credential transaction failed"
                );
                Err(error)
            }
        }
    }

    async fn coding_plan_auth(
        &self,
        force_refresh: bool,
        observed: Option<&n00n_storage::auth::OAuthTokens>,
        attempt_nonce: u64,
    ) -> Result<CodingPlanAuth, AgentError> {
        if !self.auth_managed {
            return Ok(CodingPlanAuth {
                resolved: self.current_auth(),
                oauth_tokens: None,
            });
        }
        let storage = self.storage.as_ref().ok_or_else(|| AgentError::Config {
            message: "OpenAI credential storage is unavailable".into(),
        })?;
        let codex_path = self.codex.then(auth::codex_auth_path_from_env).flatten();
        let codex_tokens = codex_path
            .as_deref()
            .map(auth::load_codex_tokens)
            .transpose()?
            .flatten();
        let uses_codex_file = codex_tokens.is_some();
        let Some(tokens) =
            codex_tokens.or_else(|| n00n_storage::auth::load_tokens(storage, auth::PROVIDER))
        else {
            if self.codex {
                return Err(AgentError::Config {
                    message:
                        "OpenAI Codex authentication not available; run `n00n auth login codex`"
                            .into(),
                });
            }
            let mut resolved = auth::resolve_api_key(storage)?;
            if resolved.base_url.is_none() {
                resolved.base_url = Some(CONFIG.base_url.into());
            }
            return Ok(CodingPlanAuth {
                resolved,
                oauth_tokens: None,
            });
        };
        let tokens = if force_refresh || tokens.is_expired() {
            let refresh_basis = match observed {
                Some(observed) => observed,
                None => &tokens,
            };
            if let Some(codex_path) = codex_path.as_deref().filter(|_| uses_codex_file) {
                let path = codex_path.to_path_buf();
                let basis = copy_oauth_tokens(refresh_basis);
                smol::unblock(move || {
                    auth::synchronize_codex_tokens(
                        &path,
                        &basis,
                        force_refresh,
                        auth::refresh_tokens,
                    )
                })
                .await?
                .tokens
            } else {
                self.synchronize_oauth_tokens(refresh_basis, force_refresh, attempt_nonce)
                    .await?
            }
        } else {
            debug!(
                process_instance_nonce = process_instance_nonce(),
                attempt_nonce,
                phase = "auth_preflight",
                auth_expiry_bucket = auth_expiry_bucket(&tokens),
                refresh_lock_wait_ms = 0,
                outcome = "current",
                "OpenAI OAuth access token passed preflight"
            );
            self.adopt_oauth_tokens(&tokens);
            tokens
        };
        Ok(CodingPlanAuth {
            resolved: auth::build_coding_plan_resolved(&tokens),
            oauth_tokens: Some(tokens),
        })
    }

    async fn acquire_coding_plan_admission(
        &self,
        auth: &ResolvedAuth,
        attempt_nonce: u64,
    ) -> Result<Option<auth::CodingPlanAdmission>, AgentError> {
        if auth.base_url.as_deref() != Some(auth::CODING_PLAN_BASE_URL) {
            return Ok(None);
        }
        let storage = self.storage.clone().ok_or_else(|| AgentError::Config {
            message: "OpenAI Coding Plan admission requires local credential storage".into(),
        })?;
        let scope_hash = {
            let storage = storage.clone();
            let auth = auth.clone();
            smol::unblock(move || auth::coding_plan_admission_scope(&storage, &auth)).await?
        };
        let slots = self.coding_plan_slots;
        let (admission, wait) = smol::unblock(move || {
            auth::acquire_coding_plan_admission(&storage, &scope_hash, slots)
        })
        .await?;
        debug!(
            process_instance_nonce = process_instance_nonce(),
            attempt_nonce,
            phase = "request_admission",
            slot = admission.slot(),
            slots,
            wait_ms = wait.as_millis(),
            "acquired OpenAI Coding Plan request admission"
        );
        Ok(Some(admission))
    }

    async fn admission_scope_matches(
        &self,
        admission: Option<&auth::CodingPlanAdmission>,
        auth: &ResolvedAuth,
    ) -> Result<bool, AgentError> {
        let Some(admission) = admission else {
            return Ok(true);
        };
        let storage = self.storage.clone().ok_or_else(|| AgentError::Config {
            message: "OpenAI Coding Plan admission requires local credential storage".into(),
        })?;
        let auth = auth.clone();
        let scope =
            smol::unblock(move || auth::coding_plan_admission_scope(&storage, &auth)).await?;
        Ok(scope == admission.scope_hash())
    }

    async fn pre_send_auth(&self, attempt_nonce: u64) -> Result<PreSendAuth, AgentError> {
        let auth = self.coding_plan_auth(false, None, attempt_nonce).await?;
        Ok(PreSendAuth {
            credential_hash: credential_hash(&auth.resolved),
            resolved: auth.resolved,
            generation: self.auth_generation.load(Ordering::Acquire),
        })
    }

    #[allow(clippy::large_futures)]
    async fn connect_current_websocket(
        &self,
        attempt_nonce: u64,
        session_id: Option<n00nId>,
    ) -> Result<ScopedResponsesWebSocket, super::websocket::WebSocketAttemptError> {
        loop {
            let auth = self
                .pre_send_auth(attempt_nonce)
                .await
                .map_err(not_sent_websocket_error)?;
            if auth.generation != self.auth_generation.load(Ordering::Acquire) {
                continue;
            }
            let websocket_auth = with_codex_session_headers(auth.resolved, session_id);
            let socket = super::websocket::ResponsesWebSocket::connect(
                &websocket_auth,
                self.websocket_connect_timeout,
            )
            .await
            .map_err(not_sent_websocket_error)?;
            return Ok(ScopedResponsesWebSocket {
                socket,
                credential_hash: auth.credential_hash,
                auth_generation: auth.generation,
            });
        }
    }

    async fn with_oauth_retry<T, F, Fut>(&self, f: F) -> Result<T, AgentError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, AgentError>>,
    {
        let observed = self
            .storage
            .as_ref()
            .and_then(|storage| n00n_storage::auth::load_tokens(storage, auth::PROVIDER));
        let result = f().await;
        if self.is_oauth() && matches!(&result, Err(e) if e.is_auth_error()) {
            let Some(observed) = observed else {
                return result;
            };
            if self
                .synchronize_oauth_tokens(&observed, true, fastrand::u64(..))
                .await
                .is_err()
            {
                return result;
            }
            return f().await;
        }
        result
    }

    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    async fn stream_websocket<F>(
        &self,
        slot: Option<ResponseConnectionSlot>,
        body: &Value,
        full_history_body: &mut Option<Value>,
        full_history_fallback_available: bool,
        mut build_full_history: F,
        chain_session: Option<n00nId>,
        admission_scope: Option<&str>,
        event_tx: &Sender<ProviderEvent>,
        _auth: &ResolvedAuth,
        credential_hash: &str,
        stream_timeout: Duration,
        attempt_nonce: u64,
        idempotency_key: Option<String>,
    ) -> Result<(Option<String>, StreamResponse), super::websocket::WebSocketAttemptError>
    where
        F: FnMut() -> Value,
    {
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        let mut reused = false;
        let mut rebuild_full_history = false;
        let mut cleared_connection_chain = false;
        let mut scoped = if let Some(slot) = slot.as_ref() {
            let mut connection = slot.lock().await;
            if connection.as_ref().is_some_and(|connection| {
                connection.socket.should_retire_before_send(stream_timeout)
                    || connection.socket.is_idle()
                    || connection.credential_hash != credential_hash
                    || connection.auth_generation != auth_generation
            }) {
                *connection = None;
            }
            let scoped = connection.take();
            reused = scoped.is_some();
            scoped
        } else {
            None
        };

        if let Some(connection) = scoped.as_mut()
            && reused
            && !connection.socket.is_validated_for_send()
            && connection
                .socket
                .preflight(self.websocket_connect_timeout)
                .await
                .is_err()
        {
            scoped = None;
            reused = false;
            rebuild_full_history = full_history_fallback_available;
        }

        loop {
            if scoped.is_none() {
                if full_history_fallback_available {
                    rebuild_full_history = true;
                }
                if rebuild_full_history && !cleared_connection_chain {
                    self.reset_connection_local_chain(chain_session);
                    cleared_connection_chain = true;
                }
                scoped = Some(
                    self.connect_current_websocket(attempt_nonce, chain_session)
                        .await?,
                );
                reused = false;
            }
            let send_auth = self
                .pre_send_auth(attempt_nonce)
                .await
                .map_err(not_sent_websocket_error)?;
            if let Some(expected_scope) = admission_scope
                && let Some(storage) = self.storage.clone()
            {
                let send_auth = send_auth.resolved.clone();
                let final_scope =
                    smol::unblock(move || auth::coding_plan_admission_scope(&storage, &send_auth))
                        .await
                        .map_err(not_sent_websocket_error)?;
                if final_scope != expected_scope {
                    return Err(not_sent_websocket_error(
                        AgentError::CodingPlanAdmissionScopeChanged,
                    ));
                }
            }
            let stale = scoped.as_ref().is_none_or(|connection| {
                connection.socket.should_retire_before_send(stream_timeout)
                    || connection.credential_hash != send_auth.credential_hash
                    || connection.auth_generation != send_auth.generation
            });
            if stale || send_auth.generation != self.auth_generation.load(Ordering::Acquire) {
                if let Some(connection) = scoped.as_ref() {
                    debug!(
                        process_instance_nonce = process_instance_nonce(),
                        attempt_nonce,
                        phase = "auth_pre_send",
                        socket_age_secs = connection.socket.age().as_secs(),
                        reused,
                        auth_generation_current =
                            connection.auth_generation == send_auth.generation,
                        credential_current =
                            connection.credential_hash == send_auth.credential_hash,
                        "discarding stale OpenAI Responses WebSocket before request send"
                    );
                }
                scoped = None;
                reused = false;
                rebuild_full_history |= full_history_fallback_available;
                continue;
            }

            let Some(mut connection) = scoped.take() else {
                continue;
            };
            let result = connection
                .socket
                .stream_message(
                    if rebuild_full_history {
                        full_history_body.get_or_insert_with(&mut build_full_history)
                    } else {
                        body
                    },
                    event_tx,
                    stream_timeout,
                    idempotency_key.clone(),
                )
                .await;
            match &result {
                Ok(_) => debug!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = "response_complete",
                    socket_age_secs = connection.socket.age().as_secs(),
                    accepted = true,
                    "OpenAI Responses WebSocket attempt completed"
                ),
                Err(error) => debug!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = ?error.delivery.phase,
                    socket_age_secs = connection.socket.age().as_secs(),
                    request_sent = error.request_sent(),
                    accepted = error.delivery.phase == RequestDeliveryPhase::Accepted,
                    response_id_present = error.delivery.response_id.is_some(),
                    close_code_present = error.delivery.close_code.is_some(),
                    close_reason_present = error.delivery.close_reason.is_some(),
                    transport_failure = error.transport_failure,
                    "OpenAI Responses WebSocket attempt failed"
                ),
            }
            if result.is_ok()
                && connection.auth_generation == self.auth_generation.load(Ordering::Acquire)
                && let Some(slot) = slot.as_ref()
            {
                let mut pooled = slot.lock().await;
                *pooled = Some(connection);
            }
            return result;
        }
    }

    async fn prepare_request<'a>(
        &self,
        session_id: Option<&SessionRef>,
        fingerprint: &CachePrefixFingerprint,
        auth_scope_hash: &str,
        messages: &'a [Message],
        response_chain_lock: Option<&OpenAiResponseChainLock>,
    ) -> Result<(Option<String>, &'a [Message]), AgentError> {
        let Some(session_id) = session_id else {
            return Ok((None, messages));
        };
        let session_id = canonical_session_key(session_id);

        let needs_load = {
            let mut states = self
                .session_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let now_epoch = now_epoch();
            states.retain(|_, state| {
                now.saturating_duration_since(state.last_used) < SESSION_STATE_TTL
                    && (state.last_response_id.is_none() || state.expires_at > now_epoch)
            });
            self.response_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|session_id, _| states.contains_key(session_id));
            !states.contains_key(&session_id)
        };

        if needs_load || response_chain_lock.is_some() {
            let loaded = if let (Some(storage), Some(lock)) = (
                self.response_state_storage.clone(),
                response_chain_lock
                    .map(OpenAiResponseChainLock::try_clone)
                    .transpose()?,
            ) {
                match smol::unblock(move || load_openai_response_chain(&storage, session_id, &lock))
                    .await
                {
                    Ok(chain) => chain.map(OpenAiSessionState::from_stored),
                    Err(error) => {
                        warn!(error = %error, "failed to load OpenAI response chain; using full history");
                        None
                    }
                }
            } else {
                None
            };
            debug!(
                chain_restore = loaded
                    .as_ref()
                    .is_some_and(|state| state.last_response_id.is_some()),
                durable_reload = response_chain_lock.is_some(),
                "loaded durable OpenAI response chain state"
            );
            let state = loaded.unwrap_or_else(OpenAiSessionState::default);
            let mut states = self
                .session_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if response_chain_lock.is_some() {
                states.insert(session_id, state);
            } else {
                states.entry(session_id).or_insert(state);
            }
        }

        let mut states = self
            .session_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let state = states.entry(session_id).or_default();
        state.last_used = now;
        incremental_for_state(state, fingerprint, auth_scope_hash, messages)
            .map_err(AgentError::Json)
    }

    async fn record_response(
        &self,
        session_id: Option<&SessionRef>,
        response_id: Option<String>,
        fingerprint: &CachePrefixFingerprint,
        auth_scope_hash: &str,
        messages: &[Message],
        persist: bool,
        response_chain_lock: Option<&OpenAiResponseChainLock>,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let session_id = canonical_session_key(session_id);
        let stored = {
            let mut states = self
                .session_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = states.entry(session_id).or_default();
            state.last_used = Instant::now();
            if let Err(error) =
                record_in_state(state, response_id, fingerprint, auth_scope_hash, messages)
            {
                warn!(error = %error, "failed to hash OpenAI response chain; clearing continuation state");
                *state = OpenAiSessionState::default();
            }
            state.to_stored()
        };
        let Some(storage) = self.response_state_storage.clone() else {
            return;
        };
        let Some(response_chain_lock) = response_chain_lock else {
            return;
        };
        let result = match response_chain_lock.try_clone() {
            Ok(lock) => {
                smol::unblock(move || match (persist, stored) {
                    (true, Some(stored)) => {
                        save_openai_response_chain(&storage, session_id, &stored, &lock)
                    }
                    (false, _) | (true, None) => {
                        delete_openai_response_chain(&storage, session_id, &lock)
                    }
                })
                .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if matches!(&error, n00n_storage::StorageError::NotFound(_)) {
                self.clear_local_response_chain(session_id);
            } else {
                warn!(error = %error, "failed to persist OpenAI response chain; keeping in-memory state");
            }
        }
    }

    async fn emit_cache_health(
        &self,
        session_id: Option<&SessionRef>,
        hit: bool,
        event_tx: &Sender<ProviderEvent>,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let session_id = canonical_session_key(session_id);
        let health = {
            let states = self
                .session_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            states.get(&session_id).map_or_else(
                || CacheHealth {
                    kind: CacheKind::ResponseChain,
                    valid_until: 0,
                    ttl_seconds: 0,
                    hit: false,
                },
                |state| match state.last_response_id.as_ref() {
                    Some(_) => CacheHealth {
                        kind: CacheKind::ResponseChain,
                        valid_until: state.expires_at,
                        ttl_seconds: OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
                        hit,
                    },
                    None => CacheHealth {
                        kind: CacheKind::ResponseChain,
                        valid_until: 0,
                        ttl_seconds: 0,
                        hit: false,
                    },
                },
            )
        };
        if let Err(error) = event_tx
            .send_async(ProviderEvent::CacheHealth { cache: health })
            .await
        {
            warn!(error = %error, "failed to send cache health event");
        }
    }

    fn remove_local_session_state(&self, session_id: n00nId) {
        self.session_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    fn remove_local_response_connection(&self, session_id: n00nId) {
        self.response_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    fn reset_connection_local_chain(&self, session_id: Option<n00nId>) {
        if let Some(session_id) = session_id {
            self.remove_local_session_state(session_id);
        }
    }

    fn clear_local_response_chain(&self, session_id: n00nId) {
        self.remove_local_session_state(session_id);
        self.remove_local_response_connection(session_id);
    }

    async fn clear_response_chain(
        &self,
        session_id: Option<&SessionRef>,
        response_chain_lock: Option<&OpenAiResponseChainLock>,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let session_id = canonical_session_key(session_id);
        self.session_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id, OpenAiSessionState::default());
        self.response_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
        let Some(storage) = self.response_state_storage.clone() else {
            return;
        };
        let Some(response_chain_lock) = response_chain_lock else {
            return;
        };
        let result = match response_chain_lock.try_clone() {
            Ok(lock) => {
                smol::unblock(move || delete_openai_response_chain(&storage, session_id, &lock))
                    .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            warn!(error = %error, "failed to clear stale OpenAI response chain from disk");
        }
    }

    async fn finish_codex_attempt(
        &self,
        attempt: CodexAttempt,
        session_id: Option<&SessionRef>,
        response_chain_lock: Option<&OpenAiResponseChainLock>,
        event_tx: &Sender<ProviderEvent>,
    ) -> CodexAttempt {
        if attempt.previous_response_id.is_some() {
            let reset_reason = if is_missing_previous_response(&attempt) {
                Some(ResponseChainResetReason::PreviousResponseNotFound)
            } else if should_clear_response_chain(&attempt.result) {
                Some(ResponseChainResetReason::AttemptFailed)
            } else {
                None
            };
            if let Some(reason) = reset_reason {
                log_response_chain_reset(reason, Some(response_chain_lock.is_some()));
                self.clear_response_chain(session_id, response_chain_lock)
                    .await;
                self.emit_cache_health(session_id, false, event_tx).await;
            }
        }
        attempt
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::large_futures)]
    async fn run_codex_attempt(
        &self,
        model: &Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        tools_hash: &str,
        event_tx: &Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&SessionRef>,
        durable_chain: bool,
        auth: &ResolvedAuth,
        attempt_nonce: u64,
    ) -> CodexAttempt {
        let state_scope_hash = response_state_scope_hash(auth);
        let socket_credential_hash = credential_hash(auth);
        // Without server-side response storage, a WebSocket reconnection must be able to replay
        // full history instead of relying on a continuation chain.
        let mut opts = opts.with_idempotency_key().with_idempotency_supported();
        opts.allow_history_replay = true;
        // The OpenAI Coding Plan endpoint has historically rejected
        // `prompt_cache_options`, so Codex keeps those fields disabled unless
        // a manual capability probe has proven this account/endpoint accepts
        // documented cache options.
        opts = self.codex_cache_capabilities.apply_to_request_options(opts);
        let admission = match self
            .acquire_coding_plan_admission(auth, attempt_nonce)
            .await
        {
            Ok(admission) => admission,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    emitted_event: false,
                    definitive_rejection: false,
                    delivery: Some(RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent)),
                    result: Err(error),
                };
            }
        };
        let response_chain_lock = if durable_chain {
            match self.lock_response_chain(session_id).await {
                Ok(lock) => lock,
                Err(error) => {
                    return CodexAttempt {
                        previous_response_id: None,
                        emitted_event: false,
                        definitive_rejection: false,
                        delivery: Some(RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent)),
                        result: Err(error),
                    };
                }
            }
        } else {
            None
        };
        let persist_response_chain = response_chain_lock.is_some();
        // The OpenAI Coding Plan endpoint rejects server-side `store=true`.
        let store = false;
        let stream_timeout = self.compat.stream_timeout();
        let connection_reusable = self
            .response_connection_is_reusable(
                session_id,
                &socket_credential_hash,
                stream_timeout,
                attempt_nonce,
            )
            .await;
        // A `store: false` response is held only in the connection-local cache of
        // the socket that produced it, so a durable chain id cannot outlive that
        // socket. Reset the chain whenever the socket is gone, or the next turn
        // replays a dead id and the endpoint answers `previous_response_not_found`.
        if !connection_reusable {
            log_response_chain_reset(
                ResponseChainResetReason::SocketNotReusable,
                Some(persist_response_chain),
            );
            self.clear_response_chain(session_id, response_chain_lock.as_ref())
                .await;
        }
        let fingerprint = CachePrefixFingerprint::new(&model.id, system, tools_hash);
        let (previous_response_id, incremental_messages) = match self
            .prepare_request(
                session_id,
                &fingerprint,
                &state_scope_hash,
                messages,
                response_chain_lock.as_ref(),
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    emitted_event: false,
                    definitive_rejection: false,
                    delivery: None,
                    result: Err(error),
                };
            }
        };
        if full_history_replay_required(
            previous_response_id.as_deref(),
            messages.len(),
            opts.protect_history_replay,
            opts.allow_history_replay,
        ) {
            return CodexAttempt {
                previous_response_id: None,
                emitted_event: false,
                definitive_rejection: false,
                delivery: Some(RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent)),
                result: Err(AgentError::HistoryReplayRequired {
                    reason: HistoryReplayReason::ContinuationUnavailable,
                }),
            };
        }
        self.emit_cache_health(session_id, previous_response_id.is_some(), event_tx)
            .await;
        let prompt_cache_key = fingerprint.prompt_cache_key(session_id);
        let body = super::websocket::build_request_body(
            model,
            incremental_messages,
            system,
            tools,
            previous_response_id.as_deref(),
            Some(&prompt_cache_key),
            store,
            &opts,
            true,
        );
        let mut full_history_body = None;
        let full_history_fallback_available = previous_response_id.is_some()
            && !persist_response_chain
            && (!opts.protect_history_replay || opts.allow_history_replay);
        log_responses_request(
            "websocket",
            &body,
            messages.len(),
            incremental_messages.len(),
            previous_response_id.is_some(),
            false,
        );
        let admission_scope = admission
            .as_ref()
            .map(|admission| admission.scope_hash().to_owned());
        let (response_id, response, chainable) = {
            let admission_guard = admission;
            let connection_slot = self.response_connection_slot(session_id);
            let websocket_result = self
                .stream_websocket(
                    connection_slot,
                    &body,
                    &mut full_history_body,
                    full_history_fallback_available,
                    || {
                        super::websocket::build_request_body(
                            model,
                            messages,
                            system,
                            tools,
                            None,
                            Some(&prompt_cache_key),
                            store,
                            &opts,
                            true,
                        )
                    },
                    session_id.map(canonical_session_key),
                    admission_scope.as_deref(),
                    event_tx,
                    auth,
                    &socket_credential_hash,
                    stream_timeout,
                    attempt_nonce,
                    opts.idempotency_supported
                        .then(|| opts.idempotency_key.clone())
                        .flatten(),
                )
                .await;
            match websocket_result {
                Ok((response_id, response)) => (response_id, response, true),
                Err(error) if should_fallback_to_http(&error) => {
                    if previous_response_id.is_some()
                        && opts.protect_history_replay
                        && !opts.allow_history_replay
                    {
                        return self
                            .finish_codex_attempt(
                                CodexAttempt {
                                    previous_response_id,
                                    emitted_event: false,
                                    definitive_rejection: false,
                                    delivery: Some(*error.delivery),
                                    result: Err(AgentError::HistoryReplayRequired {
                                        reason: HistoryReplayReason::ContinuationUnavailable,
                                    }),
                                },
                                session_id,
                                response_chain_lock.as_ref(),
                                event_tx,
                            )
                            .await;
                    }
                    // Read before the fallback consumes `error`: the HTTP probe
                    // may reject with an empty 403 that carries no delay.
                    let ws_retry_after = admission_retry_after(error.error.as_ref());
                    warn!("OpenAI Responses WebSocket unavailable; falling back to HTTP");
                    let fallback_body = if persist_response_chain {
                        &body
                    } else {
                        full_history_body.get_or_insert_with(|| {
                            super::websocket::build_request_body(
                                model,
                                messages,
                                system,
                                tools,
                                None,
                                Some(&prompt_cache_key),
                                false,
                                &opts,
                                true,
                            )
                        })
                    };
                    log_responses_request(
                        "http_sse",
                        fallback_body,
                        messages.len(),
                        if persist_response_chain {
                            incremental_messages.len()
                        } else {
                            messages.len()
                        },
                        persist_response_chain && previous_response_id.is_some(),
                        !persist_response_chain,
                    );
                    let fallback_auth = loop {
                        let preflight = match self.pre_send_auth(attempt_nonce).await {
                            Ok(auth) => auth,
                            Err(error) => {
                                return self
                                    .finish_codex_attempt(
                                        CodexAttempt {
                                            previous_response_id,
                                            emitted_event: false,
                                            definitive_rejection: false,
                                            delivery: Some(RequestDeliveryMetadata::new(
                                                RequestDeliveryPhase::NotSent,
                                            )),
                                            result: Err(error),
                                        },
                                        session_id,
                                        response_chain_lock.as_ref(),
                                        event_tx,
                                    )
                                    .await;
                            }
                        };
                        if preflight.generation == self.auth_generation.load(Ordering::Acquire) {
                            break preflight;
                        }
                    };
                    match self
                        .admission_scope_matches(admission_guard.as_ref(), &fallback_auth.resolved)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            return self
                                .finish_codex_attempt(
                                    CodexAttempt {
                                        previous_response_id,
                                        emitted_event: false,
                                        definitive_rejection: false,
                                        delivery: Some(RequestDeliveryMetadata::new(
                                            RequestDeliveryPhase::NotSent,
                                        )),
                                        result: Err(AgentError::CodingPlanAdmissionScopeChanged),
                                    },
                                    session_id,
                                    response_chain_lock.as_ref(),
                                    event_tx,
                                )
                                .await;
                        }
                        Err(error) => {
                            return self
                                .finish_codex_attempt(
                                    CodexAttempt {
                                        previous_response_id,
                                        emitted_event: false,
                                        definitive_rejection: false,
                                        delivery: Some(RequestDeliveryMetadata::new(
                                            RequestDeliveryPhase::NotSent,
                                        )),
                                        result: Err(error),
                                    },
                                    session_id,
                                    response_chain_lock.as_ref(),
                                    event_tx,
                                )
                                .await;
                        }
                    }
                    let fallback_auth = with_codex_session_headers(
                        fallback_auth.resolved,
                        session_id.map(canonical_session_key),
                    );
                    match super::responses::do_stream(
                        self.compat.client(),
                        model,
                        fallback_body,
                        event_tx,
                        &fallback_auth,
                        stream_timeout,
                        &opts,
                    )
                    .await
                    {
                        Ok((response_id, response)) => {
                            (response_id, response, persist_response_chain)
                        }
                        Err(error) => {
                            return self
                                .finish_codex_attempt(
                                    http_fallback_attempt(
                                        previous_response_id,
                                        error,
                                        ws_retry_after,
                                    ),
                                    session_id,
                                    response_chain_lock.as_ref(),
                                    event_tx,
                                )
                                .await;
                        }
                    }
                }
                Err(error) => {
                    return self
                        .finish_codex_attempt(
                            CodexAttempt::from_websocket_error(previous_response_id, error),
                            session_id,
                            response_chain_lock.as_ref(),
                            event_tx,
                        )
                        .await;
                }
            }
        };
        self.record_response(
            session_id,
            chainable.then_some(response_id).flatten(),
            &fingerprint,
            &state_scope_hash,
            messages,
            persist_response_chain,
            response_chain_lock.as_ref(),
        )
        .await;
        self.emit_cache_health(session_id, previous_response_id.is_some(), event_tx)
            .await;
        CodexAttempt {
            previous_response_id,
            emitted_event: false,
            definitive_rejection: false,
            delivery: None,
            result: Ok(response),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::large_futures)]
    async fn run_codex_attempt_with_auth_retry(
        &self,
        model: &Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        tools_hash: &str,
        event_tx: &Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&SessionRef>,
        durable_chain: bool,
    ) -> CodexAttempt {
        let attempt_nonce = fastrand::u64(..);
        let mut coding_plan_auth = match self.coding_plan_auth(false, None, attempt_nonce).await {
            Ok(auth) => auth,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    emitted_event: false,
                    definitive_rejection: false,
                    delivery: None,
                    result: Err(error),
                };
            }
        };
        let mut admission_retries = 0_u8;
        loop {
            let attempt = self
                .run_codex_attempt(
                    model,
                    messages,
                    system,
                    tools,
                    tools_hash,
                    event_tx,
                    opts.clone(),
                    session_id,
                    durable_chain,
                    &coding_plan_auth.resolved,
                    attempt_nonce,
                )
                .await;
            if attempt.should_reacquire_admission() {
                let Ok(current) = self.coding_plan_auth(false, None, attempt_nonce).await else {
                    return attempt;
                };
                coding_plan_auth = current;
                continue;
            }
            if let Some(delay) = coding_plan_admission_retry_delay(&attempt, admission_retries) {
                admission_retries += 1;
                // A server-directed `Retry-After` is clamped at 30s and up to
                // `CODING_PLAN_ADMISSION_MAX_RETRIES` of them can stack, so a
                // turn can stall for minutes. Say so above `debug`, or the
                // stall has no visible cause.
                if delay >= CODING_PLAN_LOUD_RETRY_DELAY {
                    warn!(
                        retry_delay_secs = delay.as_secs(),
                        attempt = admission_retries,
                        of = CODING_PLAN_ADMISSION_MAX_RETRIES,
                        "OpenAI Coding Plan asked us to wait before retrying"
                    );
                }
                debug!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = "request_admission_retry",
                    retry_delay_ms = delay.as_millis(),
                    "retrying OpenAI Coding Plan admission"
                );
                smol::Timer::after(delay).await;
                continue;
            }
            let Some(observed) = coding_plan_auth.oauth_tokens.as_ref() else {
                return attempt;
            };
            if !attempt.should_retry_after_oauth_refresh() {
                return attempt;
            }

            let Ok(refreshed) = self
                .coding_plan_auth(true, Some(observed), attempt_nonce)
                .await
            else {
                return attempt;
            };
            coding_plan_auth = refreshed;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_responses_attempt(
        &self,
        model: &Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        tools_hash: &str,
        event_tx: &Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&SessionRef>,
    ) -> Result<StreamResponse, AgentError> {
        // API-key Responses requests are intentionally stateless. This HTTP path cannot
        // reuse a store=false response ID safely, so every turn sends full history.
        let opts = clamp_responses_cache_breakpoints(model, opts);
        let fingerprint = CachePrefixFingerprint::new(&model.id, system, tools_hash);
        let prompt_cache_key = fingerprint.prompt_cache_key(session_id);
        let body = super::responses::build_body(
            model,
            messages,
            system,
            tools,
            None,
            Some(&prompt_cache_key),
            false,
            &opts,
            true,
        );

        log_responses_request(
            "http_sse",
            &body,
            messages.len(),
            messages.len(),
            false,
            false,
        );

        self.with_oauth_retry(|| async {
            let auth = self.current_auth();
            super::responses::do_stream(
                self.compat.client(),
                model,
                &body,
                event_tx,
                &auth,
                self.compat.stream_timeout(),
                &opts,
            )
            .await
        })
        .await
        .map(|(_, response)| response)
    }

    fn response_connection_slot(
        &self,
        session_id: Option<&SessionRef>,
    ) -> Option<ResponseConnectionSlot> {
        let session_id = session_id?;
        let session_id = canonical_session_key(session_id);
        let mut connections = self
            .response_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = connections
            .entry(session_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)));
        Some(Arc::clone(slot))
    }

    async fn response_connection_is_reusable(
        &self,
        session_id: Option<&SessionRef>,
        credential_hash: &str,
        stream_timeout: Duration,
        attempt_nonce: u64,
    ) -> bool {
        let Some(session_id) = session_id else {
            return false;
        };
        let session_id = canonical_session_key(session_id);
        let slot = self
            .response_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .map(Arc::clone);
        let Some(slot) = slot else {
            return false;
        };
        let mut connection = slot.lock().await;
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        let reusable = connection.as_ref().is_some_and(|connection| {
            !connection.socket.should_retire_before_send(stream_timeout)
                && !connection.socket.is_idle()
                && connection.credential_hash == credential_hash
                && connection.auth_generation == auth_generation
        });
        if !reusable {
            if let Some(scoped) = connection.as_ref() {
                debug!(
                    process_instance_nonce = process_instance_nonce(),
                    attempt_nonce,
                    phase = "socket_reuse_check",
                    socket_age_secs = scoped.socket.age().as_secs(),
                    socket_idle_secs = scoped.socket.idle_for().as_secs(),
                    retired = scoped.socket.should_retire_before_send(stream_timeout),
                    credential_current = scoped.credential_hash == credential_hash,
                    auth_generation_current = scoped.auth_generation == auth_generation,
                    outcome = "replace",
                    "OpenAI Responses WebSocket is not reusable"
                );
            }
            *connection = None;
            return false;
        }
        // The liveness ping belongs to `stream_websocket`, after the account-scoped
        // admission permit has been acquired. Do not issue network traffic here.
        true
    }

    fn response_operation_slot(
        &self,
        session_id: Option<&SessionRef>,
    ) -> Option<ResponseOperationSlot> {
        let session_id = canonical_session_key(session_id?);
        let storage_path = self.response_state_storage.as_ref()?.path().to_path_buf();
        let mut operations = RESPONSE_OPERATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (storage_path, session_id);
        operations.retain(|_, operation| operation.strong_count() > 0);
        if let Some(operation) = operations.get(&key).and_then(Weak::upgrade) {
            return Some(operation);
        }
        let operation = Arc::new(AsyncMutex::new(()));
        operations.insert(key, Arc::downgrade(&operation));
        Some(operation)
    }
}

#[derive(Deserialize)]
struct RateLimitStatusResponse {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimitStatusDetails>,
    #[serde(default)]
    credits: Option<CreditStatusDetails>,
    #[serde(default, rename = "additional_rate_limits")]
    additional_rate_limits: Option<Vec<AdditionalRateLimitDetails>>,
}

#[derive(Deserialize)]
struct RateLimitStatusDetails {
    #[serde(default)]
    primary_window: Option<RateLimitWindowSnapshot>,
    #[serde(default)]
    secondary_window: Option<RateLimitWindowSnapshot>,
}

#[derive(Deserialize)]
struct RateLimitWindowSnapshot {
    used_percent: i32,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct AdditionalRateLimitDetails {
    limit_name: String,
    #[serde(default)]
    rate_limit: Option<RateLimitStatusDetails>,
}

#[derive(Deserialize)]
struct CreditStatusDetails {
    has_credits: bool,
    unlimited: bool,
    #[serde(default)]
    balance: Option<String>,
}

impl RateLimitWindowSnapshot {
    fn to_limit(&self, is_secondary: bool, prefix: &str) -> UsageLimit {
        let duration = rate_limit_window_label(self.limit_window_seconds)
            .unwrap_or_else(|| if is_secondary { "weekly" } else { "5h" });
        let duration = capitalize_first(duration);
        let label = if prefix.is_empty() {
            format!("{duration} limit")
        } else {
            format!("{prefix} {duration} limit")
        };
        UsageLimit {
            label,
            percentage: Some(percentage(self.used_percent)),
            reset_at: self
                .reset_at
                .and_then(|s| u64::try_from(s).ok().map(|s| s.saturating_mul(1_000))),
            detail: None,
        }
    }
}

impl From<RateLimitStatusResponse> for ProviderUsage {
    fn from(resp: RateLimitStatusResponse) -> Self {
        let mut limits = Vec::new();
        if let Some(rate_limit) = &resp.rate_limit {
            add_rate_limit_windows(&mut limits, rate_limit, "");
        }
        for additional in resp.additional_rate_limits.into_iter().flatten() {
            let prefix = rate_limit_prefix(&additional.limit_name);
            if let Some(rate_limit) = &additional.rate_limit {
                add_rate_limit_windows(&mut limits, rate_limit, &prefix);
            }
        }
        limits.extend(credits_limit(resp.credits));
        Self {
            plan: resp.plan_type,
            limits,
        }
    }
}

fn add_rate_limit_windows(
    limits: &mut Vec<UsageLimit>,
    rate_limit: &RateLimitStatusDetails,
    prefix: &str,
) {
    if let Some(window) = &rate_limit.primary_window {
        limits.push(window.to_limit(false, prefix));
    }
    if let Some(window) = &rate_limit.secondary_window {
        limits.push(window.to_limit(true, prefix));
    }
}

fn rate_limit_window_label(seconds: Option<i64>) -> Option<&'static str> {
    let seconds = seconds?;
    if is_approximate_window(seconds, USAGE_WINDOW_5HOURS_SECONDS) {
        Some("5h")
    } else if is_approximate_window(seconds, USAGE_WINDOW_1DAY_SECONDS) {
        Some("daily")
    } else if is_approximate_window(seconds, USAGE_WINDOW_1WEEK_SECONDS) {
        Some("weekly")
    } else if is_approximate_window(seconds, USAGE_WINDOW_1MONTH_SECONDS) {
        Some("monthly")
    } else if is_approximate_window(seconds, USAGE_WINDOW_1YEAR_SECONDS) {
        Some("annual")
    } else {
        None
    }
}

fn is_approximate_window(value: i64, expected: i64) -> bool {
    let Ok(value) = i32::try_from(value) else {
        return false;
    };
    let Ok(expected) = i32::try_from(expected) else {
        return false;
    };
    let value = f64::from(value);
    let expected = f64::from(expected);
    (value - expected).abs() <= expected * USAGE_WINDOW_TOLERANCE
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn rate_limit_prefix(limit_name: &str) -> String {
    limit_name
        .split('_')
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

fn credits_limit(credits: Option<CreditStatusDetails>) -> Option<UsageLimit> {
    let credits = credits?;
    if !credits.has_credits {
        return None;
    }
    let detail = if credits.unlimited {
        Some("Unlimited credits".into())
    } else {
        credits.balance.map(|b| format!("${b} remaining"))
    };
    Some(UsageLimit {
        label: "Credits".into(),
        percentage: None,
        reset_at: None,
        detail,
    })
}

fn percentage(used_percent: i32) -> u32 {
    used_percent.clamp(0, 100).cast_unsigned()
}

impl Provider for OpenAi {
    #[allow(clippy::large_futures)]
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
            let prefixed_system = system_with_prefix(self.system_prefix.as_deref(), system);

            if self.codex {
                let operation_slot = self.response_operation_slot(session_id);
                let _operation_guard = match operation_slot.as_ref() {
                    Some(operation) => Some(operation.lock().await),
                    None => None,
                };
                let durable_chain = session_id.is_some() && self.response_state_storage.is_some();
                let tools_hash = request_tools_hash(tools, &opts)?;
                let attempt = self
                    .run_codex_attempt_with_auth_retry(
                        model,
                        messages,
                        &prefixed_system,
                        tools,
                        &tools_hash,
                        event_tx,
                        opts.clone(),
                        session_id,
                        durable_chain,
                    )
                    .await;
                if attempt.previous_response_id.is_none() {
                    return attempt.result;
                }
                if !is_missing_previous_response(&attempt) {
                    return attempt.result;
                }

                if opts.protect_history_replay && !opts.allow_history_replay {
                    return Err(AgentError::HistoryReplayRequired {
                        reason: HistoryReplayReason::ContinuationNotFound,
                    });
                }
                info!(
                    chain_reset = true,
                    full_history_fallback = true,
                    "OpenAI Responses chain was not found; replaying approved full history"
                );
                return self
                    .run_codex_attempt_with_auth_retry(
                        model,
                        messages,
                        &prefixed_system,
                        tools,
                        &tools_hash,
                        event_tx,
                        opts.clone(),
                        session_id,
                        durable_chain,
                    )
                    .await
                    .result;
            }

            let tools_hash = request_tools_hash(tools, &opts)?;

            // Try Responses API for supported models
            if model.supports_responses() {
                let result = self
                    .run_responses_attempt(
                        model,
                        messages,
                        &prefixed_system,
                        tools,
                        &tools_hash,
                        event_tx,
                        opts.clone(),
                        session_id,
                    )
                    .await;

                match result {
                    Ok(response) => return Ok(response),
                    Err(error) if is_definitive_responses_rejection(&error) => {
                        warn!(
                            error = %error,
                            "OpenAI Responses API rejected request; falling back to Chat Completions"
                        );
                    }
                    Err(error) => return Err(error),
                }
            }

            // Fallback to Chat Completions
            let fingerprint = CachePrefixFingerprint::new(&model.id, &prefixed_system, &tools_hash);
            let prompt_cache_key = fingerprint.prompt_cache_key(session_id);
            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                Some(&prompt_cache_key),
                self.system_prefix.as_deref(),
                opts.message_cache_breakpoints,
                opts.fast,
            );
            opts.thinking.apply_thinking(
                &mut body,
                model,
                &dialect::STANDARD,
                &super::super::reasoning_effort_fields(),
            );
            super::super::apply_body_overrides(&mut body, model, &[super::super::MESSAGES_FIELD]);
            self.with_oauth_retry(|| async {
                let auth = self.current_auth();
                self.compat
                    .do_stream(model, &[], &body, event_tx, &auth, &opts)
                    .await
            })
            .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async {
            let entries = if self.codex {
                super::codex_models()
            } else {
                super::models()
            };

            let mut models = if self.codex {
                entries
                    .iter()
                    .flat_map(|e| e.prefixes.iter())
                    .map(|&s| crate::model::ModelInfo::id_only(s.to_string()))
                    .collect()
            } else {
                self.with_oauth_retry(|| async {
                    let auth = self.current_auth();
                    self.compat.do_list_models(&auth).await
                })
                .await?
            };

            super::sort_models(&mut models, entries);
            Ok(models)
        })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async move {
            if !self.codex {
                return Ok(None);
            }
            let auth = self
                .coding_plan_auth(false, None, fastrand::u64(..))
                .await?
                .resolved;
            if auth.base_url.as_deref() != Some(auth::CODING_PLAN_BASE_URL) {
                return Ok(None);
            }
            let body = self.compat.get_text(&auth, USAGE_URL).await?;
            let parsed: RateLimitStatusResponse = serde_json::from_str(&body)?;
            Ok(Some(parsed.into()))
        })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            if !self.is_oauth() {
                return Ok(());
            }
            let storage = self.storage.as_ref().ok_or_else(|| AgentError::Config {
                message: "OpenAI credential storage is unavailable".into(),
            })?;
            let observed =
                n00n_storage::auth::load_tokens(storage, auth::PROVIDER).ok_or_else(|| {
                    AgentError::Api {
                        status: 401,
                        message: "OpenAI OAuth credentials are no longer available".into(),
                    }
                })?;
            self.synchronize_oauth_tokens(&observed, true, fastrand::u64(..))
                .await?;
            Ok(())
        })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            let Some(storage) = self.storage.clone() else {
                return Ok(());
            };
            let _refresh_guard = self.auth_refresh.lock().await;
            let codex = self.codex;
            let resolved = smol::unblock(move || {
                if codex {
                    auth::resolve_coding_plan(&storage)
                } else {
                    auth::resolve_api_key(&storage)
                }
            })
            .await?;
            let previous_scope = credential_hash(&self.current_auth());
            let resolved_scope = credential_hash(&resolved);
            *self
                .auth
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = resolved;
            if previous_scope != resolved_scope {
                self.auth_generation.fetch_add(1, Ordering::Release);
            }
            debug!("reloaded OpenAI auth from storage");
            Ok(())
        })
    }

    fn adjust_model(&self, model: &mut Model) {
        if self.codex {
            model.context_window = model.context_window.min(CODING_PLAN_CONTEXT_WINDOW);
        }
    }

    fn supports_hosted_tool_search(&self, model: &Model) -> bool {
        if !model.supports_responses() || !model.supports_tool_search() {
            return false;
        }
        let auth = self.current_auth();
        auth.base_url.as_deref().is_none_or(|base_url| {
            base_url == super::OPENAI_API_BASE_URL || base_url == auth::CODING_PLAN_BASE_URL
        })
    }
}

/// Exponential backoff with full jitter over `[delay / 2, delay]`, never below
/// [`CODING_PLAN_DEFAULT_RETRY_DELAY`]. Jitter keeps concurrent n00n processes
/// on the same account from retrying in lockstep and re-colliding on whatever
/// capacity limit rejected them.
fn coding_plan_backoff(retry_count: u8, jitter: f64) -> Duration {
    let exponent = u32::from(retry_count.min(CODING_PLAN_ADMISSION_MAX_RETRIES));
    let ceiling = CODING_PLAN_DEFAULT_RETRY_DELAY
        .saturating_mul(1_u32 << exponent)
        .min(CODING_PLAN_MAX_RETRY_DELAY);
    // Full jitter halves the ceiling, which would put the first retry at half
    // of `CODING_PLAN_DEFAULT_RETRY_DELAY`. The clamped `Retry-After` branch
    // treats that constant as a hard floor, so this branch honours it too.
    ceiling
        .mul_f64(0.5 + 0.5 * jitter.clamp(0.0, 1.0))
        .max(CODING_PLAN_DEFAULT_RETRY_DELAY)
}

fn coding_plan_admission_retry_delay(attempt: &CodexAttempt, retry_count: u8) -> Option<Duration> {
    coding_plan_admission_retry_delay_with_jitter(attempt, retry_count, fastrand::f64())
}

fn coding_plan_admission_retry_delay_with_jitter(
    attempt: &CodexAttempt,
    retry_count: u8,
    jitter: f64,
) -> Option<Duration> {
    if attempt.emitted_event
        || !matches!(
            &attempt.delivery,
            Some(RequestDeliveryMetadata {
                phase: RequestDeliveryPhase::NotSent,
                response_id: None,
                ..
            })
        )
    {
        return None;
    }
    if retry_count >= CODING_PLAN_ADMISSION_MAX_RETRIES {
        return None;
    }
    match &attempt.result {
        // A server-directed `Retry-After` overrides the local schedule; jitter is
        // not added on top of a delay the server chose.
        Err(AgentError::CodingPlanAdmission {
            retry_after: Some(delay),
            ..
        }) if attempt.definitive_rejection => {
            Some((*delay).clamp(CODING_PLAN_DEFAULT_RETRY_DELAY, CODING_PLAN_MAX_RETRY_AFTER))
        }
        Err(AgentError::CodingPlanAdmission {
            retry_after: None, ..
        }) if attempt.definitive_rejection => Some(coding_plan_backoff(retry_count, jitter)),
        Err(AgentError::CodingPlanAdmissionTimeout { .. }) => {
            Some(coding_plan_backoff(retry_count, jitter))
        }
        _ => None,
    }
}

fn is_missing_previous_response(attempt: &CodexAttempt) -> bool {
    if attempt.emitted_event
        || !attempt.definitive_rejection
        || !matches!(
            &attempt.delivery,
            Some(RequestDeliveryMetadata {
                phase: RequestDeliveryPhase::NotSent | RequestDeliveryPhase::SentAwaitingAcceptance,
                response_id: None,
                ..
            })
        )
    {
        return false;
    }
    let Some(previous_response_id) = attempt.previous_response_id.as_deref() else {
        return false;
    };
    let Err(AgentError::Api { status, message }) = &attempt.result else {
        return false;
    };
    let normalized = message.trim().to_ascii_lowercase();
    if *status == 400
        && (normalized.starts_with("previous_response_not_found:")
            || normalized.contains("previous response") && normalized.contains("not found"))
    {
        return true;
    }
    (*status == 0 || *status == 404)
        && normalized == format!("not found: {}", previous_response_id.to_ascii_lowercase())
}

/// A failed turn evicts the referenced `previous_response_id` from the service's
/// connection-local cache, and a `store: false` response has no persisted copy to
/// fall back on. The chain is therefore dead after any error, whether or not it
/// was written to disk.
fn should_clear_response_chain<T>(result: &Result<T, AgentError>) -> bool {
    result.is_err()
}

fn is_definitive_responses_rejection(error: &AgentError) -> bool {
    !error.is_context_overflow()
        && matches!(error, AgentError::Api { status, .. } if *status == 400 || *status == 422)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_tungstenite::tungstenite::Message as WsMessage;
    use futures_lite::StreamExt;
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
    use tempfile::TempDir;
    use test_case::test_case;

    use super::*;
    use crate::{ContentBlock, Role, TokenUsage};

    const SYSTEM_HASH: &str = "system";
    const TOOLS_HASH: &str = "[]";
    const AUTH_SCOPE_HASH: &str = "account";
    const LEGACY_SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";
    const TEST_CREDENTIAL_HASH: &str = "test-credential";
    const TEST_STREAM_TIMEOUT: Duration = Duration::from_secs(30);

    fn test_fingerprint(system: &str, tools_hash: &str) -> CachePrefixFingerprint {
        CachePrefixFingerprint::new("gpt-5.6", &System::from(system), tools_hash)
    }

    async fn read_http_request(stream: &mut smol::net::TcpStream) -> (String, Value) {
        let mut request = Vec::new();

        let mut chunk = [0_u8; 2048];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let body_start = header_end + 4;
            if request.len() < body_start + content_length {
                continue;
            }
            let path = headers
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string();
            let body =
                serde_json::from_slice(&request[body_start..body_start + content_length]).unwrap();
            return (path, body);
        }
    }

    async fn write_http_response(
        stream: &mut smol::net::TcpStream,
        status: &str,
        content_type: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    }

    #[test_case(false; "clear")]
    #[test_case(true; "reset")]
    fn local_response_chain_cleanup_preserves_expected_entries(preserve_connection: bool) {
        let auth = Arc::new(Mutex::new(ResolvedAuth::bearer("test-key")));
        let provider = OpenAi::with_auth(auth, crate::providers::Timeouts::default()).unwrap();
        let session_id = n00nId::generate();

        provider
            .session_state
            .lock()
            .unwrap()
            .insert(session_id, OpenAiSessionState::default());
        provider
            .response_connections
            .lock()
            .unwrap()
            .insert(session_id, ResponseConnectionSlot::default());

        if preserve_connection {
            provider.reset_connection_local_chain(Some(session_id));
        } else {
            provider.clear_local_response_chain(session_id);
        }

        assert!(
            !provider
                .session_state
                .lock()
                .unwrap()
                .contains_key(&session_id)
        );
        assert_eq!(
            provider
                .response_connections
                .lock()
                .unwrap()
                .contains_key(&session_id),
            preserve_connection
        );
    }

    #[test_case(1)]
    #[test_case(8)]
    fn provider_config_slots_reach_openai_provider(slots: u64) {
        let config = n00n_config::ProviderConfig {
            openai_coding_plan_slots: slots,
            ..Default::default()
        };
        let auth = Arc::new(Mutex::new(ResolvedAuth::bearer("test-key")));
        let provider = OpenAi::with_auth_options(
            auth,
            crate::providers::Timeouts::default(),
            OpenAiOptions::from(&config),
        )
        .unwrap();

        assert_eq!(provider.coding_plan_slots, u8::try_from(slots).unwrap());
    }

    #[test]
    fn provider_config_codex_cache_capabilities_reach_openai_provider() {
        let config = n00n_config::ProviderConfig {
            openai_codex_accepts_prompt_cache_options_implicit: true,
            openai_codex_accepts_prompt_cache_options_explicit: true,
            openai_codex_accepts_prompt_cache_breakpoints: true,
            ..Default::default()
        };
        let auth = Arc::new(Mutex::new(ResolvedAuth::bearer("test-key")));
        let provider = OpenAi::with_auth_options(
            auth,
            crate::providers::Timeouts::default(),
            OpenAiOptions::from(&config),
        )
        .unwrap();

        assert_eq!(
            provider.codex_cache_capabilities,
            CodexCacheCapabilities {
                accepts_prompt_cache_options_implicit: true,
                accepts_prompt_cache_options_explicit: true,
                accepts_prompt_cache_breakpoints: true,
            }
        );
    }
    fn provider_with_response_storage(path: &Path) -> OpenAi {
        let auth = Arc::new(Mutex::new(ResolvedAuth::bearer("test-key")));
        let mut provider = OpenAi::with_auth(auth, crate::providers::Timeouts::default()).unwrap();

        let storage = StateDir::from_path(path.to_path_buf());
        provider.storage = Some(storage.clone());
        provider.response_state_storage = Some(storage);
        provider
    }

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    fn tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn incremental_request_uses_previous_response_and_new_messages() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, "[]"),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, "[]"),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
        assert_eq!(incremental_messages.len(), 1);
        assert!(matches!(
            &incremental_messages[0].content[0],
            ContentBlock::Text { text } if text == "again"
        ));
    }

    #[test]
    fn incremental_request_keeps_only_tool_results_after_tool_calls() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("run".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, "[]"),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("run".into()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "one"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "result".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];

        let (previous_response_id, incremental_messages) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, "[]"),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
        assert_eq!(incremental_messages.len(), 1);
        assert!(matches!(
            &incremental_messages[0].content[0],
            ContentBlock::ToolResult { tool_use_id, content, .. }
                if tool_use_id == "call_1" && content == "result"
        ));
    }

    #[test]
    fn api_responses_reject_unsupported_raw_breakpoints() {
        let opts = clamp_responses_cache_breakpoints(
            &Model::from_spec("openai/gpt-5.5").unwrap(),
            RequestOptions {
                message_cache_breakpoints: 2,
                openai_prompt_cache_mode: Some(OpenAiPromptCacheMode::Explicit),
                ..Default::default()
            },
        );

        assert_eq!(opts.message_cache_breakpoints, 0);
    }

    #[test]
    fn codex_cache_capabilities_keep_rejected_cache_options_off_by_default() {
        let opts = CodexCacheCapabilities::default().apply_to_request_options(RequestOptions {
            message_cache_breakpoints: 2,
            openai_prompt_cache_mode: Some(OpenAiPromptCacheMode::Implicit),
            ..Default::default()
        });

        assert_eq!(opts.message_cache_breakpoints, 0);
        assert_eq!(opts.openai_prompt_cache_mode, None);
    }

    #[test]
    fn codex_cache_capabilities_gate_implicit_cache_options() {
        let opts = CodexCacheCapabilities {
            accepts_prompt_cache_options_implicit: true,
            ..Default::default()
        }
        .apply_to_request_options(RequestOptions::default());

        assert_eq!(opts.message_cache_breakpoints, 0);
        assert_eq!(
            opts.openai_prompt_cache_mode,
            Some(OpenAiPromptCacheMode::Implicit)
        );
    }

    #[test]
    fn codex_cache_capabilities_reject_explicit_mode_without_breakpoints() {
        let opts = CodexCacheCapabilities {
            accepts_prompt_cache_options_explicit: true,
            ..Default::default()
        }
        .apply_to_request_options(RequestOptions {
            message_cache_breakpoints: 3,
            ..Default::default()
        });

        assert_eq!(opts.message_cache_breakpoints, 0);
        assert_eq!(opts.openai_prompt_cache_mode, None);
    }

    #[test]
    fn codex_cache_capabilities_gate_breakpoints_without_cache_options() {
        let opts = CodexCacheCapabilities {
            accepts_prompt_cache_breakpoints: true,
            ..Default::default()
        }
        .apply_to_request_options(RequestOptions {
            message_cache_breakpoints: 3,
            ..Default::default()
        });

        assert_eq!(opts.message_cache_breakpoints, 3);
        assert_eq!(opts.openai_prompt_cache_mode, None);
    }

    #[test]
    fn codex_cache_capabilities_gate_explicit_breakpoints() {
        let opts = CodexCacheCapabilities {
            accepts_prompt_cache_options_explicit: true,
            accepts_prompt_cache_breakpoints: true,
            ..Default::default()
        }
        .apply_to_request_options(RequestOptions {
            message_cache_breakpoints: 3,
            ..Default::default()
        });

        assert_eq!(opts.message_cache_breakpoints, 3);
        assert_eq!(
            opts.openai_prompt_cache_mode,
            Some(OpenAiPromptCacheMode::Explicit)
        );
    }

    #[test]
    fn codex_cache_capabilities_fall_back_to_implicit_without_breakpoints() {
        let opts = CodexCacheCapabilities {
            accepts_prompt_cache_options_implicit: true,
            accepts_prompt_cache_options_explicit: true,
            accepts_prompt_cache_breakpoints: true,
        }
        .apply_to_request_options(RequestOptions {
            message_cache_breakpoints: 0,
            ..Default::default()
        });

        assert_eq!(opts.message_cache_breakpoints, 0);
        assert_eq!(
            opts.openai_prompt_cache_mode,
            Some(OpenAiPromptCacheMode::Implicit)
        );
    }

    #[test]
    fn incremental_request_resets_when_tools_change() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, "[]"),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, "[\"new\"]"),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(previous_response_id.is_none());
        assert_eq!(incremental_messages.len(), second.len());
    }

    #[test]
    fn is_definitive_responses_rejection_detects_400_and_422() {
        let error_400 = AgentError::Api {
            status: 400,
            message: "bad request".into(),
        };
        let error_422 = AgentError::Api {
            status: 422,
            message: "unprocessable entity".into(),
        };
        let error_500 = AgentError::Api {
            status: 500,
            message: "internal server error".into(),
        };
        let context_overflow = AgentError::Api {
            status: 400,
            message: "maximum context length is 128000 tokens".into(),
        };
        let error_network = AgentError::Io(std::io::Error::other("connection failed"));

        assert!(is_definitive_responses_rejection(&error_400));
        assert!(is_definitive_responses_rejection(&error_422));
        assert!(!is_definitive_responses_rejection(&context_overflow));
        assert!(!is_definitive_responses_rejection(&error_500));
        assert!(!is_definitive_responses_rejection(&error_network));
    }

    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    #[test_case("gpt-5.5")]
    #[test_case("gpt-5.4")]
    #[test_case("gpt-5.4-nano")]
    #[test_case("gpt-5.4-mini")]
    #[test_case("gpt-5.3-codex")]
    #[test_case("gpt-5.2-codex")]
    #[test_case("gpt-5.1-codex")]
    #[test_case("gpt-5.1-codex-mini")]
    fn codex_model_catalog_contains_known_plan_model(model_id: &str) {
        assert!(
            crate::model::lookup_entry(crate::providers::openai::codex_models(), model_id).is_ok()
        );
    }

    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    fn codex_adjustment_caps_full_context_models(model_id: &str) {
        let auth = Arc::new(Mutex::new(ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: Vec::new(),
        }));
        let provider = OpenAi::with_auth_options(
            auth,
            crate::providers::Timeouts::default(),
            OpenAiOptions::codex(),
        )
        .unwrap();
        let mut model = Model::from_spec(&format!("openai/{model_id}")).unwrap();

        provider.adjust_model(&mut model);

        assert_eq!(model.context_window, CODING_PLAN_CONTEXT_WINDOW);
    }

    #[test]
    fn openai_adjustment_keeps_full_context() {
        let auth = Arc::new(Mutex::new(ResolvedAuth {
            base_url: None,
            headers: vec![("authorization".into(), "Bearer key".into())],
        }));
        let provider = OpenAi::with_auth(auth, crate::providers::Timeouts::default()).unwrap();
        let mut model = Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let expected = model.context_window;

        provider.adjust_model(&mut model);

        assert_eq!(model.context_window, expected);
    }

    #[test]
    fn hosted_tool_search_requires_supported_model_and_official_endpoint() {
        let official = OpenAi::with_auth(
            Arc::new(Mutex::new(ResolvedAuth::bearer("test-key"))),
            crate::providers::Timeouts::default(),
        )
        .unwrap();
        assert!(official.supports_hosted_tool_search(&Model::from_spec("openai/gpt-5.6").unwrap()));
        assert!(
            !official.supports_hosted_tool_search(&Model::from_spec("openai/gpt-4.1").unwrap())
        );

        let custom = OpenAi::with_auth(
            Arc::new(Mutex::new(ResolvedAuth {
                base_url: Some("https://example.test/v1".into()),
                headers: vec![("authorization".into(), "Bearer test-key".into())],
            })),
            crate::providers::Timeouts::default(),
        )
        .unwrap();
        assert!(!custom.supports_hosted_tool_search(&Model::from_spec("openai/gpt-5.6").unwrap()));
    }

    #[test]
    fn incremental_first_turn_sends_full_messages() {
        let mut state = OpenAiSessionState::default();
        let messages = vec![Message::user("hello".into())];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &messages,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 1);
        assert!(matches!(inc[0].role, Role::User));
    }

    #[test]
    fn incremental_second_turn_skips_assistant_message() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        let (prev, _inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        assert!(prev.is_none());

        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert_eq!(prev.as_deref(), Some("resp_1"));
        assert_eq!(inc.len(), 1);
        assert!(matches!(inc[0].role, Role::User));
        assert!(matches!(
            &inc[0].content[0],
            ContentBlock::Text { text } if text == "again"
        ));
    }

    #[test]
    fn incremental_tool_loop_skips_assistant_tool_calls() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("run".into())];
        let (prev, _inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        assert!(prev.is_none());
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("run".into()),
            assistant("ok"),
            tool_result("call_1", "result"),
            Message::user("next".into()),
        ];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert_eq!(prev.as_deref(), Some("resp_1"));
        assert_eq!(inc.len(), 2);
        assert!(matches!(inc[0].role, Role::User));
        assert!(matches!(inc[1].role, Role::User));
        assert!(matches!(
            &inc[1].content[0],
            ContentBlock::Text { text } if text == "next"
        ));
    }

    #[test]
    fn incremental_tools_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, "[\"new\"]"),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 3);
        assert_eq!(state.tools_hash, Some("[\"new\"]".to_string()));
    }

    #[test]
    fn incremental_system_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint("old-system", TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint("new-system", TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), second.len());
        assert_eq!(
            state.system_hash.as_deref(),
            Some(
                test_fingerprint("new-system", TOOLS_HASH)
                    .system_hash
                    .as_str()
            )
        );
    }

    #[test]
    fn incremental_dynamic_system_change_preserves_state() {
        let mut old_system = System::new();
        old_system.push_static("stable system");
        old_system.push_dynamic("old todo state");
        let mut new_system = System::new();
        new_system.push_static("stable system");
        new_system.push_dynamic("new todo state");
        let old_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &old_system, TOOLS_HASH);
        let new_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &new_system, TOOLS_HASH);
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &old_fingerprint,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous, incremental) =
            incremental_for_state(&mut state, &new_fingerprint, AUTH_SCOPE_HASH, &second).unwrap();

        assert_eq!(previous.as_deref(), Some("resp_1"));
        assert_eq!(incremental.len(), 1);
    }

    #[test]
    fn incremental_dynamic_system_appearance_preserves_state() {
        let mut old_system = System::new();
        old_system.push_static("stable system");
        old_system.mark_dynamic_boundary();
        old_system.push_static("unchanged policy");
        let mut new_system = System::new();
        new_system.push_static("stable system");
        new_system.push_dynamic("new todo state");
        new_system.push_static("unchanged policy");
        let old_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &old_system, TOOLS_HASH);
        let new_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &new_system, TOOLS_HASH);
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &old_fingerprint,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous, incremental) =
            incremental_for_state(&mut state, &new_fingerprint, AUTH_SCOPE_HASH, &second).unwrap();

        assert_eq!(previous.as_deref(), Some("resp_1"));
        assert_eq!(incremental.len(), 1);
    }

    #[test]
    fn incremental_static_system_change_after_dynamic_block_resets_state() {
        let mut old_system = System::new();
        old_system.push_static("stable system");
        old_system.push_dynamic("todo state");
        old_system.push_static("old policy");
        let mut new_system = System::new();
        new_system.push_static("stable system");
        new_system.push_dynamic("todo state");
        new_system.push_static("new policy");
        let old_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &old_system, TOOLS_HASH);
        let new_fingerprint = CachePrefixFingerprint::new("gpt-5.6", &new_system, TOOLS_HASH);
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &old_fingerprint,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous, incremental) =
            incremental_for_state(&mut state, &new_fingerprint, AUTH_SCOPE_HASH, &second).unwrap();

        assert!(previous.is_none());
        assert_eq!(incremental.len(), second.len());
    }

    #[test]
    fn incremental_model_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let changed =
            CachePrefixFingerprint::new("gpt-5.6-luna", &System::from(SYSTEM_HASH), TOOLS_HASH);

        let (previous, incremental) =
            incremental_for_state(&mut state, &changed, AUTH_SCOPE_HASH, &second).unwrap();

        assert!(previous.is_none());
        assert_eq!(incremental.len(), second.len());
    }

    #[test]
    fn system_hash_matches_equivalent_static_wire_text() {
        let combined = System::from("ab");
        let mut split = System::new();
        split.push_static("a");
        split.push_static("b");
        split.seal();

        assert_eq!(combined.to_string(), split.to_string());
        assert_eq!(system_hash(&combined), system_hash(&split));
    }

    #[test]
    fn state_without_system_hash_is_not_persisted() {
        let state = OpenAiSessionState {
            last_response_id: Some("resp_1".into()),
            last_message_count: 1,
            model_id: Some("gpt-5.6".into()),
            system_hash: None,
            tools_hash: Some(TOOLS_HASH.into()),
            messages_hash: Some("messages".into()),
            auth_scope_hash: Some(AUTH_SCOPE_HASH.into()),
            expires_at: now_epoch().saturating_add(OPENAI_RESPONSE_CHAIN_TTL_SECONDS),
            last_used: Instant::now(),
        };

        assert!(state.to_stored().is_none());
    }

    #[test]
    fn legacy_chain_without_system_hash_resets_state() {
        let mut state = OpenAiSessionState::from_stored(StoredOpenAiResponseChain {
            response_id: "resp_1".into(),
            message_count: 1,
            model_id: None,
            system_hash: None,
            tools_hash: TOOLS_HASH.into(),
            messages_hash: stable_json_hash(&[Message::user("hello".into())]).unwrap(),
            auth_scope_hash: AUTH_SCOPE_HASH.into(),
            expires_at: now_epoch().saturating_add(OPENAI_RESPONSE_CHAIN_TTL_SECONDS),
        });
        let messages = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &messages,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), messages.len());
        assert_eq!(
            state.system_hash.as_deref(),
            Some(
                test_fingerprint(SYSTEM_HASH, TOOLS_HASH)
                    .system_hash
                    .as_str()
            )
        );
    }

    #[test]
    fn incremental_messages_shrink_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("a".into()), Message::user("b".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![Message::user("a".into())];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 1);
    }

    #[test]
    fn incremental_prefix_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("different".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 3);
    }

    #[test]
    fn record_response_without_id_clears_stale_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![Message::user("again".into())];
        record_in_state(
            &mut state,
            None,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            AUTH_SCOPE_HASH,
            &second,
        )
        .unwrap();

        assert!(state.last_response_id.is_none());
        assert_eq!(state.last_message_count, 0);
    }

    #[test]
    fn ephemeral_response_chain_skips_durable_lock() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let session_id = SessionRef::generate();
            let provider = provider_with_response_storage(temp_dir.path());

            let response_chain_lock = provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();

            assert!(response_chain_lock.is_none());
            let sessions_dir = temp_dir.path().join(n00n_storage::sessions::SESSIONS_DIR);
            let files = std::fs::read_dir(sessions_dir).unwrap().count();
            assert_eq!(files, 0);
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn durable_preflight_failure_continues_second_turn_from_persisted_response() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (body_tx, body_rx) = flume::bounded(2);
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();

                let Some(Ok(WsMessage::Text(first))) = socket.next().await else {
                    panic!("expected first response.create");
                };
                body_tx
                    .send_async(serde_json::from_str::<Value>(&first).unwrap())
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_first"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_first","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                let Some(Ok(WsMessage::Ping(_))) = socket.next().await else {
                    panic!("expected continuation preflight ping");
                };
                socket.close(None).await.unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                let Some(Ok(WsMessage::Text(second))) = socket.next().await else {
                    panic!("expected rebuilt second response.create");
                };
                body_tx
                    .send_async(serde_json::from_str::<Value>(&second).unwrap())
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_second"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_second","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });

            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let mut provider = OpenAi::with_auth_options(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
                OpenAiOptions::codex(),
            )
            .unwrap();
            let storage = StateDir::from_path(temp_dir.path().to_path_buf());
            provider.storage = Some(storage.clone());
            provider.response_state_storage = Some(storage.clone());
            let session_id = SessionRef::generate();
            let mut session = n00n_storage::sessions::Session::<Message, TokenUsage, Value>::new(
                "model", "/project",
            );
            session.id = session_id.id();
            session.save(&storage).unwrap();
            let model = Model::from_spec("codex/gpt-5.3-codex").unwrap();
            let tools = serde_json::json!([]);
            let (event_tx, _event_rx) = flume::unbounded();
            let first_messages = vec![Message::user("hello".into())];
            let mut first_system = System::new();
            first_system.push_static("stable instructions");
            first_system.push_dynamic("old todo state");
            first_system.push_static("unchanged policy");

            provider
                .stream_message(
                    &model,
                    &first_messages,
                    &first_system,
                    &tools,
                    &event_tx,
                    RequestOptions::default(),
                    Some(&session_id),
                )
                .await
                .unwrap();
            let second_messages = vec![
                Message::user("hello".into()),
                assistant("hi"),
                Message::user("again".into()),
            ];
            let mut second_system = System::new();
            second_system.push_static("stable instructions");
            second_system.push_dynamic("new todo state");
            second_system.push_static("unchanged policy");
            provider
                .stream_message(
                    &model,
                    &second_messages,
                    &second_system,
                    &tools,
                    &event_tx,
                    RequestOptions {
                        protect_history_replay: true,
                        allow_history_replay: true,
                        ..Default::default()
                    },
                    Some(&session_id),
                )
                .await
                .unwrap();
            server.await;

            let first_body = body_rx.recv_async().await.unwrap();
            let second_body = body_rx.recv_async().await.unwrap();
            assert!(first_body.get("previous_response_id").is_none());
            assert_eq!(first_body["store"], false);
            assert_eq!(second_body["previous_response_id"], "resp_first");
            assert_eq!(second_body["store"], false);
            assert_eq!(second_body["input"].as_array().unwrap().len(), 1);
            assert_eq!(
                first_body["prompt_cache_key"],
                second_body["prompt_cache_key"]
            );
            assert_eq!(
                first_body["instructions"],
                "stable instructionsold todo stateunchanged policy"
            );
            assert_eq!(
                second_body["instructions"],
                "stable instructionsnew todo stateunchanged policy"
            );

            let lock = provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap()
                .expect("durable response-chain lock");
            let chain = load_openai_response_chain(&storage, session_id.id(), &lock)
                .unwrap()
                .expect("persisted response chain");
            assert_eq!(chain.response_id, "resp_second");
        });
    }

    #[test]
    fn durable_response_chain_survives_provider_restart() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let session_id = SessionRef::generate();
            let first = vec![Message::user("hello".into())];
            let provider = provider_with_response_storage(temp_dir.path());
            let mut session = n00n_storage::sessions::Session::<Message, TokenUsage, Value>::new(
                "model", "/project",
            );
            session.id = session_id.id();
            session
                .save(provider.response_state_storage.as_ref().unwrap())
                .unwrap();
            let response_chain_lock = provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();
            assert!(response_chain_lock.is_some());
            provider
                .record_response(
                    Some(&session_id),
                    Some("resp_1".into()),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &first,
                    true,
                    response_chain_lock.as_ref(),
                )
                .await;
            drop(response_chain_lock);
            drop(provider);

            let restored = provider_with_response_storage(temp_dir.path());
            let second = vec![
                Message::user("hello".into()),
                assistant("hi"),
                Message::user("again".into()),
            ];
            let restored_lock = restored
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();
            let (previous_response_id, incremental) = restored
                .prepare_request(
                    Some(&session_id),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &second,
                    restored_lock.as_ref(),
                )
                .await
                .unwrap();

            assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
            assert_eq!(incremental.len(), 1);
        });
    }

    #[test]
    fn durable_response_chain_reloads_across_alternating_providers() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let session_id = SessionRef::generate();
            let first = vec![Message::user("first".into())];
            let second = vec![
                Message::user("first".into()),
                assistant("first response"),
                Message::user("second".into()),
            ];
            let third = vec![
                Message::user("first".into()),
                assistant("first response"),
                Message::user("second".into()),
                assistant("second response"),
                Message::user("third".into()),
            ];
            let first_provider = provider_with_response_storage(temp_dir.path());
            let second_provider = provider_with_response_storage(temp_dir.path());
            let mut session = n00n_storage::sessions::Session::<Message, TokenUsage, Value>::new(
                "model", "/project",
            );
            session.id = session_id.id();
            session
                .save(first_provider.response_state_storage.as_ref().unwrap())
                .unwrap();

            let lock = first_provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();
            assert!(lock.is_some());
            first_provider
                .record_response(
                    Some(&session_id),
                    Some("resp_first".into()),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &first,
                    true,
                    lock.as_ref(),
                )
                .await;
            drop(lock);

            let lock = second_provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();
            assert!(lock.is_some());
            let (previous, incremental) = second_provider
                .prepare_request(
                    Some(&session_id),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &second,
                    lock.as_ref(),
                )
                .await
                .unwrap();
            assert_eq!(previous.as_deref(), Some("resp_first"));
            assert_eq!(incremental.len(), 1);
            second_provider
                .record_response(
                    Some(&session_id),
                    Some("resp_second".into()),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &second,
                    true,
                    lock.as_ref(),
                )
                .await;
            drop(lock);

            let lock = first_provider
                .lock_response_chain(Some(&session_id))
                .await
                .unwrap();
            assert!(lock.is_some());
            let (previous, incremental) = first_provider
                .prepare_request(
                    Some(&session_id),
                    &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
                    AUTH_SCOPE_HASH,
                    &third,
                    lock.as_ref(),
                )
                .await
                .unwrap();
            assert_eq!(previous.as_deref(), Some("resp_second"));
            assert_eq!(incremental.len(), 1);
        });
    }

    #[test]
    fn response_chain_lock_times_out_under_subprocess_contention() {
        const CHILD_ENV: &str = "N00N_PROVIDER_RESPONSE_CHAIN_LOCK_CHILD";
        const DIR_ENV: &str = "N00N_PROVIDER_RESPONSE_CHAIN_LOCK_DIR";
        const SESSION_ENV: &str = "N00N_PROVIDER_RESPONSE_CHAIN_LOCK_SESSION";
        const READY_ENV: &str = "N00N_PROVIDER_RESPONSE_CHAIN_LOCK_READY";

        if std::env::var_os(CHILD_ENV).is_some() {
            let dir = std::env::var_os(DIR_ENV)
                .map(std::path::PathBuf::from)
                .unwrap();
            let session_id = std::env::var(SESSION_ENV)
                .unwrap()
                .parse::<n00nId>()
                .unwrap();
            let ready = std::env::var_os(READY_ENV)
                .map(std::path::PathBuf::from)
                .unwrap();
            let state_dir = StateDir::from_path(dir);
            let _lock =
                n00n_storage::sessions::lock_openai_response_chain(&state_dir, session_id).unwrap();
            std::fs::write(ready, b"ready").unwrap();
            std::thread::sleep(RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT + Duration::from_secs(1));
            return;
        }

        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let provider = provider_with_response_storage(temp_dir.path());
            let session_id = SessionRef::generate();
            let mut session = n00n_storage::sessions::Session::<Message, TokenUsage, Value>::new(
                "model", "/project",
            );
            session.id = session_id.id();
            session
                .save(provider.response_state_storage.as_ref().unwrap())
                .unwrap();
            let ready = temp_dir.path().join("ready");
            let executable = std::env::current_exe().unwrap();
            let mut child = std::process::Command::new(executable)
                .args(["--exact", "providers::openai::platform::tests::response_chain_lock_times_out_under_subprocess_contention"])
                .env(CHILD_ENV, "1")
                .env(DIR_ENV, temp_dir.path())
                .env(SESSION_ENV, session_id.id().to_string())
                .env(READY_ENV, &ready)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ready.exists());
            let started = Instant::now();
            let Err(error) = provider.lock_response_chain(Some(&session_id)).await else {
                panic!("contended response-chain lock unexpectedly acquired");
            };
            assert!(started.elapsed() >= RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT);
            assert!(matches!(error, AgentError::ResponseChainBusy { .. }));
            assert!(child.wait().unwrap().success());
        });
    }

    #[test]
    fn ordinary_api_key_uses_official_responses_base_url_without_storage() {
        let auth = Arc::new(Mutex::new(ResolvedAuth::bearer("test-key")));
        let provider = OpenAi::with_auth(auth, crate::providers::Timeouts::default()).unwrap();

        assert_eq!(
            super::super::responses::base_url(&provider.current_auth()),
            super::super::OPENAI_API_BASE_URL
        );
        let opts = RequestOptions::default();
        let body = super::super::responses::build_body(
            &Model::from_spec("openai/gpt-4.1").unwrap(),
            &[Message::user("private history".into())],
            &System::from(""),
            &serde_json::json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["store"], false);
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn supported_model_dispatches_to_responses_with_private_full_history() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (request_tx, request_rx) = flume::bounded(1);
            let server = smol::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_http_request(&mut stream).await;
                request_tx.send_async(request).await.unwrap();
                let sse = concat!(
                    "event: response.created\ndata: {\"response\":{\"id\":\"resp_test\"}}\n\n",
                    "event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n",
                    "event: response.completed\ndata: {\"response\":{\"id\":\"resp_test\",\"status\":\"completed\"}}\n\n"
                );
                write_http_response(&mut stream, "200 OK", "text/event-stream", sse).await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: vec![("authorization".into(), "Bearer test-key".into())],
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts::default(),
            )
            .unwrap();
            let model = Model::from_spec("openai/gpt-5.5").unwrap();
            let messages = vec![
                Message::user("first".into()),
                assistant("second"),
                Message::user("third".into()),
            ];
            let (event_tx, _event_rx) = flume::unbounded();

            provider
                .stream_message(
                    &model,
                    &messages,
                    &System::from("system"),
                    &serde_json::json!([]),
                    &event_tx,
                    RequestOptions::default(),
                    None,
                )
                .await
                .unwrap();
            server.await;

            let (path, body) = request_rx.recv_async().await.unwrap();
            assert_eq!(path, "/v1/responses");
            assert_eq!(body["store"], false);
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(body["input"].as_array().unwrap().len(), messages.len());
        });
    }

    #[test]
    fn responses_rejection_falls_back_to_chat_completions() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (path_tx, path_rx) = flume::bounded(2);
            let server = smol::spawn(async move {
                let (mut responses_stream, _) = listener.accept().await.unwrap();
                let (responses_path, _) = read_http_request(&mut responses_stream).await;
                path_tx.send_async(responses_path).await.unwrap();
                write_http_response(
                    &mut responses_stream,
                    "400 Bad Request",
                    "application/json",
                    r#"{"error":{"message":"Responses unsupported","type":"invalid_request_error"}}"#,
                )
                .await;

                let (mut chat_stream, _) = listener.accept().await.unwrap();
                let (chat_path, _) = read_http_request(&mut chat_stream).await;
                path_tx.send_async(chat_path).await.unwrap();
                let sse = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"fallback\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                );
                write_http_response(&mut chat_stream, "200 OK", "text/event-stream", sse).await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: vec![("authorization".into(), "Bearer test-key".into())],
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts::default(),
            )
            .unwrap();
            let model = Model::from_spec("openai/gpt-5.5").unwrap();
            let (event_tx, _event_rx) = flume::unbounded();

            let result = provider
                .stream_message(
                    &model,
                    &[Message::user("hello".into())],
                    &System::from(""),
                    &serde_json::json!([]),
                    &event_tx,
                    RequestOptions::default(),
                    None,
                )
                .await;
            server.await;

            // Should succeed with fallback to chat completions
            assert!(result.is_ok());
            assert_eq!(path_rx.recv_async().await.unwrap(), "/v1/responses");
            assert_eq!(path_rx.recv_async().await.unwrap(), "/v1/chat/completions");
        });
    }

    #[test]
    fn coding_plan_session_headers_match_codex_routing_contract() {
        let session_id = SessionRef::generate().id();
        let auth = with_codex_session_headers(
            ResolvedAuth {
                base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
                headers: vec![
                    ("authorization".into(), "Bearer test".into()),
                    ("Session-ID".into(), "stale".into()),
                ],
            },
            Some(session_id),
        );
        let expected = session_id.to_string();

        assert!(auth.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value == "Bearer test"
        }));
        for header in CODEX_SESSION_HEADERS {
            assert_eq!(
                auth.headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(header))
                    .map(|(_, value)| value.as_str()),
                Some(expected.as_str())
            );
        }
        assert_eq!(
            auth.headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(CODEX_ORIGINATOR_HEADER))
                .map(|(_, value)| value.as_str()),
            Some(CODEX_ORIGINATOR)
        );
        assert_eq!(
            auth.headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("session-id"))
                .count(),
            1
        );
    }

    #[test]
    fn coding_plan_without_session_replaces_protected_headers() {
        let auth = with_codex_session_headers(
            ResolvedAuth {
                base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
                headers: vec![
                    ("session-id".into(), "stale".into()),
                    (CODEX_ORIGINATOR_HEADER.into(), "stale".into()),
                ],
            },
            None,
        );

        assert!(auth.headers.iter().all(|(name, _)| {
            !CODEX_SESSION_HEADERS
                .iter()
                .any(|header| name.eq_ignore_ascii_case(header))
        }));
        assert_eq!(
            auth.headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(CODEX_ORIGINATOR_HEADER))
                .map(|(_, value)| value.as_str()),
            Some(CODEX_ORIGINATOR)
        );
    }

    #[test]
    fn codex_session_headers_are_not_added_to_api_requests() {
        let auth = ResolvedAuth {
            base_url: Some("https://api.openai.com/v1".into()),
            headers: vec![("authorization".into(), "Bearer test".into())],
        };
        let resolved = with_codex_session_headers(auth.clone(), Some(SessionRef::generate().id()));

        assert_eq!(resolved.base_url, auth.base_url);
        assert_eq!(resolved.headers, auth.headers);
    }

    #[test]
    fn coding_plan_state_scope_survives_token_refresh_for_same_account() {
        let first = ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: vec![
                ("authorization".into(), "Bearer first".into()),
                ("chatgpt-account-id".into(), "account-1".into()),
            ],
        };
        let refreshed = ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: vec![
                ("authorization".into(), "Bearer refreshed".into()),
                ("chatgpt-account-id".into(), "account-1".into()),
            ],
        };

        assert_eq!(
            response_state_scope_hash(&first),
            response_state_scope_hash(&refreshed)
        );
        assert_ne!(credential_hash(&first), credential_hash(&refreshed));
    }

    #[test]
    fn coding_plan_state_scope_changes_with_account() {
        let first = ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: vec![
                ("authorization".into(), "Bearer token".into()),
                ("chatgpt-account-id".into(), "account-1".into()),
            ],
        };
        let second = ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: vec![
                ("authorization".into(), "Bearer token".into()),
                ("chatgpt-account-id".into(), "account-2".into()),
            ],
        };

        assert_ne!(
            response_state_scope_hash(&first),
            response_state_scope_hash(&second)
        );
    }

    #[test]
    fn response_chain_resets_when_auth_scope_changes() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            "account-1",
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental) = incremental_for_state(
            &mut state,
            &test_fingerprint(SYSTEM_HASH, TOOLS_HASH),
            "account-2",
            &second,
        )
        .unwrap();

        assert!(previous_response_id.is_none());
        assert_eq!(incremental.len(), second.len());
    }

    #[test]
    fn response_connection_slot_is_reused_per_session() {
        let temp_dir = TempDir::new().unwrap();
        let provider = provider_with_response_storage(temp_dir.path());
        let session_id = SessionRef::generate();
        let first = provider
            .response_connection_slot(Some(&session_id))
            .unwrap();
        let second = provider
            .response_connection_slot(Some(&session_id))
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn response_operation_slot_is_reused_across_provider_instances() {
        let temp_dir = TempDir::new().unwrap();
        let first_provider = provider_with_response_storage(temp_dir.path());
        let second_provider = provider_with_response_storage(temp_dir.path());
        let session_id = SessionRef::generate();
        let first = first_provider
            .response_operation_slot(Some(&session_id))
            .unwrap();
        let second = second_provider
            .response_operation_slot(Some(&session_id))
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn request_tools_hash_includes_hosted_catalog_descriptions() {
        let tools = serde_json::json!([{"name": "read_file"}]);
        let mut opts = RequestOptions {
            hosted_tool_search: Some(crate::HostedToolSearch {
                tools: vec![crate::DeferredToolDefinition {
                    namespace: "knowledge".into(),
                    definition: serde_json::json!({
                        "name": "use_memory",
                        "description": "first description",
                        "input_schema": {"type": "object"}
                    }),
                }],
            }),
            ..Default::default()
        };
        let first = request_tools_hash(&tools, &opts).unwrap();
        assert_eq!(first, request_tools_hash(&tools, &opts).unwrap());

        opts.hosted_tool_search.as_mut().unwrap().tools[0].definition["description"] =
            Value::String("changed description".into());
        assert_ne!(first, request_tools_hash(&tools, &opts).unwrap());
    }

    #[test]
    fn cache_prefix_fingerprint_groups_matching_stable_prefixes() {
        let tools_hash = stable_json_hash(&serde_json::json!([{"type": "function"}])).unwrap();
        let system = System::from("stable instructions");
        let fingerprint = CachePrefixFingerprint::new("gpt-5.6", &system, &tools_hash);
        let key = fingerprint.prompt_cache_key(None);
        let system_text = system.to_string();
        let mut legacy_digest = Sha256::new();
        legacy_digest.update("gpt-5.6".len().to_le_bytes());
        legacy_digest.update(b"gpt-5.6");
        legacy_digest.update(system_text.len().to_le_bytes());
        legacy_digest.update(system_text.as_bytes());
        legacy_digest.update(tools_hash.as_bytes());
        let legacy_prefix_hash = format!("{:x}", legacy_digest.finalize());

        assert_eq!(fingerprint.prefix_hash(), legacy_prefix_hash);

        assert_eq!(
            fingerprint,
            CachePrefixFingerprint::new("gpt-5.6", &system, &tools_hash)
        );
        assert_eq!(key, fingerprint.prompt_cache_key(None));
        assert_ne!(
            fingerprint,
            CachePrefixFingerprint::new("gpt-5.6", &System::from("changed"), &tools_hash)
        );
        assert_ne!(
            fingerprint,
            CachePrefixFingerprint::new("gpt-5.6-sol", &system, &tools_hash)
        );
        assert_ne!(
            fingerprint,
            CachePrefixFingerprint::new("gpt-5.6", &system, "changed-tools")
        );
    }

    #[test]
    fn prompt_cache_key_ignores_dynamic_system_suffix() {
        let mut empty = System::new();
        empty.push_static("stable instructions");
        empty.mark_dynamic_boundary();
        empty.push_static("unchanged policy");
        let mut first = System::new();
        first.push_static("stable instructions");
        first.push_dynamic("todo state one");
        first.push_static("unchanged policy");
        let mut second = System::new();
        second.push_static("stable instructions");
        second.push_dynamic("todo state two");
        second.push_static("unchanged policy");

        let empty = CachePrefixFingerprint::new("gpt-5.6", &empty, TOOLS_HASH);
        let first = CachePrefixFingerprint::new("gpt-5.6", &first, TOOLS_HASH);
        let second = CachePrefixFingerprint::new("gpt-5.6", &second, TOOLS_HASH);

        assert_eq!(empty.system_hash, first.system_hash);
        assert_eq!(first.system_hash, second.system_hash);
        assert_eq!(empty.prefix_hash(), first.prefix_hash());
        assert_eq!(first.prefix_hash(), second.prefix_hash());
        assert_eq!(empty.prompt_cache_key(None), first.prompt_cache_key(None));
        assert_eq!(first.prompt_cache_key(None), second.prompt_cache_key(None));
    }

    #[test]
    fn cache_prefix_fingerprint_debug_is_sanitized() {
        let system_text = "private system instructions";
        let tools_text = "private tool schema";
        let tools_hash = stable_json_hash(tools_text).unwrap();
        let fingerprint =
            CachePrefixFingerprint::new("gpt-5.6", &System::from(system_text), &tools_hash);
        let debug = format!("{fingerprint:?}");

        assert!(!debug.contains(system_text));
        assert!(!debug.contains(tools_text));
        assert!(!debug.contains(&fingerprint.system_hash));
        assert!(!debug.contains(&fingerprint.tools_hash));
        assert!(debug.contains(short_hash(fingerprint.prefix_hash())));
    }

    #[test]
    fn response_state_uses_canonical_session_identity() {
        let temp_dir = TempDir::new().unwrap();
        let provider = provider_with_response_storage(temp_dir.path());
        let legacy: SessionRef = LEGACY_SESSION_ID.parse().unwrap();
        let canonical = SessionRef::from_id(legacy.id());
        let fingerprint = CachePrefixFingerprint::new("gpt-5.6", &System::from("system"), "tools");

        assert_ne!(legacy.as_str(), canonical.as_str());
        assert_eq!(
            fingerprint.prompt_cache_key(Some(&legacy)),
            fingerprint.prompt_cache_key(Some(&canonical))
        );

        let legacy_connection = provider.response_connection_slot(Some(&legacy)).unwrap();
        let canonical_connection = provider.response_connection_slot(Some(&canonical)).unwrap();
        assert!(Arc::ptr_eq(&legacy_connection, &canonical_connection));

        let legacy_operation = provider.response_operation_slot(Some(&legacy)).unwrap();
        let canonical_operation = provider.response_operation_slot(Some(&canonical)).unwrap();
        assert!(Arc::ptr_eq(&legacy_operation, &canonical_operation));
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn connection_limit_after_create_send_does_not_reconnect() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let (done_tx, done_rx) = flume::bounded(1);
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                    server_creates.fetch_add(1, Ordering::Relaxed);
                }
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"error",
                            "error": {
                                "code":"websocket_connection_limit_reached",
                                "message":"open a fresh connection"
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                futures_lite::future::race(
                    async {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                        if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                            server_creates.fetch_add(1, Ordering::Relaxed);
                        }
                        socket.close(None).await.unwrap();
                    },
                    async {
                        done_rx.recv_async().await.unwrap();
                    },
                )
                .await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth.clone())),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let session = SessionRef::generate();
            let slot = provider.response_connection_slot(Some(&session)).unwrap();
            let (event_tx, _) = flume::unbounded();

            let error = provider
                .stream_websocket(
                    Some(slot),
                    &serde_json::json!({"model":"test","input":[]}),
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &auth,
                    TEST_CREDENTIAL_HASH,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap_err();
            let _ = done_tx.send_async(()).await;
            server.await;

            assert_eq!(creates.load(Ordering::Relaxed), 1);
            assert_eq!(
                error.delivery.phase,
                crate::RequestDeliveryPhase::SentAwaitingAcceptance
            );
            assert!(matches!(
                error.into_agent_error(),
                AgentError::RequestSent { .. }
            ));
        });
    }

    #[test]
    fn pre_send_definitive_401_allows_oauth_refresh_retry() {
        let attempt = CodexAttempt::from_websocket_error(
            None,
            super::super::websocket::WebSocketAttemptError::transport(
                AgentError::Api {
                    status: 401,
                    message: "expired token".into(),
                },
                false,
                crate::RequestDeliveryMetadata::new(crate::RequestDeliveryPhase::NotSent),
            ),
        );

        assert!(attempt.should_retry_after_oauth_refresh());
    }

    #[test]
    fn oauth_refresh_retry_rejects_non_replay_safe_401_attempts() {
        let attempt = |phase, response_id: Option<&str>, emitted_event, transport_failure| {
            let mut delivery = RequestDeliveryMetadata::new(phase);
            delivery.response_id = response_id.map(ToOwned::to_owned);
            delivery.emitted_event = emitted_event;
            CodexAttempt::from_websocket_error(
                None,
                super::super::websocket::WebSocketAttemptError {
                    error: Box::new(AgentError::Api {
                        status: 401,
                        message: "expired token".into(),
                    }),
                    transport_failure,
                    delivery: Box::new(delivery),
                },
            )
        };

        assert!(
            !attempt(
                RequestDeliveryPhase::SentAwaitingAcceptance,
                None,
                false,
                false,
            )
            .should_retry_after_oauth_refresh()
        );
        assert!(
            !attempt(
                RequestDeliveryPhase::SentAwaitingAcceptance,
                None,
                false,
                true,
            )
            .should_retry_after_oauth_refresh()
        );
        assert!(
            !attempt(RequestDeliveryPhase::Accepted, None, false, false)
                .should_retry_after_oauth_refresh()
        );
        assert!(
            !attempt(
                RequestDeliveryPhase::NotSent,
                Some("resp_observed"),
                false,
                false,
            )
            .should_retry_after_oauth_refresh()
        );
        assert!(
            !attempt(RequestDeliveryPhase::NotSent, None, true, false)
                .should_retry_after_oauth_refresh()
        );
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn response_created_then_401_is_not_retryable_and_sends_one_create() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                    server_creates.fetch_add(1, Ordering::Relaxed);
                }
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_accepted"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"error",
                            "status":401,
                            "error": {
                                "type":"authentication_error",
                                "message":"token expired after acceptance"
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth.clone())),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let model = Model::from_spec("openai/gpt-5.3-codex").unwrap();
            let tools = serde_json::json!([]);
            let (event_tx, _) = flume::unbounded();

            let system = System::from("");
            let attempt = provider
                .run_codex_attempt(
                    &model,
                    &[Message::user("hello".into())],
                    &system,
                    &tools,
                    TOOLS_HASH,
                    &event_tx,
                    RequestOptions::default(),
                    None,
                    false,
                    &auth,
                    0,
                )
                .await;
            server.await;

            assert_eq!(creates.load(Ordering::Relaxed), 1);
            assert!(!attempt.should_retry_after_oauth_refresh());
            assert!(matches!(
                attempt.delivery,
                Some(crate::RequestDeliveryMetadata {
                    phase: crate::RequestDeliveryPhase::Accepted,
                    response_id: Some(ref response_id),
                    ..
                }) if response_id == "resp_accepted"
            ));
            assert!(matches!(
                attempt.result,
                Err(AgentError::RequestSent { .. })
            ));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn created_then_connection_limit_does_not_send_a_second_create() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let (done_tx, done_rx) = flume::bounded(1);
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                    server_creates.fetch_add(1, Ordering::Relaxed);
                }
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_accepted"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"error",
                            "error": {
                                "code":"websocket_connection_limit_reached",
                                "message":"open a fresh connection"
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                futures_lite::future::race(
                    async {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                        if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                            server_creates.fetch_add(1, Ordering::Relaxed);
                        }
                        socket.close(None).await.unwrap();
                    },
                    async {
                        done_rx.recv_async().await.unwrap();
                    },
                )
                .await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth.clone())),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let session = SessionRef::generate();
            let slot = provider.response_connection_slot(Some(&session)).unwrap();
            let (event_tx, _) = flume::unbounded();
            let error = provider
                .stream_websocket(
                    Some(slot),
                    &serde_json::json!({"model":"test","input":[]}),
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &auth,
                    TEST_CREDENTIAL_HASH,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap_err();
            done_tx.send_async(()).await.unwrap();
            server.await;

            assert_eq!(creates.load(Ordering::Relaxed), 1);
            assert_eq!(error.delivery.phase, crate::RequestDeliveryPhase::Accepted);
            assert_eq!(error.delivery.response_id.as_deref(), Some("resp_accepted"));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    fn near_expiry_pooled_socket_is_replaced_before_second_turn_create() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let server = smol::spawn(async move {
                let (first_stream, _) = listener.accept().await.unwrap();
                let mut first = async_tungstenite::accept_async(first_stream).await.unwrap();
                if matches!(first.next().await, Some(Ok(WsMessage::Text(_)))) {
                    server_creates.fetch_add(1, Ordering::Relaxed);
                }
                first
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_first"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                first
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_first","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let _ = first.next().await;

                let (second_stream, _) = listener.accept().await.unwrap();
                let mut second = async_tungstenite::accept_async(second_stream)
                    .await
                    .unwrap();
                if matches!(second.next().await, Some(Ok(WsMessage::Text(_)))) {
                    server_creates.fetch_add(1, Ordering::Relaxed);
                }
                second
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.created",
                            "response":{"id":"resp_second"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                second
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_second","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth.clone())),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let session = SessionRef::generate();
            let slot = provider.response_connection_slot(Some(&session)).unwrap();
            let (event_tx, _) = flume::unbounded();
            let body = serde_json::json!({"model":"test","input":[]});

            let (first_id, _) = provider
                .stream_websocket(
                    Some(Arc::clone(&slot)),
                    &body,
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &auth,
                    TEST_CREDENTIAL_HASH,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap();
            {
                let mut connection = slot.lock().await;
                connection
                    .as_mut()
                    .unwrap()
                    .socket
                    .set_age_for_test(Duration::from_mins(55) - Duration::from_secs(5));
            }
            assert!(
                !provider
                    .response_connection_is_reusable(
                        Some(&session),
                        TEST_CREDENTIAL_HASH,
                        Duration::from_secs(2),
                        0,
                    )
                    .await
            );
            let (second_id, _) = provider
                .stream_websocket(
                    Some(slot),
                    &body,
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &auth,
                    TEST_CREDENTIAL_HASH,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap();
            server.await;

            assert_eq!(first_id.as_deref(), Some("resp_first"));
            assert_eq!(second_id.as_deref(), Some("resp_second"));
            assert_eq!(creates.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn token_refresh_during_new_socket_handshake_reconnects_before_create() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let old_auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: vec![("authorization".into(), "Bearer expiring".into())],
            };
            let old_credential_hash = credential_hash(&old_auth);
            let provider = Arc::new(
                OpenAi::with_auth(
                    Arc::new(Mutex::new(old_auth.clone())),
                    crate::providers::Timeouts {
                        connect: Duration::from_secs(2),
                        stream: Duration::from_secs(2),
                        low_speed: Duration::from_secs(2),
                    },
                )
                .unwrap(),
            );
            let server_provider = Arc::clone(&provider);
            let server = smol::spawn(async move {
                let (first_stream, _) = listener.accept().await.unwrap();
                let mut first = async_tungstenite::accept_hdr_async(
                    first_stream,
                    #[allow(clippy::result_large_err)]
                    move |
                        _request: &async_tungstenite::tungstenite::handshake::server::Request,
                        response: async_tungstenite::tungstenite::handshake::server::Response,
                    | {
                        *server_provider
                            .auth
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = ResolvedAuth {
                            base_url: Some(format!("http://{address}/v1")),
                            headers: vec![("authorization".into(), "Bearer refreshed".into())],
                        };
                        server_provider
                            .auth_generation
                            .fetch_add(1, Ordering::Release);
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                assert!(!matches!(first.next().await, Some(Ok(WsMessage::Text(_)))));

                let (second_stream, _) = listener.accept().await.unwrap();
                let mut second = async_tungstenite::accept_async(second_stream)
                    .await
                    .unwrap();
                assert!(matches!(second.next().await, Some(Ok(WsMessage::Text(_)))));
                second
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_fresh","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });
            let (event_tx, _) = flume::unbounded();

            let (response_id, _) = provider
                .stream_websocket(
                    None,
                    &serde_json::json!({"model":"test","input":[]}),
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &old_auth,
                    &old_credential_hash,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap();
            server.await;

            assert_eq!(response_id.as_deref(), Some("resp_fresh"));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    fn token_refresh_during_reused_socket_preflight_reconnects_before_create() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let old_auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: vec![("authorization".into(), "Bearer expiring".into())],
            };
            let old_credential_hash = credential_hash(&old_auth);
            let provider = Arc::new(
                OpenAi::with_auth(
                    Arc::new(Mutex::new(old_auth.clone())),
                    crate::providers::Timeouts {
                        connect: Duration::from_secs(2),
                        stream: Duration::from_secs(2),
                        low_speed: Duration::from_secs(2),
                    },
                )
                .unwrap(),
            );
            let server_provider = Arc::clone(&provider);
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let server = smol::spawn(async move {
                let (first_stream, _) = listener.accept().await.unwrap();
                let mut first = async_tungstenite::accept_async(first_stream).await.unwrap();
                assert!(matches!(first.next().await, Some(Ok(WsMessage::Text(_)))));
                server_creates.fetch_add(1, Ordering::Relaxed);
                first
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_first","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                let Some(Ok(WsMessage::Ping(payload))) = first.next().await else {
                    panic!("expected reused-socket preflight ping");
                };
                *server_provider
                    .auth
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = ResolvedAuth {
                    base_url: Some(format!("http://{address}/v1")),
                    headers: vec![("authorization".into(), "Bearer refreshed".into())],
                };
                server_provider
                    .auth_generation
                    .fetch_add(1, Ordering::Release);
                first.send(WsMessage::Pong(payload)).await.unwrap();

                let (second_stream, _) = listener.accept().await.unwrap();
                let mut second = async_tungstenite::accept_async(second_stream)
                    .await
                    .unwrap();
                assert!(matches!(second.next().await, Some(Ok(WsMessage::Text(_)))));
                server_creates.fetch_add(1, Ordering::Relaxed);
                second
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"id":"resp_second","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });
            let session = SessionRef::generate();
            let slot = provider.response_connection_slot(Some(&session)).unwrap();
            let (event_tx, _) = flume::unbounded();
            let body = serde_json::json!({"model":"test","input":[]});

            let (first_id, _) = provider
                .stream_websocket(
                    Some(Arc::clone(&slot)),
                    &body,
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &old_auth,
                    &old_credential_hash,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap();
            let (second_id, _) = provider
                .stream_websocket(
                    Some(slot),
                    &body,
                    &mut None,
                    false,
                    || Value::Null,
                    None,
                    None,
                    &event_tx,
                    &old_auth,
                    &old_credential_hash,
                    Duration::from_secs(2),
                    0,
                    None,
                )
                .await
                .unwrap();
            server.await;

            assert_eq!(first_id.as_deref(), Some("resp_first"));
            assert_eq!(second_id.as_deref(), Some("resp_second"));
            assert_eq!(creates.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn simultaneous_post_send_closes_emit_one_create_per_attempt() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let creates = Arc::new(AtomicUsize::new(0));
            let server_creates = Arc::clone(&creates);
            let server = smol::spawn(async move {
                let mut handlers = Vec::new();
                for index in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let handler_creates = Arc::clone(&server_creates);
                    handlers.push(smol::spawn(async move {
                        let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                        if matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))) {
                            handler_creates.fetch_add(1, Ordering::SeqCst);
                        }
                        socket
                            .send(WsMessage::Text(
                                serde_json::json!({
                                    "type":"response.created",
                                    "response":{"id":format!("resp_{index}")}
                                })
                                .to_string()
                                .into(),
                            ))
                            .await
                            .unwrap();
                        socket.close(None).await.unwrap();
                    }));
                }
                for handler in handlers {
                    handler.await;
                }
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth_options(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
                OpenAiOptions::codex(),
            )
            .unwrap();
            let model = Model::from_spec("codex/gpt-5.3-codex").unwrap();
            let messages = [Message::user("hello".into())];
            let tools = serde_json::json!([]);
            let (first_tx, _) = flume::unbounded();
            let (second_tx, _) = flume::unbounded();
            let first_session = SessionRef::generate();
            let second_session = SessionRef::generate();
            let system = System::from("");

            let first = provider.stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &first_tx,
                RequestOptions::default(),
                Some(&first_session),
            );
            let second = provider.stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &second_tx,
                RequestOptions::default(),
                Some(&second_session),
            );
            let (first_result, second_result) = futures::join!(first, second);
            server.await;

            assert_eq!(creates.load(Ordering::SeqCst), 2);
            for result in [first_result, second_result] {
                assert!(matches!(
                    result,
                    Err(AgentError::RequestSent {
                        metadata: Some(RequestDeliveryMetadata {
                            phase: RequestDeliveryPhase::Accepted,
                            ..
                        }),
                        ..
                    })
                ));
            }
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn cancelled_websocket_attempt_is_not_reused() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (request_tx, request_rx) = flume::bounded(1);
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                let _ = socket.next().await;
                request_tx.send_async(()).await.unwrap();
                let _ = socket.next().await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth.clone())),
                crate::providers::Timeouts::default(),
            )
            .unwrap();
            let session = SessionRef::generate();
            let slot = provider.response_connection_slot(Some(&session)).unwrap();
            let (event_tx, _event_rx) = flume::unbounded();
            let body = serde_json::json!({"model":"test","input":[]});
            let mut full_history_body = None;
            let attempt = provider.stream_websocket(
                Some(Arc::clone(&slot)),
                &body,
                &mut full_history_body,
                false,
                || Value::Null,
                None,
                None,
                &event_tx,
                &auth,
                TEST_CREDENTIAL_HASH,
                TEST_STREAM_TIMEOUT,
                0,
                None,
            );

            let cancelled = futures_lite::future::race(
                async {
                    let _ = attempt.await;
                    false
                },
                async {
                    request_rx.recv_async().await.unwrap();
                    true
                },
            )
            .await;

            assert!(cancelled);
            assert!(slot.lock().await.is_none());
            server.await;
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    fn stale_previous_response_id_recovers_after_websocket_rejection() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                let Some(Ok(WsMessage::Text(first))) = socket.next().await else {
                    panic!("expected initial response.create");
                };
                let first: Value = serde_json::from_str(&first).unwrap();
                assert!(first.get("previous_response_id").is_none());
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type": "response.completed",
                            "response": {"id": "resp_stale", "status": "completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                let Some(Ok(WsMessage::Ping(payload))) = socket.next().await else {
                    panic!("expected continuation preflight ping");
                };
                socket.send(WsMessage::Pong(payload)).await.unwrap();
                let Some(Ok(WsMessage::Text(continuation))) = socket.next().await else {
                    panic!("expected continuation response.create");
                };
                let continuation: Value = serde_json::from_str(&continuation).unwrap();
                assert_eq!(continuation["previous_response_id"], "resp_stale");
                assert_eq!(continuation["input"].as_array().unwrap().len(), 1);
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type": "error",
                            "status": 400,
                            "error": {
                                "code": "previous_response_not_found",
                                "message": "Previous response not found"
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();

                let (retry_stream, _) = listener.accept().await.unwrap();
                let mut retry = async_tungstenite::accept_async(retry_stream).await.unwrap();
                let Some(Ok(WsMessage::Text(full_history))) = retry.next().await else {
                    panic!("expected full-history response.create");
                };
                let full_history: Value = serde_json::from_str(&full_history).unwrap();
                assert!(full_history.get("previous_response_id").is_none());
                assert_eq!(full_history["input"].as_array().unwrap().len(), 3);
                retry
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "type": "response.completed",
                            "response": {"id": "resp_recovered", "status": "completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            });

            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let provider = OpenAi::with_auth_options(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
                OpenAiOptions::codex(),
            )
            .unwrap();
            let model = Model::from_spec("codex/gpt-5.3-codex").unwrap();
            let tools = serde_json::json!([]);
            let session = SessionRef::generate();
            let (event_tx, _) = flume::unbounded();
            let first_messages = [Message::user("hello".into())];
            provider
                .stream_message(
                    &model,
                    &first_messages,
                    &System::from(""),
                    &tools,
                    &event_tx,
                    RequestOptions::default(),
                    Some(&session),
                )
                .await
                .unwrap();

            let messages = [
                Message::user("hello".into()),
                assistant("hi"),
                Message::user("what next".into()),
            ];
            provider
                .stream_message(
                    &model,
                    &messages,
                    &System::from(""),
                    &tools,
                    &event_tx,
                    RequestOptions::default(),
                    Some(&session),
                )
                .await
                .unwrap();
            server.await;

            let response_id = provider
                .session_state
                .lock()
                .unwrap()
                .get(&canonical_session_key(&session))
                .and_then(|state| state.last_response_id.as_deref().map(ToOwned::to_owned));
            assert_eq!(response_id.as_deref(), Some("resp_recovered"));
        });
    }

    #[test]
    fn full_history_replay_requires_explicit_approval() {
        assert!(full_history_replay_required(None, 2, true, false));
        assert!(!full_history_replay_required(None, 2, false, false));
        assert!(!full_history_replay_required(None, 1, true, false));
        assert!(!full_history_replay_required(None, 2, true, true));
        assert!(!full_history_replay_required(
            Some("response"),
            2,
            true,
            false
        ));
    }

    #[test]
    fn pre_acceptance_missing_previous_rejections_allow_full_history_retry() {
        let attempt =
            |phase, status, message: &str, emitted_event, definitive_rejection| CodexAttempt {
                previous_response_id: Some("resp_1".into()),
                emitted_event,
                definitive_rejection,
                delivery: Some(RequestDeliveryMetadata::new(phase)),
                result: Err(AgentError::Api {
                    status,
                    message: message.into(),
                }),
            };

        for status in [0, 400, 404] {
            let message = if status == 400 {
                "previous_response_not_found: Previous response not found"
            } else {
                "not found: resp_1"
            };
            assert!(is_missing_previous_response(&attempt(
                RequestDeliveryPhase::NotSent,
                status,
                message,
                false,
                true,
            )));
            assert!(is_missing_previous_response(&attempt(
                RequestDeliveryPhase::SentAwaitingAcceptance,
                status,
                message,
                false,
                true,
            )));
        }
        assert!(!is_missing_previous_response(&attempt(
            RequestDeliveryPhase::Accepted,
            400,
            "previous_response_not_found: Previous response not found",
            false,
            true,
        )));
        assert!(!is_missing_previous_response(&attempt(
            RequestDeliveryPhase::NotSent,
            404,
            "not found: resp_other",
            false,
            true,
        )));
        assert!(!is_missing_previous_response(&attempt(
            RequestDeliveryPhase::NotSent,
            404,
            "not found: resp_1",
            true,
            true,
        )));
        assert!(!is_missing_previous_response(&attempt(
            RequestDeliveryPhase::NotSent,
            404,
            "not found: resp_1",
            false,
            false,
        )));
    }

    #[test]
    fn coding_plan_admission_retries_before_response_create() {
        let attempt = |phase, emitted_event, error: AgentError, definitive| CodexAttempt {
            previous_response_id: Some("resp_1".into()),
            emitted_event,
            definitive_rejection: definitive,
            delivery: Some(RequestDeliveryMetadata::new(phase)),
            result: Err(error),
        };
        let admission = |retry_after| AgentError::CodingPlanAdmission {
            transport: CodingPlanAdmissionTransport::WebSocket,
            retry_after,
        };

        // `Retry-After: 7` is honoured verbatim, without jitter or backoff growth.
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    admission(Some(Duration::from_secs(7))),
                    true,
                ),
                0,
                0.0,
            ),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    admission(Some(Duration::from_secs(7))),
                    true,
                ),
                3,
                1.0,
            ),
            Some(Duration::from_secs(7))
        );
        // A `Retry-After` below the floor or above the ceiling is clamped.
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    admission(Some(Duration::from_millis(1))),
                    true,
                ),
                0,
                1.0,
            ),
            Some(CODING_PLAN_DEFAULT_RETRY_DELAY)
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    admission(Some(Duration::from_secs(600))),
                    true,
                ),
                0,
                1.0,
            ),
            Some(CODING_PLAN_MAX_RETRY_AFTER)
        );
        assert!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::SentAwaitingAcceptance,
                    false,
                    admission(Some(Duration::from_secs(7))),
                    true,
                ),
                0,
                1.0,
            )
            .is_none()
        );
        assert!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    admission(Some(Duration::from_secs(7))),
                    true,
                ),
                CODING_PLAN_ADMISSION_MAX_RETRIES,
                1.0,
            )
            .is_none()
        );

        // Without a `Retry-After` the delay grows exponentially and is jittered.
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(RequestDeliveryPhase::NotSent, false, admission(None), true),
                0,
                1.0,
            ),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(RequestDeliveryPhase::NotSent, false, admission(None), true),
                2,
                1.0,
            ),
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(RequestDeliveryPhase::NotSent, false, admission(None), true),
                2,
                0.0,
            ),
            Some(Duration::from_millis(500))
        );

        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    AgentError::CodingPlanAdmissionTimeout { millis: 15_000 },
                    false,
                ),
                0,
                1.0,
            ),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    AgentError::CodingPlanAdmissionTimeout { millis: 15_000 },
                    false,
                ),
                1,
                1.0,
            ),
            Some(Duration::from_millis(500))
        );
        assert!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    false,
                    AgentError::CodingPlanAdmissionTimeout { millis: 15_000 },
                    false,
                ),
                CODING_PLAN_ADMISSION_MAX_RETRIES,
                1.0,
            )
            .is_none()
        );
        assert!(
            coding_plan_admission_retry_delay_with_jitter(
                &attempt(
                    RequestDeliveryPhase::NotSent,
                    true,
                    AgentError::CodingPlanAdmissionTimeout { millis: 15_000 },
                    false,
                ),
                0,
                1.0,
            )
            .is_none()
        );
    }

    #[test]
    fn coding_plan_backoff_grows_and_saturates_within_the_jitter_window() {
        for retry_count in 0..=CODING_PLAN_ADMISSION_MAX_RETRIES {
            let ceiling = coding_plan_backoff(retry_count, 1.0);
            let floor = coding_plan_backoff(retry_count, 0.0);
            assert!(floor <= ceiling);
            assert!(
                floor >= CODING_PLAN_DEFAULT_RETRY_DELAY,
                "jitter must not take retry {retry_count} below the floor, got {floor:?}"
            );
            assert!(ceiling <= CODING_PLAN_MAX_RETRY_DELAY);
        }
        // The floor only binds the first step. Past it the window is the full
        // `[ceiling / 2, ceiling]`.
        for retry_count in 1..=CODING_PLAN_ADMISSION_MAX_RETRIES {
            assert_eq!(
                coding_plan_backoff(retry_count, 0.0) * 2,
                coding_plan_backoff(retry_count, 1.0)
            );
        }
        assert!(coding_plan_backoff(1, 1.0) > coding_plan_backoff(0, 1.0));
        // The ceiling saturates instead of overflowing on a large retry count.
        assert_eq!(
            coding_plan_backoff(u8::MAX, 1.0),
            coding_plan_backoff(CODING_PLAN_ADMISSION_MAX_RETRIES, 1.0)
        );
    }

    /// A durable chain used to survive a failed turn. With `store: false` the
    /// service evicts the cached `previous_response_id` on any 4xx/5xx and keeps
    /// no persisted copy, so holding the id only guarantees a
    /// `previous_response_not_found` on the next turn.
    #[test]
    fn failed_continuation_clears_the_response_chain_even_when_durable() {
        let success: Result<(), AgentError> = Ok(());
        assert!(!should_clear_response_chain(&success));

        let transport_error: Result<(), AgentError> =
            Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into());
        assert!(should_clear_response_chain(&transport_error));

        let api_error: Result<(), AgentError> = Err(AgentError::Api {
            status: 500,
            message: "temporary".into(),
        });
        assert!(should_clear_response_chain(&api_error));
    }

    #[test]
    fn http_fallback_requires_transport_failure_before_output() {
        let transport = super::super::websocket::WebSocketAttemptError {
            error: Box::new(
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
            ),
            transport_failure: true,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::NotSent,
            )),
        };
        assert!(should_fallback_to_http(&transport));

        let after_output = super::super::websocket::WebSocketAttemptError {
            delivery: {
                let mut d = crate::RequestDeliveryMetadata::new(
                    crate::RequestDeliveryPhase::SentAwaitingAcceptance,
                );
                d.emitted_event = true;
                Box::new(d)
            },
            ..transport
        };
        assert!(!should_fallback_to_http(&after_output));

        let auth = super::super::websocket::WebSocketAttemptError {
            error: Box::new(AgentError::Api {
                status: 401,
                message: "expired".into(),
            }),
            transport_failure: true,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::NotSent,
            )),
        };
        assert!(!should_fallback_to_http(&auth));

        // A WebSocket-origin admission 403 retries over HTTP, which live probes
        // show still accepting the same credentials.
        let ws_admission = super::super::websocket::WebSocketAttemptError {
            error: Box::new(AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::WebSocket,
                retry_after: None,
            }),
            transport_failure: true,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::NotSent,
            )),
        };
        assert!(should_fallback_to_http(&ws_admission));

        // An HTTP-origin admission 403 has no remaining transport to fall back to.
        let http_admission = super::super::websocket::WebSocketAttemptError {
            error: Box::new(AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::Http,
                retry_after: Some(Duration::from_secs(3)),
            }),
            transport_failure: true,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::NotSent,
            )),
        };
        assert!(!should_fallback_to_http(&http_admission));

        let response_error = super::super::websocket::WebSocketAttemptError {
            error: Box::new(AgentError::Api {
                status: 500,
                message: "server".into(),
            }),
            transport_failure: false,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::SentAwaitingAcceptance,
            )),
        };
        assert!(!should_fallback_to_http(&response_error));

        let after_send = super::super::websocket::WebSocketAttemptError {
            error: Box::new(
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
            ),
            transport_failure: true,
            delivery: Box::new(crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::SentAwaitingAcceptance,
            )),
        };
        assert!(!should_fallback_to_http(&after_send));
    }

    #[test_case(400, false, false)]
    #[test_case(401, false, true)]
    #[test_case(429, true, false)]
    #[test_case(500, true, false)]
    fn provider_status_after_http_send_is_not_request_sent(
        status: u16,
        retryable: bool,
        auth_error: bool,
    ) {
        let error = suppress_retry_after_send(AgentError::Api {
            status,
            message: "provider rejected an already-written request".into(),
        });

        assert!(!matches!(error, AgentError::RequestSent { .. }));
        assert_eq!(error.is_retryable(), retryable);
        assert_eq!(error.is_auth_error(), auth_error);
    }

    #[test]
    fn parse_rate_limit_status_response() {
        let body = r#"{
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 6, "limit_window_seconds": 18000, "reset_at": 1738300000},
                "secondary_window": {"used_percent": 24, "limit_window_seconds": 604800, "reset_at": 1738900000}
            },
            "additional_rate_limits": [
                {
                    "limit_name": "code_review",
                    "metered_feature": "code_review",
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "secondary_window": {"used_percent": 91, "limit_window_seconds": 604800, "reset_at": 1738900000}
                    }
                }
            ],
            "credits": {"has_credits": true, "unlimited": false, "balance": "5.39"}
        }"#;
        let parsed: RateLimitStatusResponse = serde_json::from_str(body).unwrap();
        let usage: ProviderUsage = parsed.into();
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        assert_eq!(usage.limits.len(), 4);
        assert_eq!(usage.limits[0].label, "5h limit");
        assert_eq!(usage.limits[0].percentage, Some(6));
        assert_eq!(usage.limits[0].reset_at, Some(1_738_300_000_000));
        assert_eq!(usage.limits[1].label, "Weekly limit");
        assert_eq!(usage.limits[1].percentage, Some(24));
        assert_eq!(usage.limits[2].label, "Code Review Weekly limit");
        assert_eq!(usage.limits[2].percentage, Some(91));
        assert_eq!(usage.limits[3].label, "Credits");
        assert_eq!(usage.limits[3].percentage, None);
        assert_eq!(usage.limits[3].detail.as_deref(), Some("$5.39 remaining"));
    }

    #[test]
    fn parse_rate_limit_status_unknown_plan_type() {
        let body = r#"{"plan_type": "prolite"}"#;
        let parsed: RateLimitStatusResponse = serde_json::from_str(body).unwrap();
        let usage: ProviderUsage = parsed.into();
        assert_eq!(usage.plan.as_deref(), Some("prolite"));
        assert!(usage.limits.is_empty());
    }

    #[test_case(Some(18_000), Some("5h"))]
    #[test_case(Some(86_400), Some("daily"))]
    #[test_case(Some(604_800), Some("weekly"))]
    #[test_case(Some(2_592_000), Some("monthly"))]
    #[test_case(Some(31_536_000), Some("annual"))]
    #[test_case(Some(120), None)]
    fn rate_limit_window_label_maps(seconds: Option<i64>, expected: Option<&str>) {
        assert_eq!(super::rate_limit_window_label(seconds), expected);
    }

    /// The HTTP fallback runs only after the WebSocket 403, so an empty 403
    /// from it too must still reach the admission retry loop. Marking it sent
    /// would end the turn on the first rejection.
    #[test]
    fn an_http_fallback_admission_rejection_stays_retryable() {
        let retryable = http_fallback_attempt(
            None,
            AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::Http,
                retry_after: Some(Duration::from_secs(7)),
            },
            Some(Duration::from_secs(2)),
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(&retryable, 0, 0.0),
            Some(Duration::from_secs(7)),
            "the HTTP response's own Retry-After wins over the WebSocket's"
        );

        // An empty HTTP 403 keeps the delay the WebSocket rejection asked for
        // instead of restarting on the short local schedule.
        let inherited = http_fallback_attempt(
            None,
            AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::Http,
                retry_after: None,
            },
            Some(Duration::from_secs(11)),
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(&inherited, 0, 0.0),
            Some(Duration::from_secs(11))
        );

        // With neither transport supplying one, the local schedule applies.
        let local = http_fallback_attempt(
            None,
            AgentError::CodingPlanAdmission {
                transport: CodingPlanAdmissionTransport::Http,
                retry_after: None,
            },
            None,
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(&local, 0, 0.0),
            Some(CODING_PLAN_DEFAULT_RETRY_DELAY)
        );

        // Any other fallback error may have arrived mid-stream.
        let mid_stream = http_fallback_attempt(
            None,
            AgentError::Api {
                status: 500,
                message: "boom".into(),
            },
            Some(Duration::from_secs(3)),
        );
        assert_eq!(
            coding_plan_admission_retry_delay_with_jitter(&mid_stream, 0, 0.0),
            None
        );
    }
}

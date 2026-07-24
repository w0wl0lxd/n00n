use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use flume::Sender;
use n00n_storage::StateDir;
use n00n_storage::id::{N00nId, SessionRef};
use n00n_storage::now_epoch;
use n00n_storage::sessions::{
    OPENAI_RESPONSE_CHAIN_TTL_SECONDS, OpenAiResponseChainLock, StoredOpenAiResponseChain,
    delete_openai_response_chain, load_openai_response_chain, openai_response_chain_parent_exists,
    save_openai_response_chain, try_lock_openai_response_chain,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::model::Model;
use crate::provider::{BoxFuture, Provider};
use crate::{
    AgentError, Message, ProviderEvent, RequestDeliveryMetadata, RequestDeliveryPhase,
    RequestOptions, StreamResponse, dialect,
};

use super::auth;
use crate::providers::ResolvedAuth;
use crate::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    slug: "openai",
    api_key_env: "OPENAI_API_KEY",
    base_url: "https://api.openai.com/v1",
    max_tokens_field: "max_completion_tokens",
    include_stream_usage: true,
    provider_name: "OpenAI",
    supports_prompt_cache_key: true,
    supports_prompt_cache_breakpoint: true,
};

// Non-codex models OpenAI offers for subscription usage via the Coding Plan.
// Codex models are matched by their `-codex` substring in
// `coding_plan_context_window`, so they never need listing here.
pub(crate) const PLAN_MODELS: &[&str] = &[
    "gpt-5.6",
    "gpt-5.6-luna",
    "gpt-5.6-terra",
    "gpt-5.6-sol",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.2",
];

const CODEX_PLAN_CONTEXT_WINDOW: u32 = 272_000;
const GPT_5_6_PLAN_CONTEXT_WINDOW: u32 = 272_000;
const SESSION_STATE_TTL: Duration = Duration::from_hours(1);
const FIVE_MINUTES_MILLIS: u64 = 5 * 60 * 1_000;
const THIRTY_MINUTES_MILLIS: u64 = 30 * 60 * 1_000;
const CODING_PLAN_DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(250);
const CODING_PLAN_MAX_SLOTS: u8 = 8;
const RESPONSE_CHAIN_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const RESPONSE_CHAIN_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

static PROCESS_INSTANCE_NONCE: OnceLock<u64> = OnceLock::new();

fn coding_plan_slot_count(slots: u64) -> u8 {
    match u8::try_from(slots.clamp(1, u64::from(CODING_PLAN_MAX_SLOTS))) {
        Ok(slots) => slots,
        Err(_) => CODING_PLAN_MAX_SLOTS,
    }
}

type ResponseOperationSlot = Arc<AsyncMutex<()>>;

#[derive(Debug, Clone, Copy)]
pub struct OpenAiOptions {
    coding_plan_slots: u8,
}

impl OpenAiOptions {
    #[must_use]
    pub fn with_coding_plan_slots(slots: u64) -> Self {
        Self {
            coding_plan_slots: coding_plan_slot_count(slots),
        }
    }
}

impl Default for OpenAiOptions {
    fn default() -> Self {
        Self::with_coding_plan_slots(u64::from(CODING_PLAN_MAX_SLOTS / 2))
    }
}

impl From<&n00n_config::ProviderConfig> for OpenAiOptions {
    fn from(config: &n00n_config::ProviderConfig) -> Self {
        Self::with_coding_plan_slots(config.openai_coding_plan_slots)
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
    store: bool,
    emitted_event: bool,
    definitive_rejection: bool,
    delivery: Option<RequestDeliveryMetadata>,
    result: Result<StreamResponse, AgentError>,
}

impl CodexAttempt {
    fn from_websocket_error(
        previous_response_id: Option<String>,
        store: bool,
        error: super::websocket::WebSocketAttemptError,
    ) -> Self {
        let emitted_event = error.emitted_event;
        let definitive_rejection = error.definitive_rejection();
        let delivery = Some(error.delivery.clone());
        let provider_error = error.into_agent_error();
        Self {
            previous_response_id,
            store,
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

fn should_fallback_to_http(error: &super::websocket::WebSocketAttemptError) -> bool {
    error.transport_failure
        && !error.emitted_event
        && !error.error.is_auth_error()
        && !matches!(&error.error, AgentError::CodingPlanAdmission { .. })
        && error.delivery.phase == RequestDeliveryPhase::NotSent
}

fn not_sent_websocket_error(error: AgentError) -> super::websocket::WebSocketAttemptError {
    super::websocket::WebSocketAttemptError::transport(
        error,
        false,
        RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent),
    )
}

fn suppress_retry_after_send(error: AgentError) -> AgentError {
    match error {
        error @ AgentError::RequestSent { .. } => error,
        error => AgentError::RequestSent {
            message: error.to_string(),
            metadata: Some(RequestDeliveryMetadata::new(
                RequestDeliveryPhase::SentAwaitingAcceptance,
            )),
        },
    }
}

fn canonical_session_key(session_id: &SessionRef) -> N00nId {
    session_id.id()
}

fn canonical_prompt_cache_key(session_id: &SessionRef) -> String {
    canonical_session_key(session_id).to_string()
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
    tools_hash: &str,
    auth_scope_hash: &str,
    messages: &'a [Message],
) -> Result<(Option<String>, &'a [Message]), serde_json::Error> {
    if state.tools_hash.as_deref() != Some(tools_hash)
        || state.auth_scope_hash.as_deref() != Some(auth_scope_hash)
        || messages.len() < state.last_message_count
    {
        if state.last_response_id.is_some() {
            debug!(
                chain_reset = true,
                chain_reset_reason = "request_prefix_scope_changed",
                "resetting OpenAI response chain"
            );
        }
        *state = OpenAiSessionState {
            tools_hash: Some(tools_hash.to_owned()),
            auth_scope_hash: Some(auth_scope_hash.to_owned()),
            ..Default::default()
        };
    }

    if state.last_message_count > 0 {
        let current_hash = stable_json_hash(&messages[..state.last_message_count])?;
        if state.messages_hash.as_deref() != Some(current_hash.as_str()) {
            debug!(
                chain_reset = true,
                chain_reset_reason = "message_prefix_changed",
                "resetting OpenAI response chain"
            );
            *state = OpenAiSessionState {
                tools_hash: Some(tools_hash.to_owned()),
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
        debug!(
            chain_reset = true,
            chain_reset_reason = "no_new_input_after_response",
            "resetting OpenAI response chain"
        );
        *state = OpenAiSessionState {
            tools_hash: Some(tools_hash.to_owned()),
            auth_scope_hash: Some(auth_scope_hash.to_owned()),
            ..Default::default()
        };
    }

    Ok((None, &messages[state.last_message_count..]))
}

fn record_in_state(
    state: &mut OpenAiSessionState,
    response_id: Option<String>,
    tools_hash: &str,
    auth_scope_hash: &str,
    messages: &[Message],
) -> Result<(), serde_json::Error> {
    if let Some(response_id) = response_id {
        *state = OpenAiSessionState {
            last_response_id: Some(response_id),
            last_message_count: messages.len(),
            tools_hash: Some(tools_hash.to_owned()),
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

pub(crate) fn is_codex_model(model_id: &str) -> bool {
    coding_plan_context_window(model_id).is_some()
}

// Codex models match by substring so future releases route without a registry
// edit; the named non-codex plans match exactly to avoid catching near-misses
// like `gpt-5.6-terra-preview`.
fn coding_plan_context_window(model_id: &str) -> Option<u32> {
    if model_id.contains("-codex") {
        return Some(CODEX_PLAN_CONTEXT_WINDOW);
    }
    if !PLAN_MODELS.contains(&model_id) {
        return None;
    }
    Some(if model_id.starts_with("gpt-5.6") {
        GPT_5_6_PLAN_CONTEXT_WINDOW
    } else {
        CODEX_PLAN_CONTEXT_WINDOW
    })
}

pub struct OpenAi {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    auth_refresh: AsyncMutex<()>,
    auth_generation: AtomicU64,
    auth_managed: bool,
    storage: Option<StateDir>,
    response_state_storage: Option<StateDir>,
    websocket_connect_timeout: Duration,
    coding_plan_slots: u8,
    system_prefix: Option<String>,
    session_state: Arc<Mutex<HashMap<N00nId, OpenAiSessionState>>>,
    response_connections: Arc<Mutex<HashMap<N00nId, ResponseConnectionSlot>>>,
    response_operations: Arc<Mutex<HashMap<N00nId, Weak<AsyncMutex<()>>>>>,
}

impl OpenAi {
    pub fn new(timeouts: crate::providers::Timeouts) -> Result<Self, AgentError> {
        Self::new_with_options(timeouts, OpenAiOptions::default())
    }

    pub fn new_with_options(
        timeouts: crate::providers::Timeouts,
        options: OpenAiOptions,
    ) -> Result<Self, AgentError> {
        let storage = StateDir::resolve()?;
        // Authentication refresh is deferred to the first request. Token files
        // are atomically replaced, so startup can safely read the cached copy
        // without waiting behind another process's network refresh.
        let resolved = auth::resolve_cached(&storage)?;
        let compat = OpenAiCompatProvider::new(&CONFIG, timeouts)?;
        Ok(Self {
            compat,
            auth: Arc::new(Mutex::new(resolved)),
            auth_refresh: AsyncMutex::new(()),
            auth_generation: AtomicU64::new(0),
            auth_managed: true,
            storage: Some(storage.clone()),
            response_state_storage: Some(storage),
            websocket_connect_timeout: timeouts.connect,
            coding_plan_slots: options.coding_plan_slots,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
            response_connections: Arc::new(Mutex::new(HashMap::new())),
            response_operations: Arc::new(Mutex::new(HashMap::new())),
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
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            auth_refresh: AsyncMutex::new(()),
            auth_generation: AtomicU64::new(0),
            auth_managed: false,
            storage: None,
            response_state_storage: None,
            websocket_connect_timeout: timeouts.connect,
            coding_plan_slots: options.coding_plan_slots,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
            response_connections: Arc::new(Mutex::new(HashMap::new())),
            response_operations: Arc::new(Mutex::new(HashMap::new())),
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
        self.auth_managed && self.storage.as_ref().is_some_and(auth::is_oauth)
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
        let Some(tokens) = n00n_storage::auth::load_tokens(storage, auth::PROVIDER) else {
            let mut resolved = auth::resolve_cached(storage)?;
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
            self.synchronize_oauth_tokens(refresh_basis, force_refresh, attempt_nonce)
                .await?
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
    ) -> Result<ScopedResponsesWebSocket, super::websocket::WebSocketAttemptError> {
        loop {
            let auth = self
                .pre_send_auth(attempt_nonce)
                .await
                .map_err(not_sent_websocket_error)?;
            if auth.generation != self.auth_generation.load(Ordering::Acquire) {
                continue;
            }
            let socket = super::websocket::ResponsesWebSocket::connect(
                &auth.resolved,
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

    fn stores_responses(&self, auth: &ResolvedAuth) -> bool {
        self.storage.is_some()
            && self.response_state_storage.is_some()
            && auth.base_url.as_deref() == Some(CONFIG.base_url)
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
        chain_session: Option<N00nId>,
        admission_scope: Option<&str>,
        event_tx: &Sender<ProviderEvent>,
        _auth: &ResolvedAuth,
        credential_hash: &str,
        stream_timeout: Duration,
        attempt_nonce: u64,
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
                scoped = Some(self.connect_current_websocket(attempt_nonce).await?);
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
        tools_hash: &str,
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
        incremental_for_state(state, tools_hash, auth_scope_hash, messages)
            .map_err(AgentError::Json)
    }

    async fn record_response(
        &self,
        session_id: Option<&SessionRef>,
        response_id: Option<String>,
        tools_hash: &str,
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
                record_in_state(state, response_id, tools_hash, auth_scope_hash, messages)
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

    fn reset_connection_local_chain(&self, session_id: Option<N00nId>) {
        if let Some(session_id) = session_id {
            self.session_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&session_id);
        }
    }

    fn clear_local_response_chain(&self, session_id: N00nId) {
        self.session_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
        self.response_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
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
    ) -> CodexAttempt {
        if attempt.previous_response_id.is_some()
            && (is_missing_previous_response(&attempt)
                || should_clear_response_chain(&attempt.result, attempt.store))
        {
            self.clear_response_chain(session_id, response_chain_lock)
                .await;
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
        system: &str,
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
        let store = self.stores_responses(auth);
        let admission = match self
            .acquire_coding_plan_admission(auth, attempt_nonce)
            .await
        {
            Ok(admission) => admission,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    store,
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
                        store,
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
        let stream_timeout = self.compat.stream_timeout();
        let connection_reusable = self
            .response_connection_is_reusable(
                session_id,
                &socket_credential_hash,
                stream_timeout,
                attempt_nonce,
            )
            .await;
        if !store && !connection_reusable {
            debug!(
                chain_reset = true,
                chain_reset_reason = "socket_not_reusable",
                "resetting connection-local OpenAI response chain"
            );
            self.clear_response_chain(session_id, response_chain_lock.as_ref())
                .await;
        }
        let (previous_response_id, incremental_messages) = match self
            .prepare_request(
                session_id,
                tools_hash,
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
                    store,
                    emitted_event: false,
                    definitive_rejection: false,
                    delivery: None,
                    result: Err(error),
                };
            }
        };
        let prompt_cache_key = session_id.map(canonical_prompt_cache_key);
        let body = super::websocket::build_request_body(
            model,
            incremental_messages,
            system,
            tools,
            opts,
            previous_response_id.as_deref(),
            prompt_cache_key.as_deref(),
            store,
        );
        let mut full_history_body = None;
        let full_history_fallback_available = !store && previous_response_id.is_some();
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
                            opts,
                            None,
                            prompt_cache_key.as_deref(),
                            false,
                        )
                    },
                    session_id.map(canonical_session_key),
                    admission_scope.as_deref(),
                    event_tx,
                    auth,
                    &socket_credential_hash,
                    stream_timeout,
                    attempt_nonce,
                )
                .await;
            match websocket_result {
                Ok((response_id, response)) => (response_id, response, true),
                Err(error) if should_fallback_to_http(&error) => {
                    warn!("OpenAI Responses WebSocket unavailable; falling back to HTTP");
                    let fallback_body = if store {
                        &body
                    } else {
                        full_history_body.get_or_insert_with(|| {
                            super::websocket::build_request_body(
                                model,
                                messages,
                                system,
                                tools,
                                opts,
                                None,
                                prompt_cache_key.as_deref(),
                                false,
                            )
                        })
                    };
                    log_responses_request(
                        "http_sse",
                        fallback_body,
                        messages.len(),
                        if store {
                            incremental_messages.len()
                        } else {
                            messages.len()
                        },
                        store && previous_response_id.is_some(),
                        !store,
                    );
                    let fallback_auth = loop {
                        let preflight = match self.pre_send_auth(attempt_nonce).await {
                            Ok(auth) => auth,
                            Err(error) => {
                                return self
                                    .finish_codex_attempt(
                                        CodexAttempt {
                                            previous_response_id,
                                            store,
                                            emitted_event: false,
                                            definitive_rejection: false,
                                            delivery: Some(RequestDeliveryMetadata::new(
                                                RequestDeliveryPhase::NotSent,
                                            )),
                                            result: Err(error),
                                        },
                                        session_id,
                                        response_chain_lock.as_ref(),
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
                                        store,
                                        emitted_event: false,
                                        definitive_rejection: false,
                                        delivery: Some(RequestDeliveryMetadata::new(
                                            RequestDeliveryPhase::NotSent,
                                        )),
                                        result: Err(AgentError::CodingPlanAdmissionScopeChanged),
                                    },
                                    session_id,
                                    response_chain_lock.as_ref(),
                                )
                                .await;
                        }
                        Err(error) => {
                            return self
                                .finish_codex_attempt(
                                    CodexAttempt {
                                        previous_response_id,
                                        store,
                                        emitted_event: false,
                                        definitive_rejection: false,
                                        delivery: Some(RequestDeliveryMetadata::new(
                                            RequestDeliveryPhase::NotSent,
                                        )),
                                        result: Err(error),
                                    },
                                    session_id,
                                    response_chain_lock.as_ref(),
                                )
                                .await;
                        }
                    }
                    match super::responses::do_stream(
                        self.compat.client(),
                        model,
                        fallback_body,
                        event_tx,
                        &fallback_auth.resolved,
                        stream_timeout,
                    )
                    .await
                    {
                        Ok((response_id, response)) => (response_id, response, store),
                        Err(error) => {
                            return self
                                .finish_codex_attempt(
                                    CodexAttempt {
                                        previous_response_id,
                                        store,
                                        emitted_event: true,
                                        definitive_rejection: false,
                                        delivery: None,
                                        result: Err(suppress_retry_after_send(error)),
                                    },
                                    session_id,
                                    response_chain_lock.as_ref(),
                                )
                                .await;
                        }
                    }
                }
                Err(error) => {
                    return self
                        .finish_codex_attempt(
                            CodexAttempt::from_websocket_error(previous_response_id, store, error),
                            session_id,
                            response_chain_lock.as_ref(),
                        )
                        .await;
                }
            }
        };
        self.record_response(
            session_id,
            chainable.then_some(response_id).flatten(),
            tools_hash,
            &state_scope_hash,
            messages,
            store,
            response_chain_lock.as_ref(),
        )
        .await;
        CodexAttempt {
            previous_response_id,
            store,
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
        system: &str,
        tools: &Value,
        tools_hash: &str,
        event_tx: &Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&SessionRef>,
        durable_chain: bool,
    ) -> CodexAttempt {
        let attempt_nonce = fastrand::u64(..);
        let coding_plan_auth = match self.coding_plan_auth(false, None, attempt_nonce).await {
            Ok(auth) => auth,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    store: false,
                    emitted_event: false,
                    definitive_rejection: false,
                    delivery: None,
                    result: Err(error),
                };
            }
        };
        let attempt = self
            .run_codex_attempt(
                model,
                messages,
                system,
                tools,
                tools_hash,
                event_tx,
                opts,
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
            return self
                .run_codex_attempt(
                    model,
                    messages,
                    system,
                    tools,
                    tools_hash,
                    event_tx,
                    opts,
                    session_id,
                    durable_chain,
                    &current.resolved,
                    attempt_nonce,
                )
                .await;
        }
        if let Some(delay) = coding_plan_admission_retry_delay(&attempt) {
            debug!(
                process_instance_nonce = process_instance_nonce(),
                attempt_nonce,
                phase = "request_admission_retry",
                retry_delay_ms = delay.as_millis(),
                "retrying definitively unsent OpenAI Coding Plan admission rejection"
            );
            smol::Timer::after(delay).await;
            return self
                .run_codex_attempt(
                    model,
                    messages,
                    system,
                    tools,
                    tools_hash,
                    event_tx,
                    opts,
                    session_id,
                    durable_chain,
                    &coding_plan_auth.resolved,
                    attempt_nonce,
                )
                .await;
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
        self.run_codex_attempt(
            model,
            messages,
            system,
            tools,
            tools_hash,
            event_tx,
            opts,
            session_id,
            durable_chain,
            &refreshed.resolved,
            attempt_nonce,
        )
        .await
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
        let session_id = session_id?;
        let session_id = canonical_session_key(session_id);
        let mut operations = self
            .response_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operations.retain(|_, operation| operation.strong_count() > 0);
        if let Some(operation) = operations.get(&session_id).and_then(Weak::upgrade) {
            return Some(operation);
        }
        let operation = Arc::new(AsyncMutex::new(()));
        operations.insert(session_id, Arc::downgrade(&operation));
        Some(operation)
    }
}

impl Provider for OpenAi {
    #[allow(clippy::large_futures)]
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let mut buf = String::new();
            let system = super::super::with_prefix(self.system_prefix.as_deref(), system, &mut buf);

            if is_codex_model(&model.id) {
                let operation_slot = self.response_operation_slot(session_id);
                let _operation_guard = match operation_slot.as_ref() {
                    Some(operation) => Some(operation.lock().await),
                    None => None,
                };
                let durable_chain = session_id.is_some() && self.response_state_storage.is_some();
                let tools_hash = stable_json_hash(tools)?;
                let attempt = self
                    .run_codex_attempt_with_auth_retry(
                        model,
                        messages,
                        system,
                        tools,
                        &tools_hash,
                        event_tx,
                        opts,
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

                warn!(
                    chain_reset = true,
                    full_history_fallback = true,
                    "OpenAI Responses chain was not found; retrying with full history"
                );
                return self
                    .run_codex_attempt_with_auth_retry(
                        model,
                        messages,
                        system,
                        tools,
                        &tools_hash,
                        event_tx,
                        opts,
                        session_id,
                        durable_chain,
                    )
                    .await
                    .result;
            }

            let prompt_cache_key = session_id.map(canonical_prompt_cache_key);
            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                prompt_cache_key.as_deref(),
            );
            opts.thinking
                .apply_reasoning_effort(&mut body, &dialect::STANDARD, model);
            self.with_oauth_retry(|| async {
                let auth = self.current_auth();
                self.compat
                    .do_stream(model, &[], &body, event_tx, &auth)
                    .await
            })
            .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async {
            if self.is_oauth() {
                let models = super::models()
                    .iter()
                    .flat_map(|e| e.prefixes.iter())
                    .filter(|id| is_codex_model(id))
                    .map(|&s| crate::model::ModelInfo::id_only(s.to_string()))
                    .collect();
                return Ok(models);
            }
            self.with_oauth_retry(|| async {
                let auth = self.current_auth();
                self.compat.do_list_models(&auth).await
            })
            .await
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
            let resolved = smol::unblock(move || auth::resolve_cached(&storage)).await?;
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
        let coding_plan_auth =
            self.current_auth().base_url.as_deref() == Some(auth::CODING_PLAN_BASE_URL);
        if (coding_plan_auth || self.is_oauth())
            && let Some(context_window) = coding_plan_context_window(&model.id)
        {
            model.context_window = model.context_window.min(context_window);
        }
    }
}

fn coding_plan_admission_retry_delay(attempt: &CodexAttempt) -> Option<Duration> {
    if attempt.emitted_event
        || !attempt.definitive_rejection
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
    let Err(AgentError::CodingPlanAdmission { retry_after }) = &attempt.result else {
        return None;
    };
    let delay = match retry_after {
        Some(delay) => *delay,
        None => CODING_PLAN_DEFAULT_RETRY_DELAY,
    };
    Some(delay.max(CODING_PLAN_DEFAULT_RETRY_DELAY))
}

fn is_missing_previous_response(attempt: &CodexAttempt) -> bool {
    if attempt.emitted_event
        || !attempt.definitive_rejection
        || !matches!(
            &attempt.delivery,
            Some(RequestDeliveryMetadata {
                phase: RequestDeliveryPhase::NotSent,
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

fn should_clear_response_chain<T>(result: &Result<T, AgentError>, store: bool) -> bool {
    match result {
        Err(AgentError::Api { status, .. }) => !store || !(*status == 429 || *status >= 500),
        Err(_) => !store,
        Ok(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_tungstenite::tungstenite::Message as WsMessage;
    use futures_lite::StreamExt;
    use tempfile::TempDir;
    use test_case::test_case;

    use super::*;
    use crate::{ContentBlock, Role, TokenUsage};

    const TOOLS_HASH: &str = "[]";
    const AUTH_SCOPE_HASH: &str = "account";
    const LEGACY_SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";
    const TEST_CREDENTIAL_HASH: &str = "test-credential";
    const TEST_STREAM_TIMEOUT: Duration = Duration::from_secs(30);

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
            "[]",
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[]", AUTH_SCOPE_HASH, &second).unwrap();

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
            "[]",
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

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[]", AUTH_SCOPE_HASH, &second).unwrap();

        assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
        assert_eq!(incremental_messages.len(), 1);
        assert!(matches!(
            &incremental_messages[0].content[0],
            ContentBlock::ToolResult { tool_use_id, content, .. }
                if tool_use_id == "call_1" && content == "result"
        ));
    }
    #[test]
    fn incremental_request_resets_when_tools_change() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            "[]",
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[\"new\"]", AUTH_SCOPE_HASH, &second).unwrap();

        assert!(previous_response_id.is_none());
        assert_eq!(incremental_messages.len(), second.len());
    }

    #[test_case("gpt-5.6")]
    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    #[test_case("gpt-5.5")]
    #[test_case("gpt-5.3-codex")]
    fn coding_plan_models_use_websocket(model_id: &str) {
        assert!(is_codex_model(model_id));
    }

    #[test_case("gpt-5.6", Some(272_000))]
    #[test_case("gpt-5.6-luna", Some(272_000))]
    #[test_case("gpt-5.6-terra", Some(272_000))]
    #[test_case("gpt-5.6-sol", Some(272_000))]
    #[test_case("gpt-5.5", Some(272_000))]
    #[test_case("gpt-5.4", Some(272_000))]
    #[test_case("gpt-5.2", Some(272_000))]
    #[test_case("gpt-5.3-codex", Some(272_000))]
    #[test_case("gpt-5.7-codex", Some(272_000) ; "unlisted codex model still routes")]
    #[test_case("gpt-5.5-preview", None ; "non_plan_5_5_preview_rejected")]
    #[test_case("gpt-5.6-terra-preview", None ; "non_plan_5_6_preview_rejected")]
    #[test_case("gpt-5.6-codex", Some(272_000) ; "codex_model")]
    #[test_case("gpt-5.4-nano", None ; "non_plan_5_4_nano_rejected")]
    fn coding_plan_context_window_resolves_plan_models(model_id: &str, expected: Option<u32>) {
        assert_eq!(coding_plan_context_window(model_id), expected);
    }

    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    fn coding_plan_adjustment_caps_authenticated_gpt_5_6_at_272k(model_id: &str) {
        let auth = Arc::new(Mutex::new(ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: Vec::new(),
        }));
        let provider = OpenAi::with_auth(auth, crate::providers::Timeouts::default()).unwrap();
        let mut model = Model::from_spec(&format!("openai/{model_id}")).unwrap();

        provider.adjust_model(&mut model);

        assert_eq!(model.context_window, 272_000);
    }

    #[test]
    fn incremental_first_turn_sends_full_messages() {
        let mut state = OpenAiSessionState::default();
        let messages = vec![Message::user("hello".into())];
        let (prev, inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &messages).unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 1);
        assert!(matches!(inc[0].role, Role::User));
    }

    #[test]
    fn incremental_second_turn_skips_assistant_message() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        let (prev, _inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &first).unwrap();
        assert!(prev.is_none());

        record_in_state(
            &mut state,
            Some("resp_1".into()),
            TOOLS_HASH,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &second).unwrap();

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
        let (prev, _inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &first).unwrap();
        assert!(prev.is_none());
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            TOOLS_HASH,
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
        let (prev, inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &second).unwrap();

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
            TOOLS_HASH,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) =
            incremental_for_state(&mut state, "[\"new\"]", AUTH_SCOPE_HASH, &second).unwrap();

        assert!(prev.is_none());
        assert_eq!(inc.len(), 3);
        assert_eq!(state.tools_hash, Some("[\"new\"]".to_string()));
    }

    #[test]
    fn incremental_messages_shrink_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("a".into()), Message::user("b".into())];
        record_in_state(
            &mut state,
            Some("resp_1".into()),
            TOOLS_HASH,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![Message::user("a".into())];
        let (prev, inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &second).unwrap();

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
            TOOLS_HASH,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![
            Message::user("different".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) =
            incremental_for_state(&mut state, TOOLS_HASH, AUTH_SCOPE_HASH, &second).unwrap();

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
            TOOLS_HASH,
            AUTH_SCOPE_HASH,
            &first,
        )
        .unwrap();

        let second = vec![Message::user("again".into())];
        record_in_state(&mut state, None, TOOLS_HASH, AUTH_SCOPE_HASH, &second).unwrap();

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
    fn ephemeral_preflight_failure_rebuilds_second_turn_with_full_history() {
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
            let mut provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let storage = StateDir::from_path(temp_dir.path().to_path_buf());
            provider.storage = Some(storage.clone());
            provider.response_state_storage = Some(storage);
            let session_id = SessionRef::generate();
            let model = Model::from_spec("openai/gpt-5.3-codex").unwrap();
            let tools = serde_json::json!([]);
            let (event_tx, _event_rx) = flume::unbounded();
            let first_messages = vec![Message::user("hello".into())];

            provider
                .stream_message(
                    &model,
                    &first_messages,
                    "",
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
            provider
                .stream_message(
                    &model,
                    &second_messages,
                    "",
                    &tools,
                    &event_tx,
                    RequestOptions::default(),
                    Some(&session_id),
                )
                .await
                .unwrap();
            server.await;

            let first_body = body_rx.recv_async().await.unwrap();
            let second_body = body_rx.recv_async().await.unwrap();
            assert!(first_body.get("previous_response_id").is_none());
            assert_eq!(first_body["store"], false);
            assert!(second_body.get("previous_response_id").is_none());
            assert_eq!(second_body["store"], false);
            assert_eq!(second_body["input"].as_array().unwrap().len(), 3);

            let sessions_dir = temp_dir.path().join(n00n_storage::sessions::SESSIONS_DIR);
            let session_prefix = session_id.id().to_string();
            assert!(std::fs::read_dir(sessions_dir).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&session_prefix)
            }));
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
                    TOOLS_HASH,
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
                    TOOLS_HASH,
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
                    TOOLS_HASH,
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
                    TOOLS_HASH,
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
                    TOOLS_HASH,
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
                    TOOLS_HASH,
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
                .parse::<N00nId>()
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
    fn coding_plan_uses_socket_local_state_while_api_keys_store_responses() {
        let api_key = ResolvedAuth {
            base_url: Some(CONFIG.base_url.into()),
            headers: Vec::new(),
        };
        let coding_plan = ResolvedAuth {
            base_url: Some(auth::CODING_PLAN_BASE_URL.into()),
            headers: Vec::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let provider = provider_with_response_storage(temp_dir.path());
        assert!(provider.stores_responses(&api_key));
        assert!(!provider.stores_responses(&coding_plan));

        let external = OpenAi::with_auth(
            Arc::new(Mutex::new(api_key.clone())),
            crate::providers::Timeouts::default(),
        )
        .unwrap();
        assert!(!external.stores_responses(&api_key));
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
            TOOLS_HASH,
            "account-1",
            &first,
        )
        .unwrap();
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental) =
            incremental_for_state(&mut state, TOOLS_HASH, "account-2", &second).unwrap();

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
    fn response_operation_slot_is_reused_while_request_is_live() {
        let temp_dir = TempDir::new().unwrap();
        let provider = provider_with_response_storage(temp_dir.path());
        let session_id = SessionRef::generate();
        let first = provider.response_operation_slot(Some(&session_id)).unwrap();
        let second = provider.response_operation_slot(Some(&session_id)).unwrap();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn response_state_uses_canonical_session_identity() {
        let temp_dir = TempDir::new().unwrap();
        let provider = provider_with_response_storage(temp_dir.path());
        let legacy: SessionRef = LEGACY_SESSION_ID.parse().unwrap();
        let canonical = SessionRef::from_id(legacy.id());

        assert_ne!(legacy.as_str(), canonical.as_str());
        assert_eq!(canonical_prompt_cache_key(&legacy), canonical.as_str());
        assert_ne!(
            canonical_prompt_cache_key(&legacy),
            canonical_prompt_cache_key(&SessionRef::generate())
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
            false,
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
            CodexAttempt::from_websocket_error(
                None,
                false,
                super::super::websocket::WebSocketAttemptError {
                    error: AgentError::Api {
                        status: 401,
                        message: "expired token".into(),
                    },
                    emitted_event,
                    transport_failure,
                    delivery,
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

            let attempt = provider
                .run_codex_attempt(
                    &model,
                    &[Message::user("hello".into())],
                    "",
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
    #[allow(clippy::result_large_err)]
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
            let provider = OpenAi::with_auth(
                Arc::new(Mutex::new(auth)),
                crate::providers::Timeouts {
                    connect: Duration::from_secs(2),
                    stream: Duration::from_secs(2),
                    low_speed: Duration::from_secs(2),
                },
            )
            .unwrap();
            let model = Model::from_spec("openai/gpt-5.3-codex").unwrap();
            let messages = [Message::user("hello".into())];
            let tools = serde_json::json!([]);
            let (first_tx, _) = flume::unbounded();
            let (second_tx, _) = flume::unbounded();
            let first_session = SessionRef::generate();
            let second_session = SessionRef::generate();

            let first = provider.stream_message(
                &model,
                &messages,
                "",
                &tools,
                &first_tx,
                RequestOptions::default(),
                Some(&first_session),
            );
            let second = provider.stream_message(
                &model,
                &messages,
                "",
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
    fn only_not_sent_missing_previous_rejections_allow_full_history_retry() {
        let attempt =
            |phase, status, message: &str, emitted_event, definitive_rejection| CodexAttempt {
                previous_response_id: Some("resp_1".into()),
                store: false,
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
            assert!(!is_missing_previous_response(&attempt(
                RequestDeliveryPhase::SentAwaitingAcceptance,
                status,
                message,
                false,
                true,
            )));
        }
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
    fn coding_plan_admission_retries_only_once_before_response_create() {
        let attempt = |phase, emitted_event| CodexAttempt {
            previous_response_id: Some("resp_1".into()),
            store: false,
            emitted_event,
            definitive_rejection: true,
            delivery: Some(RequestDeliveryMetadata::new(phase)),
            result: Err(AgentError::CodingPlanAdmission {
                retry_after: Some(Duration::from_secs(7)),
            }),
        };

        assert_eq!(
            coding_plan_admission_retry_delay(&attempt(RequestDeliveryPhase::NotSent, false)),
            Some(Duration::from_secs(7))
        );
        assert!(
            coding_plan_admission_retry_delay(&attempt(
                RequestDeliveryPhase::SentAwaitingAcceptance,
                false,
            ))
            .is_none()
        );
        assert!(
            coding_plan_admission_retry_delay(&attempt(RequestDeliveryPhase::Accepted, false))
                .is_none()
        );
        assert!(
            coding_plan_admission_retry_delay(&attempt(RequestDeliveryPhase::NotSent, true))
                .is_none()
        );
    }

    #[test]
    fn successful_socket_local_continuation_keeps_response_chain() {
        let success: Result<(), AgentError> = Ok(());
        assert!(!should_clear_response_chain(&success, false));

        let transport_error: Result<(), AgentError> =
            Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into());
        assert!(should_clear_response_chain(&transport_error, false));
        assert!(!should_clear_response_chain(&transport_error, true));

        let transient_api_error: Result<(), AgentError> = Err(AgentError::Api {
            status: 500,
            message: "temporary".into(),
        });
        assert!(!should_clear_response_chain(&transient_api_error, true));

        let permanent_api_error: Result<(), AgentError> = Err(AgentError::Api {
            status: 400,
            message: "invalid request".into(),
        });
        assert!(should_clear_response_chain(&permanent_api_error, true));
    }

    #[test]
    fn http_fallback_requires_transport_failure_before_output() {
        let transport = super::super::websocket::WebSocketAttemptError {
            error: std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
            emitted_event: false,
            transport_failure: true,
            delivery: crate::RequestDeliveryMetadata::new(crate::RequestDeliveryPhase::NotSent),
        };
        assert!(should_fallback_to_http(&transport));

        let after_output = super::super::websocket::WebSocketAttemptError {
            emitted_event: true,
            delivery: crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::SentAwaitingAcceptance,
            ),
            ..transport
        };
        assert!(!should_fallback_to_http(&after_output));

        let auth = super::super::websocket::WebSocketAttemptError {
            error: AgentError::Api {
                status: 401,
                message: "expired".into(),
            },
            emitted_event: false,
            transport_failure: true,
            delivery: crate::RequestDeliveryMetadata::new(crate::RequestDeliveryPhase::NotSent),
        };
        assert!(!should_fallback_to_http(&auth));

        let admission = super::super::websocket::WebSocketAttemptError {
            error: AgentError::CodingPlanAdmission { retry_after: None },
            emitted_event: false,
            transport_failure: true,
            delivery: crate::RequestDeliveryMetadata::new(crate::RequestDeliveryPhase::NotSent),
        };
        assert!(!should_fallback_to_http(&admission));

        let response_error = super::super::websocket::WebSocketAttemptError {
            error: AgentError::Api {
                status: 500,
                message: "server".into(),
            },
            emitted_event: false,
            transport_failure: false,
            delivery: crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::SentAwaitingAcceptance,
            ),
        };
        assert!(!should_fallback_to_http(&response_error));

        let after_send = super::super::websocket::WebSocketAttemptError {
            error: std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
            emitted_event: false,
            transport_failure: true,
            delivery: crate::RequestDeliveryMetadata::new(
                crate::RequestDeliveryPhase::SentAwaitingAcceptance,
            ),
        };
        assert!(!should_fallback_to_http(&after_send));
    }

    #[test_case(400)]
    #[test_case(401)]
    #[test_case(429)]
    #[test_case(500)]
    fn provider_status_after_http_send_becomes_non_retryable(status: u16) {
        let error = suppress_retry_after_send(AgentError::Api {
            status,
            message: "provider rejected an already-written request".into(),
        });

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
        assert!(!error.is_retryable());
        assert!(!error.is_auth_error());
    }
}

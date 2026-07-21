use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use flume::Sender;
use n00n_storage::StateDir;
use n00n_storage::id::{N00nId, SessionRef};
use n00n_storage::now_epoch;
use n00n_storage::sessions::{
    OPENAI_RESPONSE_CHAIN_TTL_SECONDS, OpenAiResponseChainLock, StoredOpenAiResponseChain,
    delete_openai_response_chain, load_openai_response_chain, lock_openai_response_chain,
    openai_response_chain_parent_exists, save_openai_response_chain,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::model::Model;
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, dialect};

use super::auth;
use crate::providers::ResolvedAuth;
use crate::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
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
const OPENAI_AUTH_DIR: &str = "auth";
const OPENAI_AUTH_LOCK_FILE: &str = "openai.refresh.lock";

type ResponseOperationSlot = Arc<AsyncMutex<()>>;

struct ScopedResponsesWebSocket {
    socket: super::websocket::ResponsesWebSocket,
    credential_hash: String,
    auth_generation: u64,
}

type ResponseConnectionSlot = Arc<AsyncMutex<Option<ScopedResponsesWebSocket>>>;

struct CodexAttempt {
    previous_response_id: Option<String>,
    store: bool,
    emitted_event: bool,
    result: Result<StreamResponse, AgentError>,
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
        && !error.request_sent
        && !error.emitted_event
        && !error.error.is_auth_error()
}

fn suppress_retry_after_send(error: AgentError) -> AgentError {
    if error.is_retryable() {
        AgentError::RequestSent {
            message: error.to_string(),
        }
    } else {
        error
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

fn lock_openai_auth(storage: &StateDir) -> Result<File, AgentError> {
    let auth_dir = storage.ensure_subdir(OPENAI_AUTH_DIR)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(auth_dir.join(OPENAI_AUTH_LOCK_FILE))?;
    file.lock()?;
    Ok(file)
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
    storage: Option<StateDir>,
    response_state_storage: Option<StateDir>,
    websocket_connect_timeout: Duration,
    system_prefix: Option<String>,
    session_state: Arc<Mutex<HashMap<N00nId, OpenAiSessionState>>>,
    response_connections: Arc<Mutex<HashMap<N00nId, ResponseConnectionSlot>>>,
    response_operations: Arc<Mutex<HashMap<N00nId, Weak<AsyncMutex<()>>>>>,
}

impl OpenAi {
    pub fn new(timeouts: crate::providers::Timeouts) -> Result<Self, AgentError> {
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
            storage: Some(storage.clone()),
            response_state_storage: Some(storage),
            websocket_connect_timeout: timeouts.connect,
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
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            auth_refresh: AsyncMutex::new(()),
            auth_generation: AtomicU64::new(0),
            storage: None,
            response_state_storage: None,
            websocket_connect_timeout: timeouts.connect,
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
        self.auth.lock().unwrap().clone()
    }

    fn is_oauth(&self) -> bool {
        self.storage.as_ref().is_some_and(auth::is_oauth)
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
        smol::unblock(move || {
            let lock = lock_openai_response_chain(&storage, session_id)?;
            if !openai_response_chain_parent_exists(&storage, session_id)? {
                return Err(n00n_storage::StorageError::NotFound(session_id.to_string()).into());
            }
            Ok(Some(lock))
        })
        .await
    }

    async fn refresh_oauth_locked(&self) -> Result<(), AgentError> {
        let storage = self.storage.clone().ok_or_else(|| AgentError::Config {
            message: "OAuth refresh not available for externally-managed auth".into(),
        })?;
        let resolved = smol::unblock(move || {
            let _auth_file_lock = lock_openai_auth(&storage)?;
            let tokens =
                n00n_storage::auth::load_tokens(&storage, auth::PROVIDER).ok_or_else(|| {
                    AgentError::Api {
                        status: 401,
                        message: "OpenAI OAuth tokens not found on disk".into(),
                    }
                })?;
            match auth::refresh_tokens(&tokens) {
                Ok(fresh) => {
                    n00n_storage::auth::save_tokens(&storage, auth::PROVIDER, &fresh)?;
                    Ok(auth::build_oauth_resolved(&fresh))
                }
                Err(e) => {
                    warn!(error = %e, "OpenAI OAuth refresh failed, clearing stale tokens");
                    let _ = n00n_storage::auth::delete_tokens(&storage, auth::PROVIDER);
                    Err(e)
                }
            }
        })
        .await?;
        *self.auth.lock().unwrap() = resolved;
        self.auth_generation.fetch_add(1, Ordering::Release);
        debug!("refreshed OpenAI OAuth token");
        Ok(())
    }

    async fn with_oauth_retry<T, F, Fut>(&self, f: F) -> Result<T, AgentError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, AgentError>>,
    {
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        let result = f().await;
        if self.is_oauth() && matches!(&result, Err(e) if e.is_auth_error()) {
            let refresh_guard = self.auth_refresh.lock().await;
            if self.auth_generation.load(Ordering::Acquire) == auth_generation
                && self.refresh_oauth_locked().await.is_err()
            {
                return result;
            }
            drop(refresh_guard);
            return f().await;
        }
        result
    }

    fn codex_auth(&self) -> Result<ResolvedAuth, AgentError> {
        // Prefer OAuth tokens for the ChatGPT Coding Plan backend.
        if let Some(storage) = self.storage.as_ref()
            && let Some(tokens) = n00n_storage::auth::load_tokens(storage, auth::PROVIDER)
        {
            return Ok(auth::build_coding_plan_resolved(&tokens));
        }
        // Fall back to standard API key via the Responses API.
        let mut auth = self.current_auth();
        if auth.base_url.is_none() {
            auth.base_url = Some(CONFIG.base_url.into());
        }
        Ok(auth)
    }

    fn stores_responses(&self, auth: &ResolvedAuth) -> bool {
        self.storage.is_some()
            && self.response_state_storage.is_some()
            && auth.base_url.as_deref() == Some(CONFIG.base_url)
    }

    async fn stream_websocket(
        &self,
        slot: Option<ResponseConnectionSlot>,
        body: &Value,
        event_tx: &Sender<ProviderEvent>,
        auth: &ResolvedAuth,
        credential_hash: &str,
        stream_timeout: Duration,
    ) -> Result<(Option<String>, StreamResponse), super::websocket::WebSocketAttemptError> {
        let Some(slot) = slot else {
            return super::websocket::stream_message(
                body,
                event_tx,
                auth,
                self.websocket_connect_timeout,
                stream_timeout,
            )
            .await;
        };

        let mut connection = slot.lock().await;
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        if connection.as_ref().is_some_and(|connection| {
            connection.socket.is_expired()
                || connection.credential_hash != credential_hash
                || connection.auth_generation != auth_generation
        }) {
            *connection = None;
        }
        if connection.is_none() {
            let socket =
                super::websocket::ResponsesWebSocket::connect(auth, self.websocket_connect_timeout)
                    .await
                    .map_err(|error| {
                        super::websocket::WebSocketAttemptError::transport(error, false, false)
                    })?;
            *connection = Some(ScopedResponsesWebSocket {
                socket,
                credential_hash: credential_hash.to_owned(),
                auth_generation,
            });
        }
        let result = connection
            .as_mut()
            .ok_or_else(|| AgentError::Config {
                message: "OpenAI WebSocket connection was not initialized".into(),
            })
            .map_err(|error| {
                super::websocket::WebSocketAttemptError::transport(error, false, false)
            })?
            .socket
            .stream_message(body, event_tx, stream_timeout)
            .await;
        if result.is_err() {
            *connection = None;
        }
        result
    }

    async fn prepare_request<'a>(
        &self,
        session_id: Option<&SessionRef>,
        tools_hash: &str,
        auth_scope_hash: &str,
        messages: &'a [Message],
        load_durable: bool,
    ) -> Result<(Option<String>, &'a [Message]), AgentError> {
        let Some(session_id) = session_id else {
            return Ok((None, messages));
        };
        let session_id = canonical_session_key(session_id);

        let needs_load = {
            let mut states = self.session_state.lock().unwrap();
            let now = Instant::now();
            let now_epoch = now_epoch();
            states.retain(|_, state| {
                now.duration_since(state.last_used) < SESSION_STATE_TTL
                    && (state.last_response_id.is_none() || state.expires_at > now_epoch)
            });
            self.response_connections
                .lock()
                .unwrap()
                .retain(|session_id, _| states.contains_key(session_id));
            !states.contains_key(&session_id)
        };

        if needs_load {
            let loaded = if load_durable && let Some(storage) = self.response_state_storage.clone()
            {
                match smol::unblock(move || load_openai_response_chain(&storage, session_id)).await
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
                "loaded durable OpenAI response chain state"
            );
            self.session_state
                .lock()
                .unwrap()
                .entry(session_id)
                .or_insert_with(|| loaded.unwrap_or_default());
        }

        let mut states = self.session_state.lock().unwrap();
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
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let session_id = canonical_session_key(session_id);
        let stored = {
            let mut states = self.session_state.lock().unwrap();
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
        let result = smol::unblock(move || match (persist, stored) {
            (true, Some(stored)) => save_openai_response_chain(&storage, session_id, &stored),
            (false, _) | (true, None) => delete_openai_response_chain(&storage, session_id),
        })
        .await;
        if let Err(error) = result {
            if matches!(&error, n00n_storage::StorageError::NotFound(_)) {
                self.clear_local_response_chain(session_id);
            } else {
                warn!(error = %error, "failed to persist OpenAI response chain; keeping in-memory state");
            }
        }
    }

    fn clear_local_response_chain(&self, session_id: N00nId) {
        self.session_state.lock().unwrap().remove(&session_id);
        self.response_connections
            .lock()
            .unwrap()
            .remove(&session_id);
    }

    async fn clear_response_chain(&self, session_id: Option<&SessionRef>) {
        let Some(session_id) = session_id else {
            return;
        };
        let session_id = canonical_session_key(session_id);
        self.session_state
            .lock()
            .unwrap()
            .insert(session_id, OpenAiSessionState::default());
        self.response_connections
            .lock()
            .unwrap()
            .remove(&session_id);
        let Some(storage) = self.response_state_storage.clone() else {
            return;
        };
        if let Err(error) =
            smol::unblock(move || delete_openai_response_chain(&storage, session_id)).await
        {
            warn!(error = %error, "failed to clear stale OpenAI response chain from disk");
        }
    }

    #[allow(clippy::too_many_arguments)]
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
    ) -> CodexAttempt {
        let auth = match self.codex_auth() {
            Ok(auth) => auth,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    store: false,
                    emitted_event: false,
                    result: Err(error),
                };
            }
        };
        let state_scope_hash = response_state_scope_hash(&auth);
        let socket_credential_hash = credential_hash(&auth);
        let store = self.stores_responses(&auth);
        if !store
            && !self
                .response_connection_is_reusable(session_id, &socket_credential_hash)
                .await
        {
            debug!(
                chain_reset = true,
                chain_reset_reason = "socket_not_reusable",
                "resetting connection-local OpenAI response chain"
            );
            self.clear_response_chain(session_id).await;
        }
        let (previous_response_id, incremental_messages) = match self
            .prepare_request(session_id, tools_hash, &state_scope_hash, messages, store)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return CodexAttempt {
                    previous_response_id: None,
                    store,
                    emitted_event: false,
                    result: Err(error),
                };
            }
        };
        let prompt_cache_key = session_id.map(canonical_prompt_cache_key);
        let stream_timeout = self.compat.stream_timeout();
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
        log_responses_request(
            "websocket",
            &body,
            messages.len(),
            incremental_messages.len(),
            previous_response_id.is_some(),
            false,
        );
        let connection_slot = self.response_connection_slot(session_id);
        let websocket_result = self
            .stream_websocket(
                connection_slot,
                &body,
                event_tx,
                &auth,
                &socket_credential_hash,
                stream_timeout,
            )
            .await;
        let (response_id, response, chainable) = match websocket_result {
            Ok((response_id, response)) => (response_id, response, true),
            Err(error) if should_fallback_to_http(&error) => {
                warn!("OpenAI Responses WebSocket unavailable; falling back to HTTP");
                let fallback_body = if store {
                    body
                } else {
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
                };
                log_responses_request(
                    "http_sse",
                    &fallback_body,
                    messages.len(),
                    if store {
                        incremental_messages.len()
                    } else {
                        messages.len()
                    },
                    store && previous_response_id.is_some(),
                    !store,
                );
                match super::responses::do_stream(
                    self.compat.client(),
                    model,
                    &fallback_body,
                    event_tx,
                    &auth,
                    stream_timeout,
                )
                .await
                {
                    Ok((response_id, response)) => (response_id, response, store),
                    Err(error) => {
                        return CodexAttempt {
                            previous_response_id,
                            store,
                            emitted_event: true,
                            result: Err(suppress_retry_after_send(error)),
                        };
                    }
                }
            }
            Err(error) => {
                let emitted_event = error.emitted_event;
                let provider_error = if error.request_sent {
                    suppress_retry_after_send(error.error)
                } else {
                    error.error
                };
                return CodexAttempt {
                    previous_response_id,
                    store,
                    emitted_event,
                    result: Err(provider_error),
                };
            }
        };
        self.record_response(
            session_id,
            chainable.then_some(response_id).flatten(),
            tools_hash,
            &state_scope_hash,
            messages,
            store,
        )
        .await;
        CodexAttempt {
            previous_response_id,
            store,
            emitted_event: false,
            result: Ok(response),
        }
    }

    #[allow(clippy::too_many_arguments)]
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
    ) -> CodexAttempt {
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        let attempt = self
            .run_codex_attempt(
                model, messages, system, tools, tools_hash, event_tx, opts, session_id,
            )
            .await;
        if attempt.emitted_event
            || !self.is_oauth()
            || !matches!(&attempt.result, Err(error) if error.is_auth_error())
        {
            return attempt;
        }

        let refresh_guard = self.auth_refresh.lock().await;
        if self.auth_generation.load(Ordering::Acquire) == auth_generation
            && self.refresh_oauth_locked().await.is_err()
        {
            return attempt;
        }
        drop(refresh_guard);
        self.run_codex_attempt(
            model, messages, system, tools, tools_hash, event_tx, opts, session_id,
        )
        .await
    }

    fn response_connection_slot(
        &self,
        session_id: Option<&SessionRef>,
    ) -> Option<ResponseConnectionSlot> {
        let session_id = session_id?;
        let session_id = canonical_session_key(session_id);
        let mut connections = self.response_connections.lock().unwrap();
        let slot = connections
            .entry(session_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)));
        Some(Arc::clone(slot))
    }

    async fn response_connection_is_reusable(
        &self,
        session_id: Option<&SessionRef>,
        credential_hash: &str,
    ) -> bool {
        let Some(session_id) = session_id else {
            return false;
        };
        let session_id = canonical_session_key(session_id);
        let slot = self
            .response_connections
            .lock()
            .unwrap()
            .get(&session_id)
            .map(Arc::clone);
        let Some(slot) = slot else {
            return false;
        };
        let connection = slot.lock().await;
        let auth_generation = self.auth_generation.load(Ordering::Acquire);
        connection.as_ref().is_some_and(|connection| {
            !connection.socket.is_expired()
                && connection.credential_hash == credential_hash
                && connection.auth_generation == auth_generation
        })
    }

    fn response_operation_slot(
        &self,
        session_id: Option<&SessionRef>,
    ) -> Option<ResponseOperationSlot> {
        let session_id = session_id?;
        let session_id = canonical_session_key(session_id);
        let mut operations = self.response_operations.lock().unwrap();
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
                let _response_chain_lock = self.lock_response_chain(session_id).await?;
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
                    )
                    .await;
                if attempt.previous_response_id.is_none() {
                    return attempt.result;
                }
                if !is_missing_previous_response(&attempt.result) {
                    if should_clear_response_chain(&attempt.result, attempt.store) {
                        self.clear_response_chain(session_id).await;
                    }
                    return attempt.result;
                }

                warn!(
                    chain_reset = true,
                    full_history_fallback = true,
                    "OpenAI Responses chain was not found; retrying with full history"
                );
                self.clear_response_chain(session_id).await;
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
            if self.is_oauth() {
                let _refresh_guard = self.auth_refresh.lock().await;
                self.refresh_oauth_locked().await
            } else {
                Ok(())
            }
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
            *self.auth.lock().unwrap() = resolved;
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

fn is_missing_previous_response<T>(result: &Result<T, AgentError>) -> bool {
    matches!(result, Err(AgentError::Api { status: 400, message }) if {
        let normalized = message.to_ascii_lowercase();
        normalized.starts_with("previous_response_not_found:")
            || normalized.contains("previous response") && normalized.contains("not found")
    })
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

    use tempfile::TempDir;
    use test_case::test_case;

    use super::*;
    use crate::{ContentBlock, Role, TokenUsage};

    const TOOLS_HASH: &str = "[]";
    const AUTH_SCOPE_HASH: &str = "account";
    const LEGACY_SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";

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
            provider
                .record_response(
                    Some(&session_id),
                    Some("resp_1".into()),
                    TOOLS_HASH,
                    AUTH_SCOPE_HASH,
                    &first,
                    true,
                )
                .await;
            drop(provider);

            let restored = provider_with_response_storage(temp_dir.path());
            let second = vec![
                Message::user("hello".into()),
                assistant("hi"),
                Message::user("again".into()),
            ];
            let (previous_response_id, incremental) = restored
                .prepare_request(
                    Some(&session_id),
                    TOOLS_HASH,
                    AUTH_SCOPE_HASH,
                    &second,
                    true,
                )
                .await
                .unwrap();

            assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
            assert_eq!(incremental.len(), 1);
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
            request_sent: false,
        };
        assert!(should_fallback_to_http(&transport));

        let after_output = super::super::websocket::WebSocketAttemptError {
            emitted_event: true,
            request_sent: true,
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
            request_sent: false,
        };
        assert!(!should_fallback_to_http(&auth));

        let response_error = super::super::websocket::WebSocketAttemptError {
            error: AgentError::Api {
                status: 500,
                message: "server".into(),
            },
            emitted_event: false,
            transport_failure: false,
            request_sent: true,
        };
        assert!(!should_fallback_to_http(&response_error));

        let after_send = super::super::websocket::WebSocketAttemptError {
            error: std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
            emitted_event: false,
            transport_failure: true,
            request_sent: true,
        };
        assert!(!should_fallback_to_http(&after_send));
    }

    #[test]
    fn retryable_error_after_send_becomes_non_retryable() {
        let error = suppress_retry_after_send(
            std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed").into(),
        );

        assert!(matches!(error, AgentError::RequestSent { .. }));
        assert!(!error.is_retryable());
    }
}

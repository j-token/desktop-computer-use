//! One desktop session (indicator, stop key, input lease) is shared by every
//! agent session that joined it. Agents run their operations at the same time:
//! observations and headless actions proceed in parallel, while physical input
//! takes turns because there is only one pointer and one keyboard. A backend
//! that cannot run calls concurrently is still called one at a time.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use serde_json::json;

use crate::backend::{Backend, Context, Error, Json};
use crate::input_lease::InputLease;
use crate::validation::{
    bool_param, canonical_toggle_key, is_input_action, is_semantic_action, string_param,
    validate_headless_input, validate_params,
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const MAXIMUM_AGENT_SESSIONS: usize = 16;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(45);

struct AgentSession {
    agent_id: String,
    multi_agent: bool,
    headless: bool,
    busy: bool,
    last_activity: Instant,
}

struct RunningOperation {
    session_id: String,
    context: Context,
    /// `None` while the operation waits for its turn.
    started: Option<Instant>,
}

/// Owned by whoever holds the desktop lock exclusively, which is only the
/// session start and the cleanup after a stop.
#[derive(Default)]
struct DesktopSession {
    id: String,
    lease: InputLease,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<String, AgentSession>,
    operations: HashMap<u64, RunningOperation>,
    next_agent_number: u64,
    stopping: bool,
    /// Empty whenever no session is active.
    toggles: BTreeSet<String>,
    /// Session that holds `toggles`, empty once its session ended.
    toggle_owner: String,
}

enum DesktopGuard<'a> {
    Shared(RwLockReadGuard<'a, DesktopSession>),
    Exclusive(RwLockWriteGuard<'a, DesktopSession>),
}

impl DesktopGuard<'_> {
    fn session_id(&self) -> &str {
        match self {
            Self::Shared(desktop) => &desktop.id,
            Self::Exclusive(desktop) => &desktop.id,
        }
    }
}

pub struct Dispatcher {
    backend: Arc<dyn Backend>,
    token: String,
    // Lock order: desktop, physical_input, backend_turn, state. The desktop
    // lock is exclusive while the desktop session starts or stops and shared
    // by every other operation. Stopping takes none of the first three, so Esc
    // and stop never wait behind running input.
    desktop: RwLock<DesktopSession>,
    physical_input: Mutex<()>,
    backend_turn: Mutex<()>,
    state: Mutex<State>,
    /// Desktop-wide cancellation (Esc, stop, lost indicator). Each operation
    /// runs under its own context so stopping one agent leaves the others alone.
    context: Context,
    next_operation_id: AtomicU64,
    idle_timeout: Duration,
    queue_timeout: Duration,
    stop_requested: AtomicBool,
    orphaned_toggles_pending: AtomicBool,
    quitting: AtomicBool,
}

impl Dispatcher {
    pub fn new(backend: Arc<dyn Backend>, token: String) -> Self {
        Self::with_timeouts(backend, token, DEFAULT_IDLE_TIMEOUT, DEFAULT_QUEUE_TIMEOUT)
    }

    pub fn with_timeouts(
        backend: Arc<dyn Backend>,
        token: String,
        idle_timeout: Duration,
        queue_timeout: Duration,
    ) -> Self {
        Self {
            backend,
            token,
            desktop: RwLock::default(),
            physical_input: Mutex::new(()),
            backend_turn: Mutex::new(()),
            state: Mutex::new(State {
                next_agent_number: 1,
                ..State::default()
            }),
            context: Context::new(),
            next_operation_id: AtomicU64::new(1),
            idle_timeout,
            queue_timeout,
            stop_requested: AtomicBool::new(false),
            orphaned_toggles_pending: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
        }
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    pub fn request_quit(&self) {
        self.quitting.store(true, Ordering::SeqCst);
    }

    pub fn handle(&self, request: &Json) -> Json {
        let request_id = request.get("id").cloned().unwrap_or(Json::Null);
        let mut authenticated = false;
        let mut requester = String::new();
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            authenticate_request(request, &request_id, &self.token)?;
            authenticated = true;
            let method = request
                .get("method")
                .and_then(Json::as_str)
                .ok_or_else(|| Error::invalid_argument("method must be a string"))?;
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            if params.is_object() {
                requester = string_param(&params, "sessionId");
            }
            validate_params(method, &params)?;
            let mut result = self.route(method, params, &requester)?;
            if let Some(result) = result.as_object_mut() {
                let details = self.toggle_details(&requester);
                result.insert("toggles".into(), details.toggles);
                if let Some(notice) = details.notice {
                    let notice = match result.get("notice").and_then(Json::as_str) {
                        Some(existing) => format!("{existing} {notice}"),
                        None => notice,
                    };
                    result.insert("notice".into(), notice.into());
                }
            }
            Ok(result)
        }));
        let error = match outcome {
            Ok(Ok(result)) => return json!({"id": request_id, "ok": true, "result": result}),
            Ok(Err(error)) => error,
            Err(panic) => Error::new("provider_error", panic_message(panic.as_ref())),
        };
        let mut body = json!({"code": error.code, "message": error.message});
        // Unauthenticated callers learn nothing about the desktop state.
        if authenticated {
            let details = self.toggle_details(&requester);
            if let Some(notice) = details.notice {
                body["details"] = json!({"toggles": details.toggles, "notice": notice});
            }
        }
        json!({"id": request_id, "ok": false, "error": body})
    }

    pub fn tick(&self) {
        let mut stop_all;
        {
            let mut state = self.state.lock();
            let now = Instant::now();
            let expired: Vec<String> = state
                .sessions
                .iter()
                .filter(|(_, session)| {
                    !session.busy
                        && now.saturating_duration_since(session.last_activity) > self.idle_timeout
                })
                .map(|(id, _)| id.clone())
                .collect();
            stop_all = !state.sessions.is_empty()
                && (self.context.is_cancelled() || expired.len() == state.sessions.len());
            if !stop_all {
                for id in expired {
                    state.sessions.remove(&id);
                    if id != state.toggle_owner {
                        continue;
                    }
                    state.toggle_owner.clear();
                    if !state.toggles.is_empty() {
                        self.orphaned_toggles_pending.store(true, Ordering::SeqCst);
                    }
                }
            }
            let operation_expired = state.operations.values().any(|operation| {
                operation.started.is_some_and(|started| {
                    now.saturating_duration_since(started) > OPERATION_TIMEOUT
                })
            });
            if operation_expired && !self.context.is_cancelled() {
                stop_all = true;
            }
        }
        if stop_all {
            self.stop();
        }

        if !self.cleanup_pending() {
            return;
        }
        let Some(mut desktop) = self.desktop.try_write() else {
            return;
        };
        self.finish_pending_stop(&mut desktop);
        self.release_orphaned_toggles(&mut desktop);
    }

    pub fn shutdown(&self) {
        self.stop();
        self.tick();
    }

    fn route(&self, method: &str, params: Json, requester: &str) -> Result<Json, Error> {
        match method {
            "session.stop" | "daemon.shutdown" => {
                let agent_stop = if method == "session.stop" && !requester.is_empty() {
                    self.stop_agent_session(requester)?
                } else {
                    None
                };
                if let Some(agent_stop) = agent_stop {
                    return Ok(agent_stop);
                }
                {
                    // A cancelled session (Esc, lost indicator) is already
                    // ending and releasing its toggles; let the stop through.
                    let state = self.state.lock();
                    if !state.toggles.is_empty() && !self.context.is_cancelled() {
                        return Err(toggles_still_on(&state.toggles));
                    }
                }
                let stopped = self.stop();
                if method == "daemon.shutdown" {
                    self.request_quit();
                }
                Ok(stopped)
            }
            "session.status" => Ok(self.session_status(requester)),
            "toggle.status" => Ok(json!({"active": !self.state.lock().sessions.is_empty()})),
            _ => self.execute_operation(method, params),
        }
    }

    fn stop(&self) -> Json {
        self.context.cancel();
        self.stop_requested.store(true, Ordering::SeqCst);
        {
            let mut state = self.state.lock();
            state.stopping = !state.sessions.is_empty();
            state.sessions.clear();
            // Every stop path (user Esc, idle expiry, operation timeout, client
            // disconnect, shutdown) ends the toggles; interrupt() and the
            // backend's session.stop release the physical keys.
            state.toggles.clear();
            state.toggle_owner.clear();
            for operation in state.operations.values() {
                operation.context.cancel();
            }
        }
        self.orphaned_toggles_pending.store(false, Ordering::SeqCst);
        self.backend.interrupt();
        json!({"stopped": true})
    }

    fn stop_agent_session(&self, session_id: &str) -> Result<Option<Json>, Error> {
        let mut state = self.state.lock();
        if !state.sessions.contains_key(session_id) {
            if state.sessions.is_empty() {
                return Ok(None);
            }
            return Err(Error::new(
                "session_required",
                "Unknown sessionId; omit sessionId to stop every session on this desktop",
            ));
        }
        // The last session, or a desktop that is already ending, takes the full stop path.
        if state.sessions.len() == 1 || self.context.is_cancelled() {
            return Ok(None);
        }
        if state.toggle_owner == session_id && !state.toggles.is_empty() {
            return Err(toggles_still_on(&state.toggles));
        }
        let session = state
            .sessions
            .remove(session_id)
            .expect("session checked above");
        for operation in state
            .operations
            .values()
            .filter(|operation| operation.session_id == session_id)
        {
            operation.context.cancel();
        }
        Ok(Some(
            json!({"stopped": true, "agentId": session.agent_id, "agentCount": state.sessions.len()}),
        ))
    }

    fn cleanup_pending(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
            || self.orphaned_toggles_pending.load(Ordering::SeqCst)
    }

    fn settle_pending_cleanup(&self) -> Result<(), Error> {
        if !self.cleanup_pending() {
            return Ok(());
        }
        let Some(mut desktop) = self.desktop.try_write_for(self.queue_timeout) else {
            return Err(Error::new(
                "busy",
                "The desktop session is still stopping; retry shortly",
            ));
        };
        self.finish_pending_stop(&mut desktop);
        self.release_orphaned_toggles(&mut desktop);
        Ok(())
    }

    /// Urgent stop never waits for this cleanup, and the input lease stays
    /// held until the backend's cleanup completes.
    fn finish_pending_stop(&self, desktop: &mut DesktopSession) {
        if !self.stop_requested.swap(false, Ordering::SeqCst) {
            return;
        }
        self.backend.release_toggles();
        // interrupt() already requested input release. Cleanup is best effort
        // when a provider has disconnected or its target window has closed.
        let _ = self.call_backend("session.stop", &json!({}), &self.context);
        desktop.lease.release();

        let mut state = self.state.lock();
        state.stopping = false;
        desktop.id.clear();
    }

    /// Taking the desktop exclusively proves no physical input is running.
    fn release_orphaned_toggles(&self, _exclusive: &mut DesktopSession) {
        if !self.orphaned_toggles_pending.swap(false, Ordering::SeqCst) {
            return;
        }
        self.backend.release_toggles();
        let mut state = self.state.lock();
        if state.toggle_owner.is_empty() {
            state.toggles.clear();
        }
    }

    fn call_backend(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        panic::catch_unwind(AssertUnwindSafe(|| {
            self.backend.execute(method, params, context)
        }))
        .unwrap_or_else(|panic| Err(Error::new("provider_error", panic_message(panic.as_ref()))))
    }

    fn append_observation(&self, result: &mut Json, mut params: Json, operation: &Context) {
        let observation_mode = params
            .get("observe")
            .and_then(Json::as_str)
            .unwrap_or("none")
            .to_owned();
        if observation_mode == "none" {
            return;
        }
        params["includeScreenshot"] = json!(observation_mode != "text");
        params["includeText"] = json!(observation_mode != "screenshot");
        if bool_param(&params, "headless") {
            params
                .as_object_mut()
                .expect("validated params")
                .remove("activate");
        }
        let Some(result) = result.as_object_mut() else {
            return;
        };
        match self.call_backend("get-app-state", &params, operation) {
            Ok(observation) => {
                result.insert("observation".into(), observation);
            }
            // A capture failure must not turn delivered input into a retryable failure.
            Err(error) => {
                result.insert("observationError".into(), json!({"message": error.message}));
            }
        }
    }

    fn start_session(&self, params: &Json, desktop: &mut DesktopSession) -> Result<Json, Error> {
        let multi_agent = bool_param(params, "multiAgent");
        let headless = bool_param(params, "headless");
        let mut agent_id = string_param(params, "agentId");
        let joining;
        {
            let mut state = self.state.lock();
            joining = !state.sessions.is_empty();
            if joining {
                let everyone_shares = state.sessions.values().all(|session| session.multi_agent);
                if !multi_agent || !everyone_shares {
                    return Err(Error::new(
                        "session_busy",
                        "An active session already owns this desktop; start every agent's session with \
                         multiAgent to share it",
                    ));
                }
                self.context.check()?;
                if state.sessions.len() >= MAXIMUM_AGENT_SESSIONS {
                    return Err(Error::new(
                        "session_busy",
                        format!("This desktop already has {MAXIMUM_AGENT_SESSIONS} agent sessions"),
                    ));
                }
            }
            let in_use = |state: &State, candidate: &str| {
                state
                    .sessions
                    .values()
                    .any(|session| session.agent_id == candidate)
            };
            if !agent_id.is_empty() && in_use(&state, &agent_id) {
                return Err(Error::new(
                    "agent_id_in_use",
                    format!(
                        "Agent {agent_id} already has a session on this desktop; pick another agentId"
                    ),
                ));
            }
            while agent_id.is_empty() || in_use(&state, &agent_id) {
                agent_id = format!("agent-{}", state.next_agent_number);
                state.next_agent_number += 1;
            }
            if !joining {
                desktop.lease.acquire()?;
                self.context.reset();
                state.toggles.clear();
                state.toggle_owner.clear();
                desktop.id = create_session_id();
            }
        }

        let mut result = json!({"ready": true});
        if !joining {
            let started = self
                .call_backend(
                    "session.start",
                    &json!({"sessionId": desktop.id}),
                    &self.context,
                )
                .and_then(|started| {
                    if !started
                        .get("ready")
                        .and_then(Json::as_bool)
                        .unwrap_or(false)
                    {
                        return Err(Error::new(
                            "setup_required",
                            "Provider did not confirm a ready usage indicator",
                        ));
                    }
                    self.context.check()?;
                    Ok(started)
                });
            match started {
                Ok(started) => result = started,
                Err(error) => {
                    self.stop();
                    return Err(error);
                }
            }
        }

        let session_id = create_session_id();
        let cancelled;
        {
            let mut state = self.state.lock();
            cancelled = self.context.is_cancelled();
            if !cancelled {
                state.sessions.insert(
                    session_id.clone(),
                    AgentSession {
                        agent_id: agent_id.clone(),
                        multi_agent,
                        headless,
                        busy: false,
                        last_activity: Instant::now(),
                    },
                );
                result["agentCount"] = json!(state.sessions.len());
            }
        }
        if cancelled {
            // Without a session nothing else would end the desktop session just started.
            if !joining {
                self.stop();
            }
            return Err(Error::cancelled());
        }
        result["sessionId"] = json!(session_id);
        result["agentId"] = json!(agent_id);
        result["multiAgent"] = json!(multi_agent);
        result["headless"] = json!(headless);
        result["joined"] = json!(joining);
        Ok(result)
    }

    fn execute_operation(&self, method: &str, mut params: Json) -> Result<Json, Error> {
        let starts_session = method == "session.start";
        let needs_session = !starts_session && method != "doctor" && method != "capabilities";
        let session_id = if needs_session {
            string_param(&params, "sessionId")
        } else {
            String::new()
        };

        // An agent runs one operation at a time; a second one would race its
        // own observation. Other agents are not affected by this flag.
        if needs_session {
            let mut state = self.state.lock();
            let session = state
                .sessions
                .get_mut(&session_id)
                .ok_or_else(session_required)?;
            if session.busy {
                return Err(Error::new(
                    "busy",
                    "Another operation is in progress; observe before retrying an input",
                ));
            }
            session.busy = true;
            session.last_activity = Instant::now();
        }
        let _busy_reset = BusyReset {
            dispatcher: self,
            session_id: &session_id,
        };

        self.settle_pending_cleanup()?;
        let desktop = if starts_session {
            self.desktop
                .try_write_for(self.queue_timeout)
                .map(DesktopGuard::Exclusive)
        } else {
            self.desktop
                .try_read_for(self.queue_timeout)
                .map(DesktopGuard::Shared)
        };
        let Some(mut desktop) = desktop else {
            return Err(Error::new(
                "busy",
                "The desktop session is starting or stopping; retry shortly",
            ));
        };
        if let DesktopGuard::Exclusive(desktop) = &mut desktop {
            self.finish_pending_stop(desktop);
            self.release_orphaned_toggles(desktop);
        }

        // Registering in the same critical section that validates the session
        // means a concurrent stop either fails this check or cancels the operation.
        let operation = Context::new();
        let operation_id = self.next_operation_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.state.lock();
            if needs_session {
                let session = state
                    .sessions
                    .get(&session_id)
                    .ok_or_else(session_required)?;
                self.context.check()?;
                let headless = params
                    .get("headless")
                    .and_then(Json::as_bool)
                    .unwrap_or(session.headless);
                params["headless"] = json!(headless);
            }
            state.operations.insert(
                operation_id,
                RunningOperation {
                    session_id: session_id.clone(),
                    context: operation.clone(),
                    started: None,
                },
            );
        }
        let _operation_reset = OperationReset {
            dispatcher: self,
            operation_id,
        };

        if needs_session && is_input_action(method) {
            validate_headless_input(method, &params)?;
        }

        let _physical_input = if needs_physical_input(method, &params) {
            Some(self.physical_input.try_lock_for(self.queue_timeout).ok_or_else(|| {
                Error::new("busy", "Another agent's physical input is still running; retry, or use a headless action")
            })?)
        } else {
            None
        };
        let _backend_turn = if self.backend.supports_concurrent_execute() {
            None
        } else {
            Some(self.backend_turn.try_lock_for(self.queue_timeout).ok_or_else(|| {
                Error::new(
                    "busy",
                    "Another agent's operation is still running; this platform runs operations one at a time",
                )
            })?)
        };
        operation.check()?;
        if needs_session {
            self.context.check()?;
            self.enforce_toggle_owner(method, &params, &session_id)?;
            params["agentSessionId"] = json!(session_id);
            params["sessionId"] = json!(desktop.session_id());
        }
        if let Some(running) = self.state.lock().operations.get_mut(&operation_id) {
            running.started = Some(Instant::now());
        }

        if let DesktopGuard::Exclusive(desktop) = &mut desktop {
            return self.start_session(&params, desktop);
        }
        let mut result = if method.starts_with("toggle.") {
            self.execute_toggle(method, &params, &session_id, &operation)?
        } else {
            self.call_backend(method, &params, &operation)?
        };
        if is_input_action(method) {
            operation.check()?;
            if let Some(result) = result.as_object_mut() {
                result
                    .entry("delivery")
                    .or_insert_with(|| "physical".into());
            }
            self.append_observation(&mut result, params, &operation);
        }
        if needs_session && let Some(result) = result.as_object_mut() {
            result.insert("sessionId".into(), session_id.clone().into());
        }
        Ok(result)
    }

    /// The caller owns the physical-input turn and has verified the session.
    fn execute_toggle(
        &self,
        method: &str,
        params: &Json,
        session_id: &str,
        operation: &Context,
    ) -> Result<Json, Error> {
        if method == "toggle.release-all" {
            self.backend.release_toggles();
            let mut state = self.state.lock();
            let released: Vec<String> = std::mem::take(&mut state.toggles).into_iter().collect();
            state.toggle_owner.clear();
            return Ok(json!({"released": released}));
        }
        let key = params
            .get("key")
            .and_then(Json::as_str)
            .and_then(canonical_toggle_key)
            .ok_or_else(|| {
                Error::invalid_argument("toggle key must be shift, ctrl, alt, win, or space")
            })?;
        let on = bool_param(params, "on");
        let held = self.state.lock().toggles.contains(key);
        if held == on {
            return Ok(json!({"key": key, "on": on, "changed": false}));
        }

        self.backend.set_toggle(key, on, operation)?;
        let kept = {
            let mut state = self.state.lock();
            if !on {
                state.toggles.remove(key);
                if state.toggles.is_empty() {
                    state.toggle_owner.clear();
                }
                true
            } else if state.sessions.contains_key(session_id) && !self.context.is_cancelled() {
                state.toggles.insert(key.to_owned());
                state.toggle_owner = session_id.to_owned();
                true
            } else {
                false
            }
        };
        if !kept {
            // A desktop stop owns the key through its release paths; a stop of
            // this agent alone leaves nobody else to release it.
            if !self.context.is_cancelled() {
                let _ = self.backend.set_toggle(key, false, &Context::new());
            }
            return Err(Error::cancelled());
        }
        Ok(json!({"key": key, "on": on, "changed": true}))
    }

    fn enforce_toggle_owner(
        &self,
        method: &str,
        params: &Json,
        session_id: &str,
    ) -> Result<(), Error> {
        let state = self.state.lock();
        if state.toggles.is_empty() || state.toggle_owner == session_id {
            return Ok(());
        }
        let holder = match state.sessions.get(&state.toggle_owner) {
            Some(owner) => format!("Agent {}", owner.agent_id),
            None => "An ended agent session".to_owned(),
        };
        let toggles = join_toggles(&state.toggles);
        if method.starts_with("toggle.") && method != "toggle.status" {
            return Err(Error::new(
                "toggles_active",
                format!("{holder} holds toggle {toggles}; only that agent can change toggles"),
            ));
        }
        if needs_physical_input(method, params) {
            return Err(Error::new(
                "toggles_active",
                format!(
                    "{holder} holds toggle {toggles}, which would join this physical input; retry with headless \
                     true or after that agent releases them"
                ),
            ));
        }
        Ok(())
    }

    fn session_status(&self, requester: &str) -> Json {
        let state = self.state.lock();
        let now = Instant::now();
        let agents: Vec<Json> = state
            .sessions
            .iter()
            .map(|(id, session)| {
                let idle_ms = now
                    .saturating_duration_since(session.last_activity)
                    .as_millis() as u64;
                json!({
                    "agentId": session.agent_id,
                    "multiAgent": session.multi_agent,
                    "headless": session.headless,
                    "busy": session.busy,
                    "idleMs": idle_ms,
                    "current": id == requester,
                })
            })
            .collect();
        // A session id is a credential, so only its owner or a lone
        // single-agent session (whose id the CLI saved anyway) sees it.
        let visible = if state.sessions.contains_key(requester) {
            Json::from(requester)
        } else {
            match state.sessions.iter().next() {
                Some((id, session)) if state.sessions.len() == 1 && !session.multi_agent => {
                    Json::from(id.as_str())
                }
                _ => Json::Null,
            }
        };
        json!({
            "active": !state.sessions.is_empty(),
            "stopping": state.stopping,
            "sessionId": visible,
            "idleTimeoutMs": self.idle_timeout.as_millis() as u64,
            "agentCount": state.sessions.len(),
            "agents": agents,
        })
    }

    fn toggle_details(&self, requester: &str) -> ToggleDetails {
        let state = self.state.lock();
        let toggles = Json::from(state.toggles.iter().cloned().collect::<Vec<_>>());
        if state.toggles.is_empty() {
            return ToggleDetails {
                toggles,
                notice: None,
            };
        }
        let joined = join_toggles(&state.toggles);
        let held_by_another_agent =
            state.toggle_owner != requester && state.sessions.contains_key(requester);
        let notice = match state.sessions.get(&state.toggle_owner) {
            Some(owner) if held_by_another_agent => format!(
                "Agent {} holds toggle {joined}; your physical input is refused until it releases them, headless \
                 actions still work.",
                owner.agent_id
            ),
            _ => format!(
                "Toggle still on: {joined}. Release with `dcu toggle off --all` before ending."
            ),
        };
        ToggleDetails {
            toggles,
            notice: Some(notice),
        }
    }
}

struct ToggleDetails {
    toggles: Json,
    notice: Option<String>,
}

struct BusyReset<'a> {
    dispatcher: &'a Dispatcher,
    session_id: &'a str,
}

impl Drop for BusyReset<'_> {
    fn drop(&mut self) {
        if self.session_id.is_empty() {
            return;
        }
        let mut state = self.dispatcher.state.lock();
        if let Some(session) = state.sessions.get_mut(self.session_id) {
            session.busy = false;
            session.last_activity = Instant::now();
        }
    }
}

struct OperationReset<'a> {
    dispatcher: &'a Dispatcher,
    operation_id: u64,
}

impl Drop for OperationReset<'_> {
    fn drop(&mut self) {
        self.dispatcher
            .state
            .lock()
            .operations
            .remove(&self.operation_id);
    }
}

/// Moving the pointer, pressing keys, or bringing a window to the front would
/// interleave with another agent's physical input, so these take turns.
fn needs_physical_input(method: &str, params: &Json) -> bool {
    if method.starts_with("toggle.") && method != "toggle.status" {
        return true;
    }
    if is_semantic_action(method) {
        return false;
    }
    if is_input_action(method) {
        return !bool_param(params, "headless");
    }
    method == "get-app-state" && bool_param(params, "activate")
}

fn authenticate_request(request: &Json, request_id: &Json, token: &str) -> Result<(), Error> {
    let has_valid_id = request_id.as_str().is_some_and(|id| id.len() <= 256);
    let supplied = request.get("token").and_then(Json::as_str);
    match supplied {
        Some(supplied) if request.is_object() && has_valid_id && tokens_match(supplied, token) => {
            Ok(())
        }
        _ => Err(Error::new("unauthorized", "Invalid request credentials")),
    }
}

fn tokens_match(supplied: &str, expected: &str) -> bool {
    if supplied.len() != expected.len() {
        return false;
    }
    let difference = supplied
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b));
    difference == 0
}

fn create_session_id() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system random source");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn join_toggles(toggles: &BTreeSet<String>) -> String {
    toggles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn toggles_still_on(toggles: &BTreeSet<String>) -> Error {
    Error::new(
        "toggles_active",
        format!(
            "Toggle still on: {}. Run `dcu toggle off --all` before stopping the session.",
            join_toggles(toggles)
        ),
    )
}

fn session_required() -> Error {
    Error::new(
        "session_required",
        "Start a session and provide its sessionId",
    )
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    "Backend failed unexpectedly".to_owned()
}

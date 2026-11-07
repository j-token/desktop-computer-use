//! Protocol contract of the dispatcher.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use dcu_core::{Backend, Context, Dispatcher, Error, Json};
use parking_lot::Mutex;
use serde_json::json;

/// The input lease is one per user, so tests that start a desktop session
/// cannot overlap.
static DESKTOP: Mutex<()> = Mutex::new(());

const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const TOGGLE_NOTICE: &str = ". Release with `dcu toggle off --all` before ending.";

#[derive(Default)]
struct FakeBackend {
    captures: AtomicI32,
    actions: AtomicI32,
    entered: AtomicBool,
    block: AtomicBool,
    fail_observation: AtomicBool,
    fail_start: AtomicBool,
    held: Mutex<BTreeSet<String>>,
    toggle_calls: AtomicI32,
    release_toggle_calls: AtomicI32,
    session_context: Mutex<Option<Context>>,
    last_method: Mutex<String>,
    last_params: Mutex<Json>,
}

impl Backend for FakeBackend {
    fn execute(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        *self.last_method.lock() = method.to_owned();
        *self.last_params.lock() = params.clone();
        match method {
            "session.start" if self.fail_start.load(Ordering::SeqCst) => {
                Err(Error::new("setup_required", "Indicator unavailable"))
            }
            "session.start" => {
                *self.session_context.lock() = Some(context.clone());
                Ok(json!({"ready": true}))
            }
            "session.stop" => {
                self.held.lock().clear();
                Ok(json!({}))
            }
            "get-full-screenshot" => Ok(json!({"notice": "Use --coords full."})),
            "get-app-state" => {
                self.captures.fetch_add(1, Ordering::SeqCst);
                if self.fail_observation.load(Ordering::SeqCst) {
                    return Err(Error::new("screenshot_failed", "capture failed"));
                }
                Ok(
                    json!({"includeText": params.get("includeText").and_then(Json::as_bool).unwrap_or(false)}),
                )
            }
            "drag" | "click" => {
                self.actions.fetch_add(1, Ordering::SeqCst);
                self.entered.store(true, Ordering::SeqCst);
                while self.block.load(Ordering::SeqCst) {
                    context.check()?;
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(json!({"delivered": true, "verification": {"state": "unverified"}}))
            }
            _ => Ok(json!({})),
        }
    }

    // Real backends release held input, toggles included, on interrupt.
    fn interrupt(&self) {
        self.held.lock().clear();
    }

    fn set_toggle(&self, key: &str, down: bool, _context: &Context) -> Result<(), Error> {
        self.toggle_calls.fetch_add(1, Ordering::SeqCst);
        let mut held = self.held.lock();
        if down {
            held.insert(key.to_owned());
        } else {
            held.remove(key);
        }
        Ok(())
    }

    fn release_toggles(&self) {
        self.release_toggle_calls.fetch_add(1, Ordering::SeqCst);
        self.held.lock().clear();
    }
}

/// Holds get-app-state or click open until released, so a test can tell
/// whether another agent's operation ran meanwhile.
#[derive(Default)]
struct ConcurrentBackend {
    hold_observation: AtomicBool,
    hold_click: AtomicBool,
    observations_running: AtomicI32,
    clicks_running: AtomicI32,
}

impl ConcurrentBackend {
    fn hold(gate: &AtomicBool, running: &AtomicI32, context: &Context) -> Result<(), Error> {
        running.fetch_add(1, Ordering::SeqCst);
        while gate.load(Ordering::SeqCst) {
            if context.is_cancelled() {
                running.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::cancelled());
            }
            thread::sleep(Duration::from_millis(1));
        }
        running.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Backend for ConcurrentBackend {
    fn supports_concurrent_execute(&self) -> bool {
        true
    }

    fn execute(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        match method {
            "session.start" => return Ok(json!({"ready": true})),
            "get-app-state" => {
                Self::hold(&self.hold_observation, &self.observations_running, context)?
            }
            "click" => Self::hold(&self.hold_click, &self.clicks_running, context)?,
            _ => {}
        }
        Ok(
            json!({"delivered": true, "headless": params.get("headless").and_then(Json::as_bool).unwrap_or(false)}),
        )
    }

    fn interrupt(&self) {}

    fn set_toggle(&self, _key: &str, _down: bool, _context: &Context) -> Result<(), Error> {
        Ok(())
    }

    fn release_toggles(&self) {}
}

fn send(dispatcher: &Dispatcher, method: &str, params: Json) -> Json {
    dispatcher.handle(&json!({"id": "test", "token": TOKEN, "method": method, "params": params}))
}

fn dispatcher_for(backend: &Arc<impl Backend + 'static>) -> Dispatcher {
    Dispatcher::new(backend.clone(), TOKEN.into())
}

fn session_id(response: &Json) -> String {
    response["result"]["sessionId"]
        .as_str()
        .expect("session started")
        .to_owned()
}

fn wait_until(condition: impl Fn() -> bool, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() <= deadline, "timed out waiting: {name}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn single_agent_session_owns_the_desktop_and_stop_cancels_running_input() {
    let _desktop = DESKTOP.lock();
    let backend = Arc::new(FakeBackend::default());
    let dispatcher = dispatcher_for(&backend);
    let unauthorized =
        dispatcher.handle(&json!({"id": "bad", "token": "wrong", "method": "session.start"}));
    assert_eq!(unauthorized["ok"], false, "reject auth");
    assert_eq!(
        send(&dispatcher, "click", json!({"app": "test", "x": 1, "y": 1}))["ok"],
        false,
        "reject no session"
    );

    backend.fail_start.store(true, Ordering::SeqCst);
    assert_eq!(
        send(&dispatcher, "session.start", json!({}))["ok"],
        false,
        "indicator required"
    );
    assert_eq!(
        send(&dispatcher, "session.status", json!({}))["result"]["active"],
        false,
        "failed start inactive"
    );
    backend.fail_start.store(false, Ordering::SeqCst);
    let session = session_id(&send(&dispatcher, "session.start", json!({})));
    assert_eq!(
        send(&dispatcher, "session.start", json!({}))["ok"],
        false,
        "one desktop owner"
    );

    let other_backend = Arc::new(FakeBackend::default());
    let other_token = "b".repeat(64);
    let other = Dispatcher::new(other_backend, other_token.clone());
    let competing =
        other.handle(&json!({"id": "other", "token": other_token, "method": "session.start"}));
    assert_eq!(
        competing["error"]["code"], "session_busy",
        "cross-daemon desktop ownership"
    );

    let mut parameters = json!({"sessionId": session, "app": "test", "x": 1, "y": 1});
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["ok"],
        true,
        "click delivered"
    );
    assert_eq!(
        backend.captures.load(Ordering::SeqCst),
        0,
        "actions skip capture by default"
    );
    parameters["observe"] = json!("screenshot");
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["result"]["observation"]["includeText"],
        false,
        "screenshot skips accessibility"
    );
    backend.fail_observation.store(true, Ordering::SeqCst);
    let result = send(&dispatcher, "click", parameters.clone());
    assert!(
        result["ok"] == true
            && result["result"]["delivered"] == true
            && result["result"].get("observationError").is_some(),
        "capture failure preserves delivery outcome"
    );
    parameters["elementIndex"] = json!(3);
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["ok"],
        false,
        "element requires observation"
    );
    let fields = parameters.as_object_mut().unwrap();
    fields.remove("elementIndex");
    fields.remove("observe");

    let actions_before = backend.actions.load(Ordering::SeqCst);
    parameters["coords"] = json!("window");
    assert!(
        send(&dispatcher, "click", parameters.clone())["ok"] == false
            && backend.actions.load(Ordering::SeqCst) == actions_before,
        "unknown coords rejected before input"
    );
    parameters["coords"] = json!("full");
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["ok"],
        true,
        "full coords accepted"
    );
    parameters.as_object_mut().unwrap().remove("coords");
    backend.fail_observation.store(false, Ordering::SeqCst);

    let captures_before = backend.captures.load(Ordering::SeqCst);
    assert_eq!(
        send(
            &dispatcher,
            "get-full-screenshot",
            json!({"sessionId": session, "app": "test"})
        )["ok"],
        false,
        "full screenshot requires observation"
    );
    let full =
        json!({"sessionId": session, "app": "test", "observationId": "o", "observe": "screenshot"});
    assert_eq!(
        send(&dispatcher, "get-full-screenshot", full)["ok"],
        true,
        "full screenshot served"
    );
    assert_eq!(
        backend.captures.load(Ordering::SeqCst),
        captures_before,
        "full screenshot never recaptures"
    );
    parameters["x"] = json!(-1);
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["ok"],
        false,
        "invalid coordinate rejected before input"
    );
    parameters["x"] = json!(1);

    backend.entered.store(false, Ordering::SeqCst);
    backend.block.store(true, Ordering::SeqCst);
    let cancelled_response = thread::scope(|scope| {
        let worker = scope.spawn(|| send(&dispatcher, "click", parameters.clone()));
        wait_until(|| backend.entered.load(Ordering::SeqCst), "click entered");
        assert_eq!(
            send(&dispatcher, "click", parameters.clone())["error"]["code"],
            "busy",
            "overlapping input rejected"
        );
        assert_eq!(
            send(&dispatcher, "session.stop", json!({}))["ok"],
            true,
            "stop bypasses active operation"
        );
        worker.join().unwrap()
    });
    assert_eq!(
        cancelled_response["error"]["code"], "cancelled",
        "running action cancelled"
    );
    assert_eq!(
        send(&dispatcher, "click", parameters.clone())["ok"],
        false,
        "stopped owner cannot input"
    );
    assert_eq!(
        send(&dispatcher, "doctor", json!({}))["ok"],
        true,
        "doctor works after cancellation"
    );
    dispatcher.shutdown();
}

#[test]
fn toggles_stay_held_until_released_and_block_stopping() {
    let _desktop = DESKTOP.lock();
    let backend = Arc::new(FakeBackend::default());
    let toggled = dispatcher_for(&backend);
    let call = |method: &str, params: Json| send(&toggled, method, params);
    assert_eq!(
        call("toggle.set", json!({"key": "shift", "on": true}))["error"]["code"],
        "session_required",
        "toggle requires session"
    );
    let id = session_id(&call("session.start", json!({})));
    assert_eq!(
        call("session.status", json!({}))["result"]["toggles"],
        json!([]),
        "status lists no toggles"
    );
    assert!(
        call("session.status", json!({}))["result"]
            .get("notice")
            .is_none(),
        "no notice without toggles"
    );
    assert_eq!(
        call(
            "toggle.set",
            json!({"sessionId": id, "key": "capslock", "on": true})
        )["error"]["code"],
        "invalid_argument",
        "unknown toggle key rejected"
    );
    assert_eq!(
        call("toggle.set", json!({"sessionId": id, "key": "shift"}))["error"]["code"],
        "invalid_argument",
        "toggle requires on"
    );
    let drag = |modifiers: &str| json!({"sessionId": id, "app": "test", "fromX": 1, "fromY": 1, "toX": 2, "toY": 2, "modifiers": modifiers});
    assert_eq!(
        call("drag", drag("shift+space"))["error"]["code"],
        "invalid_argument",
        "unknown drag modifier rejected"
    );
    assert_eq!(
        call("drag", drag("Shift+ctrl"))["ok"],
        true,
        "drag modifiers accepted"
    );

    let on = call(
        "toggle.set",
        json!({"sessionId": id, "key": "shift", "on": true}),
    );
    assert!(
        on["ok"] == true
            && on["result"]["changed"] == true
            && backend.held.lock().contains("shift"),
        "toggle pressed"
    );
    assert_eq!(on["result"]["toggles"], json!(["shift"]), "toggle listed");
    assert_eq!(
        on["result"]["notice"],
        format!("Toggle still on: shift{TOGGLE_NOTICE}"),
        "toggle notice"
    );
    let calls_before = backend.toggle_calls.load(Ordering::SeqCst);
    assert!(
        call(
            "toggle.set",
            json!({"sessionId": id, "key": "SHIFT", "on": true})
        )["result"]["changed"]
            == false
            && backend.toggle_calls.load(Ordering::SeqCst) == calls_before,
        "toggle on is idempotent"
    );
    assert_eq!(
        call(
            "toggle.set",
            json!({"sessionId": id, "key": "super", "on": true})
        )["result"]["toggles"],
        json!(["shift", "win"]),
        "super aliases win"
    );
    assert_eq!(
        call("session.status", json!({}))["result"]["toggles"],
        json!(["shift", "win"]),
        "status lists toggles"
    );
    assert_eq!(
        call("toggle.status", json!({}))["result"]["notice"],
        format!("Toggle still on: shift, win{TOGGLE_NOTICE}"),
        "toggle.status notice"
    );
    assert_eq!(
        call(
            "click",
            json!({"sessionId": id, "app": "test", "x": 1, "y": 1})
        )["result"]["toggles"],
        json!(["shift", "win"]),
        "action lists toggles"
    );
    let full = call(
        "get-full-screenshot",
        json!({"sessionId": id, "app": "test", "observationId": "o"}),
    );
    assert_eq!(
        full["result"]["notice"],
        format!("Use --coords full. Toggle still on: shift, win{TOGGLE_NOTICE}"),
        "toggle notice appends to an existing notice"
    );
    let failed = call("click", json!({"sessionId": id, "app": "test"}));
    assert!(
        failed["error"]["code"] == "invalid_argument"
            && failed["error"]["details"]["toggles"] == json!(["shift", "win"])
            && failed["error"]["details"].get("notice").is_some(),
        "errors carry toggles"
    );
    let unauthorized =
        toggled.handle(&json!({"id": "bad", "token": "wrong", "method": "session.status"}));
    assert!(
        unauthorized["error"].get("details").is_none(),
        "unauthorized errors reveal nothing"
    );

    for method in ["session.stop", "daemon.shutdown"] {
        let refused = call(method, json!({}));
        assert_eq!(
            refused["error"]["code"], "toggles_active",
            "stop refused while toggles are on"
        );
        assert_eq!(
            refused["error"]["details"]["toggles"],
            json!(["shift", "win"]),
            "refusal lists toggles"
        );
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("dcu toggle off --all"),
            "refusal names the release command"
        );
    }
    assert!(
        !toggled.is_quitting() && call("session.status", json!({}))["result"]["active"] == true,
        "refused stop keeps the session"
    );
    assert!(
        call(
            "toggle.set",
            json!({"sessionId": id, "key": "shift", "on": false})
        )["result"]["toggles"]
            == json!(["win"])
            && !backend.held.lock().contains("shift"),
        "toggle off releases one key"
    );
    let released = call("toggle.release-all", json!({"sessionId": id}));
    assert!(
        released["result"]["released"] == json!(["win"])
            && released["result"]["toggles"] == json!([])
            && released["result"].get("notice").is_none()
            && backend.held.lock().is_empty(),
        "release-all clears toggles"
    );
    assert_eq!(
        call("session.stop", json!({}))["ok"],
        true,
        "stop succeeds after release"
    );
    toggled.shutdown();
}

#[test]
fn double_escape_stop_clears_and_releases_toggles() {
    let _desktop = DESKTOP.lock();
    let backend = Arc::new(FakeBackend::default());
    let escaped = dispatcher_for(&backend);
    let id = session_id(&send(&escaped, "session.start", json!({})));
    assert_eq!(
        send(
            &escaped,
            "toggle.set",
            json!({"sessionId": id, "key": "space", "on": true})
        )["ok"],
        true,
        "space toggled"
    );

    backend
        .session_context
        .lock()
        .as_ref()
        .expect("session context kept")
        .cancel();
    assert_eq!(
        send(&escaped, "session.stop", json!({}))["ok"],
        true,
        "stop allowed once the session is cancelled"
    );
    escaped.tick();

    let status = &send(&escaped, "session.status", json!({}))["result"];
    assert!(
        status["active"] == false && status["toggles"] == json!([]),
        "Esc stop clears toggles"
    );
    assert!(
        backend.held.lock().is_empty() && backend.release_toggle_calls.load(Ordering::SeqCst) > 0,
        "Esc stop releases toggled keys"
    );
    escaped.shutdown();
}

#[test]
fn idle_expiry_clears_and_releases_toggles() {
    let _desktop = DESKTOP.lock();
    let backend = Arc::new(FakeBackend::default());
    let idle = Dispatcher::with_timeouts(
        backend.clone(),
        TOKEN.into(),
        Duration::from_millis(50),
        Duration::from_secs(45),
    );
    let id = session_id(&send(&idle, "session.start", json!({})));
    assert_eq!(
        send(
            &idle,
            "toggle.set",
            json!({"sessionId": id, "key": "ctrl", "on": true})
        )["ok"],
        true,
        "ctrl toggled"
    );

    thread::sleep(Duration::from_millis(120));
    idle.tick();

    let status = &send(&idle, "session.status", json!({}))["result"];
    assert!(
        status["active"] == false && status["toggles"] == json!([]),
        "idle expiry clears toggles"
    );
    assert!(
        backend.held.lock().is_empty() && backend.release_toggle_calls.load(Ordering::SeqCst) > 0,
        "idle expiry releases toggled keys"
    );
    idle.shutdown();
}

#[test]
fn multi_agent_sessions_share_the_desktop_with_headless_and_toggle_rules() {
    let _desktop = DESKTOP.lock();
    let shared = Arc::new(FakeBackend::default());
    let desktop = Dispatcher::with_timeouts(
        shared.clone(),
        TOKEN.into(),
        Duration::from_secs(120),
        Duration::from_millis(200),
    );
    let call = |method: &str, params: Json| send(&desktop, method, params);
    let alpha_start = call(
        "session.start",
        json!({"multiAgent": true, "agentId": "alpha"}),
    );
    assert!(
        alpha_start["ok"] == true
            && alpha_start["result"]["agentId"] == "alpha"
            && alpha_start["result"]["joined"] == false,
        "first multi-agent session starts the desktop"
    );
    let alpha = session_id(&alpha_start);
    assert_eq!(
        call("session.start", json!({}))["error"]["code"],
        "session_busy",
        "single-agent start cannot join a shared desktop"
    );
    assert_eq!(
        call(
            "session.start",
            json!({"multiAgent": true, "agentId": "alpha"})
        )["error"]["code"],
        "agent_id_in_use",
        "agent ids are unique"
    );
    assert_eq!(
        call(
            "session.start",
            json!({"multiAgent": true, "agentId": "a b"})
        )["error"]["code"],
        "invalid_argument",
        "agent id must be printable"
    );
    let beta_start = call(
        "session.start",
        json!({"multiAgent": true, "headless": true}),
    );
    let beta = session_id(&beta_start);
    assert!(
        beta_start["result"]["joined"] == true
            && beta_start["result"]["agentCount"] == 2
            && beta_start["result"]["agentId"] == "agent-1"
            && beta_start["result"]["headless"] == true,
        "second agent joins with a generated id"
    );
    assert_ne!(beta, alpha, "each agent gets its own session id");

    let status = &call("session.status", json!({"sessionId": alpha}))["result"];
    assert!(
        status["sessionId"] == alpha && status["agentCount"] == 2,
        "status shows the caller's session"
    );
    assert!(
        call("session.status", json!({}))["result"]["sessionId"].is_null(),
        "status hides shared session ids"
    );

    let element = |session: &str| json!({"sessionId": session, "app": "test", "observationId": "o", "elementIndex": 2});
    assert_eq!(
        call("click", element(&beta))["ok"],
        true,
        "headless element click accepted"
    );
    {
        let last = shared.last_params.lock();
        assert!(
            last["headless"] == true && last["agentSessionId"] == beta && last["sessionId"] != beta,
            "backend sees the headless flag and the owning agent"
        );
    }
    assert_eq!(
        call(
            "type-text",
            json!({"sessionId": beta, "app": "test", "text": "x"})
        )["error"]["code"],
        "headless_unavailable",
        "session default headless refuses typing"
    );
    assert_eq!(
        call(
            "click",
            json!({"sessionId": beta, "app": "test", "x": 1, "y": 1})
        )["error"]["code"],
        "headless_unavailable",
        "headless refuses a coordinate click"
    );
    let mut right_click = element(&beta);
    right_click["button"] = json!("right");
    assert_eq!(
        call("click", right_click)["error"]["code"],
        "headless_unavailable",
        "headless refuses a right click"
    );
    let actions_before = shared.actions.load(Ordering::SeqCst);
    assert_eq!(
        call(
            "type-text",
            json!({"sessionId": beta, "app": "test", "text": "x", "headless": false})
        )["ok"],
        true,
        "request headless false overrides the session default"
    );
    let physical = call(
        "click",
        json!({"sessionId": alpha, "app": "test", "x": 1, "y": 1}),
    );
    assert!(
        physical["result"]["delivery"] == "physical"
            && physical["result"]["sessionId"] == alpha
            && shared.actions.load(Ordering::SeqCst) == actions_before + 1
            && shared.last_params.lock()["headless"] == false,
        "physical input reports its delivery"
    );

    assert_eq!(
        call(
            "toggle.set",
            json!({"sessionId": alpha, "key": "shift", "on": true})
        )["ok"],
        true,
        "owner holds shift"
    );
    let refused = call(
        "click",
        json!({"sessionId": beta, "app": "test", "x": 1, "y": 1, "headless": false}),
    );
    assert!(
        refused["error"]["code"] == "toggles_active"
            && refused["error"]["details"]["notice"]
                .as_str()
                .unwrap()
                .contains("Agent alpha"),
        "another agent's toggle blocks physical input"
    );
    assert_eq!(
        call("click", element(&beta))["ok"],
        true,
        "headless input ignores another agent's toggles"
    );
    let mut set_state = element(&beta);
    set_state["state"] = json!("checked");
    set_state["headless"] = json!(false);
    assert!(
        call("set-state", set_state)["ok"] == true && *shared.last_method.lock() == "set-state",
        "set-state ignores another agent's toggles even when not headless"
    );
    let mut unknown_state = element(&beta);
    unknown_state["state"] = json!("pressed");
    assert_eq!(
        call("set-state", unknown_state)["error"]["code"],
        "invalid_argument",
        "set-state refuses an unknown state"
    );
    assert_eq!(
        call("set-state", element(&beta))["error"]["code"],
        "invalid_argument",
        "set-state requires a state"
    );
    assert_eq!(
        call("scroll-to", json!({"sessionId": beta, "app": "test"}))["error"]["code"],
        "invalid_argument",
        "scroll-to requires an element"
    );
    assert_eq!(
        call("toggle.release-all", json!({"sessionId": beta}))["error"]["code"],
        "toggles_active",
        "only the owner changes toggles"
    );
    assert_eq!(
        call("session.stop", json!({"sessionId": alpha}))["error"]["code"],
        "toggles_active",
        "the toggle owner cannot leave with keys held"
    );
    assert_eq!(
        call("toggle.release-all", json!({"sessionId": alpha}))["ok"],
        true,
        "owner releases toggles"
    );

    let beta_stop = call("session.stop", json!({"sessionId": beta}));
    assert!(
        beta_stop["ok"] == true && beta_stop["result"]["agentCount"] == 1,
        "stopping one agent keeps the desktop"
    );
    assert_eq!(
        call("session.status", json!({}))["result"]["active"],
        true,
        "desktop survives one agent leaving"
    );
    assert_eq!(
        call(
            "click",
            json!({"sessionId": beta, "app": "test", "x": 1, "y": 1})
        )["error"]["code"],
        "session_required",
        "a stopped agent cannot input"
    );
    assert_eq!(
        call("session.stop", json!({"sessionId": "unknown"}))["error"]["code"],
        "session_required",
        "an unknown id cannot stop other agents"
    );
    assert!(
        call("session.stop", json!({"sessionId": alpha}))["ok"] == true
            && call("session.status", json!({}))["result"]["active"] == false,
        "the last agent stops the desktop"
    );
    desktop.shutdown();
}

#[test]
fn serial_backend_makes_other_agents_wait_for_their_turn() {
    let _desktop = DESKTOP.lock();
    let serialized = Arc::new(FakeBackend::default());
    let desktop = Dispatcher::with_timeouts(
        serialized.clone(),
        TOKEN.into(),
        Duration::from_secs(120),
        Duration::from_millis(100),
    );
    let alpha = session_id(&send(
        &desktop,
        "session.start",
        json!({"multiAgent": true}),
    ));
    let beta = session_id(&send(
        &desktop,
        "session.start",
        json!({"multiAgent": true}),
    ));
    serialized.entered.store(false, Ordering::SeqCst);
    serialized.block.store(true, Ordering::SeqCst);

    let blocked = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            send(
                &desktop,
                "click",
                json!({"sessionId": alpha, "app": "test", "x": 1, "y": 1}),
            )
        });
        wait_until(
            || serialized.entered.load(Ordering::SeqCst),
            "alpha click entered",
        );
        assert_eq!(
            send(
                &desktop,
                "get-app-state",
                json!({"sessionId": beta, "app": "test"})
            )["error"]["code"],
            "busy",
            "a serial backend makes other agents wait, then gives up"
        );
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(30));
            serialized.block.store(false, Ordering::SeqCst);
        });
        assert_eq!(
            send(
                &desktop,
                "get-app-state",
                json!({"sessionId": beta, "app": "test"})
            )["ok"],
            true,
            "a waiting agent runs once its turn comes"
        );
        worker.join().unwrap()
    });
    assert_eq!(blocked["ok"], true, "the first agent's click completed");
    desktop.shutdown();
}

#[test]
fn concurrent_backend_runs_observations_and_semantic_actions_during_physical_input() {
    let _desktop = DESKTOP.lock();
    let parallel = Arc::new(ConcurrentBackend::default());
    let desktop = Dispatcher::with_timeouts(
        parallel.clone(),
        TOKEN.into(),
        Duration::from_secs(120),
        Duration::from_millis(100),
    );
    let call = |method: &str, params: Json| send(&desktop, method, params);
    let alpha = session_id(&call("session.start", json!({"multiAgent": true})));
    let beta = session_id(&call("session.start", json!({"multiAgent": true})));
    let gamma = session_id(&call("session.start", json!({"multiAgent": true})));

    parallel.hold_observation.store(true, Ordering::SeqCst);
    let (slow_observation, second_observation) = thread::scope(|scope| {
        let first =
            scope.spawn(|| call("get-app-state", json!({"sessionId": alpha, "app": "test"})));
        let second =
            scope.spawn(|| call("get-app-state", json!({"sessionId": beta, "app": "test"})));
        wait_until(
            || parallel.observations_running.load(Ordering::SeqCst) == 2,
            "two agents observe at the same time",
        );
        parallel.hold_observation.store(false, Ordering::SeqCst);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert!(
        slow_observation["ok"] == true && second_observation["ok"] == true,
        "concurrent observations both succeed"
    );

    parallel.hold_click.store(true, Ordering::SeqCst);
    let physical_click = thread::scope(|scope| {
        let clicker = scope.spawn(|| {
            call(
                "click",
                json!({"sessionId": alpha, "app": "test", "x": 1, "y": 1}),
            )
        });
        wait_until(
            || parallel.clicks_running.load(Ordering::SeqCst) == 1,
            "alpha physical click running",
        );
        assert_eq!(
            call("get-app-state", json!({"sessionId": beta, "app": "test"}))["ok"],
            true,
            "observation runs during physical input"
        );
        assert_eq!(
            call(
                "click",
                json!({"sessionId": beta, "app": "test", "x": 1, "y": 1})
            )["error"]["code"],
            "busy",
            "physical input takes turns"
        );
        let element =
            json!({"sessionId": beta, "app": "test", "observationId": "o", "elementIndex": 1});
        let mut expanded = element.clone();
        expanded["state"] = json!("expanded");
        assert_eq!(
            call("set-state", expanded)["ok"],
            true,
            "set-state runs during another agent's physical input"
        );
        assert_eq!(
            call("scroll-to", element.clone())["ok"],
            true,
            "scroll-to runs during another agent's physical input"
        );
        let mut medium = element.clone();
        medium["option"] = json!("Medium");
        assert_eq!(
            call("select-option", medium)["ok"],
            true,
            "select-option runs during another agent's physical input"
        );
        let mut empty = element;
        empty["option"] = json!("");
        assert_eq!(
            call("select-option", empty)["error"]["code"],
            "invalid_argument",
            "select-option refuses an empty option"
        );

        let headless = scope.spawn(|| {
            call(
                "click",
                json!({"sessionId": gamma, "app": "test", "observationId": "o", "elementIndex": 1, "headless": true}),
            )
        });
        wait_until(
            || parallel.clicks_running.load(Ordering::SeqCst) == 2,
            "headless click runs during physical input",
        );
        let gamma_stop = call("session.stop", json!({"sessionId": gamma}));
        let headless_click = headless.join().unwrap();
        assert!(
            gamma_stop["ok"] == true && headless_click["error"]["code"] == "cancelled",
            "stopping one agent cancels only its operation"
        );
        assert_eq!(
            parallel.clicks_running.load(Ordering::SeqCst),
            1,
            "the other agent's click keeps running"
        );
        parallel.hold_click.store(false, Ordering::SeqCst);
        clicker.join().unwrap()
    });
    assert_eq!(
        physical_click["ok"], true,
        "physical click completes after another agent stopped"
    );
    desktop.shutdown();
}

#[test]
fn idle_agent_expires_alone_while_another_stays_active() {
    let _desktop = DESKTOP.lock();
    let backend = Arc::new(FakeBackend::default());
    let idle = Dispatcher::with_timeouts(
        backend,
        TOKEN.into(),
        Duration::from_millis(100),
        Duration::from_secs(45),
    );
    let alpha = session_id(&send(&idle, "session.start", json!({"multiAgent": true})));
    let beta = session_id(&send(&idle, "session.start", json!({"multiAgent": true})));

    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        send(&idle, "list-apps", json!({"sessionId": beta}))["ok"],
        true,
        "beta stays active"
    );
    idle.tick();

    let status = &send(&idle, "session.status", json!({"sessionId": beta}))["result"];
    assert!(
        status["active"] == true && status["agentCount"] == 1,
        "an idle agent expires alone"
    );
    assert_eq!(
        send(&idle, "list-apps", json!({"sessionId": alpha}))["error"]["code"],
        "session_required",
        "the expired agent is gone"
    );
    idle.shutdown();
}

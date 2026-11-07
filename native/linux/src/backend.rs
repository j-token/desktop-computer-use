use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dcu_core::{Backend, Context, Error, Json, WindowRelation, find_blocking_modal};
use parking_lot::Mutex;
use serde_json::json;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::atspi::{self, Accessible};
use crate::atspi_actions::{self, StateChange};
use crate::bus::{SessionBus, ShellStopSubscription};
use crate::eis_client::{EisClient, libei_runtime_available};
use crate::image::{self, RgbaImage};
#[cfg(feature = "pipewire")]
use crate::pipewire::PipeWireCapture;
use crate::portal_bus::{
    self, PORTAL_BUS_NAME, PORTAL_DESKTOP_PATH, PortalBus, VariantDict, empty_options,
};
use crate::x11::{self, X11Display};

/// Wayland capture needs libpipewire; the other desktop pieces are pure Rust
/// and always built in.
const HAVE_PIPEWIRE: bool = cfg!(feature = "pipewire");

const DEFAULT_DRAG_DURATION_MS: i32 = 240;
const DEFAULT_DRAG_STEPS: i32 = 12;
const DEFAULT_HOLD_BEFORE_MS: i32 = 50;
const DEFAULT_HOLD_AFTER_MS: i32 = 50;
/// How long the GNOME extension releases Escape before an injected Escape press.
const STOP_KEY_SUSPEND_MS: u32 = 300;
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1500);
const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_OBSERVATIONS: usize = 64;
const INPUT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: String,
    pub app: String,
    pub title: String,
    pub pid: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub active: bool,
    /// Transient-for parent of a dialog, when that parent is listed.
    pub owner_id: String,
    /// A modal dialog that blocks input to owner_id.
    pub modal: bool,
}

impl WindowInfo {
    fn same_geometry(&self, other: &WindowInfo) -> bool {
        self.id == other.id
            && self.x == other.x
            && self.y == other.y
            && self.width == other.width
            && self.height == other.height
    }

    fn to_json(&self) -> Json {
        let mut window = json!({
            "id": self.id,
            "app": self.app,
            "title": self.title,
            "pid": self.pid,
            "x": self.x,
            "y": self.y,
            "width": self.width,
            "height": self.height,
            "active": self.active,
        });
        if !self.owner_id.is_empty() {
            window["ownerWindowId"] = json!(self.owner_id);
            window["modal"] = json!(self.modal);
        }
        window
    }
}

/// One encoded screenshot. scale_x/scale_y map window-local coordinates to
/// this image's pixels: pixel = window * scale.
#[derive(Clone, Default)]
struct ObservationImage {
    path: PathBuf,
    width: i32,
    height: i32,
    scale_x: f64,
    scale_y: f64,
}

impl ObservationImage {
    fn action_transform(&self) -> Json {
        json!({"scaleX": self.scale_x, "scaleY": self.scale_y, "offsetX": 0, "offsetY": 0})
    }
}

struct ObservationRecord {
    /// The agent session that took it; another agent's indexes and screenshot
    /// pixels describe what that agent saw, not this one.
    owner: String,
    window: WindowInfo,
    created: Instant,
    coordinate_revision: u64,
    has_screenshot: bool,
    mime: String,
    reduced: ObservationImage,
    full: ObservationImage,
}

impl ObservationRecord {
    fn remove_files(&self) {
        for image in [&self.reduced, &self.full] {
            if !image.path.as_os_str().is_empty() {
                let _ = std::fs::remove_file(&image.path);
            }
        }
    }
}

/// Screenshot pixel space selected for coordinate input.
struct CoordinateTransform {
    space: &'static str,
    image: ObservationImage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalStream {
    valid: bool,
    node_id: u32,
    pipewire_serial: u64,
    mapping_id: String,
    position_x: i32,
    position_y: i32,
    compositor_width: i32,
    compositor_height: i32,
    logical_width: i32,
    logical_height: i32,
}

impl Default for PortalStream {
    fn default() -> Self {
        Self {
            valid: false,
            node_id: u32::MAX,
            pipewire_serial: 0,
            mapping_id: String::new(),
            position_x: 0,
            position_y: 0,
            compositor_width: 0,
            compositor_height: 0,
            logical_width: 0,
            logical_height: 0,
        }
    }
}

/// The first stream of a RemoteDesktop `Start` response.
fn first_stream(values: &VariantDict) -> PortalStream {
    let mut stream = PortalStream::default();
    let Some(streams) = values
        .get("streams")
        .and_then(|value| value.try_clone().ok())
    else {
        return stream;
    };
    let Ok(streams) = Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(streams) else {
        return stream;
    };
    let Some((node_id, properties)) = streams.into_iter().next() else {
        return stream;
    };
    // A response can contain an empty or sentinel stream tuple when the
    // compositor denied capture. Such a tuple must never make the backend
    // advertise a started stream.
    stream.valid = node_id != u32::MAX;
    if !stream.valid {
        return stream;
    }
    stream.node_id = node_id;
    if let Some(Value::U64(serial)) = properties.get("pipewire-serial").map(|value| &**value) {
        stream.pipewire_serial = *serial;
    }
    stream.mapping_id = portal_bus::variant_string(&properties, "mapping_id");
    let pair = |key: &str| {
        let Some(Value::Structure(structure)) = properties.get(key).map(|value| &**value) else {
            return None;
        };
        match structure.fields() {
            [Value::I32(first), Value::I32(second)] => Some((*first, *second)),
            _ => None,
        }
    };
    if let Some((x, y)) = pair("position") {
        (stream.position_x, stream.position_y) = (x, y);
    }
    if let Some((width, height)) = pair("size") {
        (stream.compositor_width, stream.compositor_height) = (width, height);
    }
    if let Some((width, height)) = pair("logical_size") {
        (stream.logical_width, stream.logical_height) = (width, height);
    }
    stream
}

/// State the monitor threads share with the backend.
struct Shared {
    shell: SessionBus,
    interrupted: AtomicBool,
    session_active: AtomicBool,
    heartbeat_stop: AtomicBool,
    stop_monitor_stop: AtomicBool,
    active_context: Mutex<Option<Context>>,
}

impl Shared {
    fn cancel_active_context(&self) {
        if let Some(context) = &*self.active_context.lock() {
            context.cancel();
        }
    }

    /// The indicator stopped or its bus disappeared. The serialized worker
    /// performs the actual release and portal cleanup after it observes this
    /// cancellation.
    fn end_session(&self) {
        self.session_active.store(false, Ordering::Release);
        self.heartbeat_stop.store(true, Ordering::Release);
        self.cancel_active_context();
        self.stop_monitor_stop.store(true, Ordering::Release);
    }
}

pub struct LinuxBackend {
    shared: Arc<Shared>,
    state: Mutex<State>,
}

pub fn make_backend() -> Result<Arc<dyn Backend>, Error> {
    Ok(Arc::new(LinuxBackend::new()))
}

impl LinuxBackend {
    fn new() -> Self {
        let session_type = env("XDG_SESSION_TYPE");
        let wayland =
            session_type.eq_ignore_ascii_case("wayland") || !env("WAYLAND_DISPLAY").is_empty();
        let display = if env("DISPLAY").is_empty() {
            None
        } else {
            X11Display::open()
        };
        let shared = Arc::new(Shared {
            shell: SessionBus::new(),
            interrupted: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            heartbeat_stop: AtomicBool::new(false),
            stop_monitor_stop: AtomicBool::new(true),
            active_context: Mutex::new(None),
        });
        let state = State {
            shared: Arc::clone(&shared),
            portal: PortalBus::new(),
            x11: display.is_some(),
            display,
            wayland,
            session_type,
            image_directory: runtime_directory(),
            session_id: String::new(),
            last_activity: Instant::now(),
            toggled: BTreeSet::new(),
            heartbeat_thread: None,
            stop_monitor_thread: None,
            observations: HashMap::new(),
            coordinate_revision: 0,
            coordinate_signature: String::new(),
            remote: RemoteDesktop::default(),
            eis: None,
            #[cfg(feature = "pipewire")]
            pipewire: None,
            accessibility: None,
        };
        Self {
            shared,
            state: Mutex::new(state),
        }
    }
}

impl Backend for LinuxBackend {
    fn execute(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        let mut state = self.state.lock();
        // The dispatcher serializes calls, but this guard still ensures a
        // stale session cannot change the desktop after a heartbeat or
        // interrupt event.
        let controls_session = matches!(
            method,
            "doctor" | "capabilities" | "session.start" | "session.stop"
        );
        if !controls_session && self.shared.interrupted.load(Ordering::Acquire) {
            return Err(session_interrupted());
        }
        match method {
            "doctor" => return Ok(state.doctor()),
            "capabilities" => return Ok(state.capabilities()),
            "session.start" => return state.session_start(params, context),
            "session.stop" => return state.session_stop(params),
            _ => {}
        }
        let result = state.run(method, params, context)?;
        // The idle timeout counts from the last successful request, as in the
        // dispatcher, not from session start.
        state.last_activity = Instant::now();
        Ok(result)
    }

    fn interrupt(&self) {
        self.shared.interrupted.store(true, Ordering::Release);
        self.shared.heartbeat_stop.store(true, Ordering::Release);
        self.shared.cancel_active_context();
    }

    fn set_toggle(&self, key: &str, down: bool, context: &Context) -> Result<(), Error> {
        self.state.lock().set_toggle(key, down, context)
    }

    fn release_toggles(&self) {
        self.state.lock().release_toggled_keys();
    }
}

impl Drop for LinuxBackend {
    fn drop(&mut self) {
        self.shared.heartbeat_stop.store(true, Ordering::Release);
        self.shared.stop_monitor_stop.store(true, Ordering::Release);
        let state = self.state.get_mut();
        state.join_monitors();
        state.release_inputs();
        #[cfg(feature = "pipewire")]
        {
            state.pipewire = None;
        }
        state.close_portal_sessions();
    }
}

/// RemoteDesktop portal session and the stream it shares with ScreenCast.
#[derive(Default)]
struct RemoteDesktop {
    session: String,
    started: bool,
    eis_selected: bool,
    stream: PortalStream,
}

struct State {
    shared: Arc<Shared>,
    portal: PortalBus,
    display: Option<X11Display>,
    wayland: bool,
    x11: bool,
    session_type: String,
    image_directory: PathBuf,
    session_id: String,
    last_activity: Instant,
    /// Canonical names (shift, ctrl, alt, super, space) of keys held across
    /// requests.
    toggled: BTreeSet<String>,
    heartbeat_thread: Option<JoinHandle<()>>,
    stop_monitor_thread: Option<JoinHandle<()>>,
    observations: HashMap<String, ObservationRecord>,
    coordinate_revision: u64,
    coordinate_signature: String,
    remote: RemoteDesktop,
    eis: Option<EisClient>,
    #[cfg(feature = "pipewire")]
    pipewire: Option<PipeWireCapture>,
    accessibility: Option<Connection>,
}

impl State {
    fn shell(&self) -> &SessionBus {
        &self.shared.shell
    }

    fn run(&mut self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        match method {
            "list-windows" => return Ok(self.list_windows()),
            "list-apps" => return Ok(self.list_apps()),
            "get-app-state" => return self.make_observation(params, context),
            "get-full-screenshot" => return self.full_screenshot(params),
            _ => {}
        }
        let is_action = matches!(
            method,
            "click"
                | "drag"
                | "scroll"
                | "type-text"
                | "press-key"
                | "hotkey"
                | "set-value"
                | "paste-text"
                | "set-state"
                | "scroll-to"
                | "select-option"
        );
        if is_action {
            // A modal parent ignores input, so it would only look delivered.
            self.ensure_active(params)?;
            self.reject_blocked_target(params)?;
        }
        match method {
            "click" => self.click(params, context),
            "drag" => self.drag(params, context),
            "scroll" => self.scroll(params, context),
            "type-text" => self.type_text(params, context),
            "press-key" => self.press_key(params, context),
            "hotkey" => self.hotkey(params, context),
            "set-value" => self.set_value(params, context),
            "paste-text" => self.paste_text(params, context),
            "set-state" => self.set_state(params, context),
            "scroll-to" => self.scroll_to(params, context),
            "select-option" => self.select_option(params, context),
            _ => Err(Error::new(
                "unsupported",
                format!("Unknown Linux backend method: {method}"),
            )),
        }
    }

    fn ensure_active(&self, params: &Json) -> Result<(), Error> {
        if !self.shared.session_active.load(Ordering::Acquire) {
            return Err(session_required());
        }
        let requested = string_value(params, "sessionId", "")?;
        if !requested.is_empty() && requested != self.session_id {
            return Err(Error::new(
                "session_owner",
                "The input session does not belong to this client",
            ));
        }
        if self.last_activity.elapsed() > IDLE_TIMEOUT {
            return Err(Error::new(
                "session_expired",
                "The computer-use session expired after 120 seconds idle",
            ));
        }
        Ok(())
    }

    fn doctor(&self) -> Json {
        let indicator_available = self.shell().available();
        let mut result = json!({
            "platform": "linux",
            "sessionType": if self.session_type.is_empty() { "unknown" } else { &self.session_type },
            "graphicalSession": self.wayland || self.x11,
            "xdgRuntimeDir": env("XDG_RUNTIME_DIR"),
            "dbusSessionBus": !env("DBUS_SESSION_BUS_ADDRESS").is_empty(),
            "display": env("DISPLAY"),
            "waylandDisplay": env("WAYLAND_DISPLAY"),
            "dependencies": {
                "gio": true,
                "x11": true,
                "xtest": true,
                "xrandr": true,
                "jpeg": true,
                "png": true,
                "pipewire": HAVE_PIPEWIRE,
                "atspi": true,
                "wlCopy": command_exists("wl-copy"),
                "xclip": command_exists("xclip"),
                "xdotool": command_exists("xdotool"),
            },
            "gnomeExtension": indicator_available,
            "portal": self.portal.available(),
            "ready": indicator_available && (self.x11 || (self.wayland && HAVE_PIPEWIRE)),
            "stopKey": "Escape x2",
            "indicatorRequired": true,
        });
        if !indicator_available {
            result["gnomeExtensionError"] = json!(self.shell().error());
        }
        if !self.portal.available() {
            result["portalError"] = json!(self.portal.error());
        }
        let mut notes = Vec::new();
        if !self.wayland && !self.x11 {
            notes.push("Run inside the logged-in GNOME graphical session; SSH alone is not a desktop session.");
        }
        if self.wayland && !HAVE_PIPEWIRE {
            notes.push(
                "Wayland requires the XDG ScreenCast/RemoteDesktop portals and PipeWire development/runtime support.",
            );
        }
        if !indicator_available {
            notes.push(
                "Install and enable the GNOME desktop-computer-use extension before session.start.",
            );
        }
        result["notes"] = json!(notes);
        result
    }

    fn capabilities(&self) -> Json {
        let indicator_available = self.shell().available();
        let physical_input = self.x11 || self.wayland;
        json!({
            "platform": "linux",
            "sessionType": if self.session_type.is_empty() { "unknown" } else { &self.session_type },
            "input": {
                "click": physical_input,
                "drag": physical_input,
                "scroll": physical_input,
                "keyboard": physical_input,
                "modifierClick": physical_input,
                "setValue": true,
                "pasteText": command_exists("wl-copy") || command_exists("xclip") || command_exists("xsel"),
            },
            "headless": {
                "click": true,
                "setValue": true,
                "scroll": false,
                "setState": true,
                "scrollTo": true,
                "selectOption": true,
            },
            // AT-SPI and the X11 connection are used from one thread at a
            // time, so agents' operations take turns here.
            "multiAgent": {
                "maxAgents": 16,
                "concurrentObservation": false,
                "concurrentHeadless": false,
                "physicalInput": "one at a time",
            },
            "observation": {
                "screenshot": self.x11 || (self.wayland && HAVE_PIPEWIRE),
                "accessibility": true,
                "jpeg": true,
                "png": true,
                "maxEdge": 0,
                "capturePersistent": true,
            },
            "dragDefaults": {
                "durationMs": DEFAULT_DRAG_DURATION_MS,
                "steps": DEFAULT_DRAG_STEPS,
                "holdBeforeMs": DEFAULT_HOLD_BEFORE_MS,
                "holdAfterMs": DEFAULT_HOLD_AFTER_MS,
            },
            "wayland": {
                "portalScreenCast": self.wayland && HAVE_PIPEWIRE,
                "portalRemoteDesktop": self.wayland,
                "libei": self.wayland && libei_runtime_available(),
                "legacyNotifyFallback": self.wayland,
            },
            "indicatorRequired": true,
            "stopKey": "Escape x2",
            "hotkeyStop": "Escape x2",
            "supported": indicator_available && (self.x11 || (self.wayland && HAVE_PIPEWIRE)),
        })
    }

    fn extension_windows(&self) -> Vec<WindowInfo> {
        if !self.shell().available() {
            return Vec::new();
        }
        let Ok(payload) = self.shell().list_windows() else {
            return Vec::new();
        };
        let Some(items) = payload.as_array() else {
            return Vec::new();
        };
        // A present field of the wrong type fails the whole list, as in
        // earlier releases.
        let parsed: Option<Vec<WindowInfo>> = items.iter().map(extension_window).collect();
        let mut windows: Vec<WindowInfo> = parsed
            .unwrap_or_default()
            .into_iter()
            .filter(|window| !window.id.is_empty() && window.width > 0 && window.height > 0)
            .collect();
        drop_unlisted_owners(&mut windows);
        windows
    }

    fn enumerate_windows(&self) -> Vec<WindowInfo> {
        let from_extension = self.extension_windows();
        if !from_extension.is_empty() {
            return from_extension;
        }
        let Some(display) = &self.display else {
            return Vec::new();
        };
        let active = display.active_window();
        let mut windows: Vec<WindowInfo> = display
            .client_windows()
            .into_iter()
            .filter(|window| display.is_viewable(*window))
            .map(|window| {
                let properties = display.window_properties(window);
                let geometry = properties.geometry.unwrap_or(x11::Geometry {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                });
                WindowInfo {
                    id: format!("x11:{window}"),
                    app: if properties.app.is_empty() {
                        "unknown".to_owned()
                    } else {
                        properties.app
                    },
                    title: properties.title,
                    pid: properties.pid,
                    x: geometry.x,
                    y: geometry.y,
                    width: geometry.width,
                    height: geometry.height,
                    active: window == active,
                    owner_id: properties
                        .transient_for
                        .map(|owner| format!("x11:{owner}"))
                        .unwrap_or_default(),
                    modal: properties.modal,
                }
            })
            .collect();
        drop_unlisted_owners(&mut windows);
        windows
    }

    fn list_windows(&self) -> Json {
        let windows: Vec<Json> = self
            .enumerate_windows()
            .iter()
            .map(WindowInfo::to_json)
            .collect();
        json!({"windows": windows})
    }

    fn list_apps(&self) -> Json {
        let mut apps: BTreeMap<String, Json> = BTreeMap::new();
        for window in self.enumerate_windows() {
            let app = apps
                .entry(window.app.clone())
                .or_insert_with(|| json!({"app": window.app, "pid": window.pid, "windows": []}));
            app["windows"]
                .as_array_mut()
                .expect("windows is an array")
                .push(json!(window.id));
            if !window.title.is_empty() {
                app["title"] = json!(window.title);
            }
        }
        json!({"apps": apps.into_values().collect::<Vec<_>>()})
    }

    fn select_window(&self, params: &Json) -> Result<WindowInfo, Error> {
        let requested_id = string_value(params, "windowId", "")?;
        let requested_app = string_value(params, "app", "")?;
        let windows = self.enumerate_windows();
        if !requested_id.is_empty() {
            return windows
                .into_iter()
                .find(|window| window.id == requested_id)
                .ok_or_else(|| {
                    Error::new(
                        "window_not_found",
                        "windowId does not identify a visible window",
                    )
                });
        }
        if !requested_app.is_empty() {
            let needle = requested_app.to_ascii_lowercase();
            let matches = |window: &WindowInfo| {
                window.app.to_ascii_lowercase() == needle
                    || window.title.to_ascii_lowercase() == needle
                    || (window.pid > 0 && needle == format!("pid:{}", window.pid))
            };
            // An app selects its main window before its dialogs; an
            // observation of the main window reports a blocking modal.
            for owned in [false, true] {
                if let Some(window) = windows
                    .iter()
                    .find(|window| window.owner_id.is_empty() != owned && matches(window))
                {
                    return Ok(window.clone());
                }
            }
            return Err(Error::new(
                "window_not_found",
                "app does not identify a visible window",
            ));
        }
        windows
            .iter()
            .find(|window| window.active)
            .or_else(|| windows.first())
            .cloned()
            .ok_or_else(|| Error::new("window_not_found", "No visible desktop windows were found"))
    }

    fn blocking_modal(&self, target: &WindowInfo) -> Option<WindowInfo> {
        let windows = self.enumerate_windows();
        let relations: Vec<WindowRelation> = windows
            .iter()
            .map(|window| WindowRelation {
                id: window.id.clone(),
                owner_id: window.owner_id.clone(),
                modal: window.modal,
            })
            .collect();
        find_blocking_modal(&relations, &target.id).map(|index| windows[index].clone())
    }

    fn reject_blocked_target(&self, params: &Json) -> Result<(), Error> {
        let target = self.select_window(params)?;
        let Some(modal) = self.blocking_modal(&target) else {
            return Ok(());
        };
        Err(Error::new(
            "modal_active",
            format!(
                "Window {} is blocked by modal dialog {} \"{}\"; observe and act on --window-id {} instead",
                target.id, modal.id, modal.title, modal.id
            ),
        ))
    }

    fn current_window(&self, expected: &WindowInfo) -> Result<WindowInfo, Error> {
        self.enumerate_windows()
            .into_iter()
            .find(|window| window.id == expected.id)
            .ok_or_else(|| Error::new("window_not_found", "The target window no longer exists"))
    }

    fn validate_observation(&mut self, params: &Json, window: &WindowInfo) -> Result<(), Error> {
        self.refresh_coordinate_state();
        let id = string_value(params, "observationId", "")?;
        if id.is_empty() {
            return Ok(());
        }
        let record = self.observations.get(&id).ok_or_else(|| {
            Error::new("stale_observation", "Unknown observationId; observe again")
        })?;
        if record.owner != string_value(params, "agentSessionId", "")? {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to another agent session; observe again",
            ));
        }
        if !record.window.same_geometry(window) {
            return Err(Error::new(
                "stale_observation",
                "Target window geometry changed; observe again",
            ));
        }
        if record.coordinate_revision != self.coordinate_revision {
            return Err(Error::new(
                "stale_observation",
                "Display geometry or capture mapping changed; observe again",
            ));
        }
        Ok(())
    }

    fn store_observation(&mut self, id: String, record: ObservationRecord) {
        self.observations.insert(id, record);
        // Evict the oldest observations first; their screenshots go with them.
        // Several agents share this cache, so it keeps more than one agent needs.
        while self.observations.len() > MAX_OBSERVATIONS {
            let oldest = self
                .observations
                .iter()
                .min_by_key(|(_, record)| record.created)
                .map(|(id, _)| id.clone())
                .expect("the cache is not empty");
            if let Some(record) = self.observations.remove(&oldest) {
                record.remove_files();
            }
        }
    }

    fn clear_observations(&mut self) {
        for record in self.observations.values() {
            record.remove_files();
        }
        self.observations.clear();
    }

    fn coordinate_transform(
        &self,
        params: &Json,
        window: &WindowInfo,
    ) -> Result<CoordinateTransform, Error> {
        let full = string_value(params, "coords", "reduced")? == "full";
        let id = string_value(params, "observationId", "")?;
        let source = if !id.is_empty() {
            // validate_observation has already matched this id to the window.
            Some(self.observations.get(&id).ok_or_else(|| {
                Error::new("stale_observation", "Unknown observationId; observe again")
            })?)
        } else {
            // Without an observationId, pixel coordinates refer to the most
            // recent screenshot this agent took of this window.
            let owner = string_value(params, "agentSessionId", "")?;
            let latest = self
                .observations
                .values()
                .filter(|record| {
                    record.has_screenshot && record.window.id == window.id && record.owner == owner
                })
                .max_by_key(|record| record.created);
            if let Some(record) = latest {
                let resized =
                    record.window.width != window.width || record.window.height != window.height;
                if resized || record.coordinate_revision != self.coordinate_revision {
                    return Err(Error::new(
                        "stale_observation",
                        "Window geometry changed since the last screenshot; observe again",
                    ));
                }
            }
            latest
        };
        let Some(source) = source.filter(|record| record.has_screenshot) else {
            return Err(Error::new(
                "observation_required",
                "Observe the window with a screenshot before clicking by coordinates",
            ));
        };
        Ok(if full {
            CoordinateTransform {
                space: "full",
                image: source.full.clone(),
            }
        } else {
            CoordinateTransform {
                space: "reduced",
                image: source.reduced.clone(),
            }
        })
    }

    fn full_screenshot(&mut self, params: &Json) -> Result<Json, Error> {
        self.ensure_active(params)?;
        self.refresh_coordinate_state();
        let id = string_value(params, "observationId", "")?;
        if id.is_empty() {
            return Err(Error::new(
                "observation_required",
                "get-full-screenshot requires observationId",
            ));
        }
        let record = self.observations.get(&id).ok_or_else(|| {
            Error::new(
                "stale_observation",
                "Unknown or expired observationId; observe again",
            )
        })?;
        if record.owner != string_value(params, "agentSessionId", "")? {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to another agent session; observe again",
            ));
        }
        let window_id = string_value(params, "windowId", "")?;
        if !window_id.is_empty() && window_id != record.window.id {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to a different window; observe again",
            ));
        }
        if !record.has_screenshot || !readable(&record.full.path) {
            return Err(Error::new(
                "stale_observation",
                "Observation has no full screenshot; observe again with a screenshot",
            ));
        }
        Ok(json!({
            "observationId": id,
            "screenshot": {
                "path": record.full.path,
                "mimeType": record.mime,
                "width": record.full.width,
                "height": record.full.height,
                "variant": "full",
                "actionTransform": record.full.action_transform(),
            },
            "notice": "To click a point read from this image, pass --coords full.",
        }))
    }

    fn join_monitors(&mut self) {
        self.shared.stop_monitor_stop.store(true, Ordering::Release);
        for monitor in [
            self.stop_monitor_thread.take(),
            self.heartbeat_thread.take(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = monitor.join();
        }
    }

    fn session_start(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        if self.shared.session_active.load(Ordering::Acquire) {
            return Ok(json!({
                "sessionId": self.session_id,
                "ready": true,
                "reused": true,
                "indicator": {"ready": true, "sessionId": self.session_id},
            }));
        }
        context.check()?;

        // Escape or a disconnected session bus can end a previous session
        // while the common dispatcher is between requests. Join both monitor
        // threads before changing the session context so a late callback
        // cannot cancel a newly started session.
        self.shared.heartbeat_stop.store(true, Ordering::Release);
        self.join_monitors();

        // A stop signal can make the session inactive before the dispatcher
        // gets a chance to run session.stop. Tear down that old authorization
        // before starting a new one; otherwise the next session could reuse an
        // EIS fd or ScreenCast stream that belongs to the previous indicator
        // session.
        self.release_inputs();
        #[cfg(feature = "pipewire")]
        {
            self.pipewire = None;
        }
        self.close_portal_sessions();

        let requested_id = string_value(params, "sessionId", &uuid()?)?;
        if requested_id.is_empty() {
            return Err(Error::invalid_argument("sessionId must not be empty"));
        }
        if !self.shell().available() {
            return Err(Error::new(
                "setup_required",
                "Enable the GNOME desktop-computer-use extension in the logged-in session",
            ));
        }
        let indicator = self.shell().call_string("Start", &requested_id, 5000)?;
        if !indicator
            .get("ready")
            .and_then(Json::as_bool)
            .unwrap_or(false)
        {
            return Err(Error::new(
                "setup_required",
                "GNOME extension did not report a ready indicator",
            ));
        }

        self.session_id = requested_id;
        *self.shared.active_context.lock() = Some(context.clone());
        self.shared.session_active.store(true, Ordering::Release);
        self.shared.interrupted.store(false, Ordering::Release);
        self.last_activity = Instant::now();
        self.shared.heartbeat_stop.store(false, Ordering::Release);
        self.heartbeat_thread = Some(spawn_heartbeat(Arc::clone(&self.shared)));
        self.shared
            .stop_monitor_stop
            .store(false, Ordering::Release);
        self.stop_monitor_thread = Some(spawn_stop_monitor(Arc::clone(&self.shared)));
        let platform = if self.wayland {
            "wayland"
        } else if self.x11 {
            "x11"
        } else {
            "unknown"
        };
        Ok(
            json!({"sessionId": self.session_id, "ready": true, "indicator": indicator, "platform": platform}),
        )
    }

    fn session_stop(&mut self, params: &Json) -> Result<Json, Error> {
        let requested = string_value(params, "sessionId", "")?;
        if !requested.is_empty() && requested != self.session_id {
            return Err(Error::new("session_owner", "Unknown sessionId"));
        }
        self.shared.heartbeat_stop.store(true, Ordering::Release);
        self.join_monitors();
        self.release_inputs();
        #[cfg(feature = "pipewire")]
        {
            self.pipewire = None;
        }
        self.close_portal_sessions();
        if self.shell().available() {
            let _ = self.shell().call_bool("Stop", 3000);
        }
        let old_session = std::mem::take(&mut self.session_id);
        self.shared.session_active.store(false, Ordering::Release);
        *self.shared.active_context.lock() = None;
        self.shared.interrupted.store(false, Ordering::Release);
        self.clear_observations();
        Ok(json!({"stopped": true, "sessionId": old_session}))
    }

    /// Resets every piece of session geometry even when D-Bus has already
    /// disappeared. Keeping stale EIS or stream state would make a later
    /// session send coordinates through an authorization that no longer owns
    /// the desktop.
    fn close_portal_sessions(&mut self) {
        if let Some(mut eis) = self.eis.take() {
            eis.close();
        }
        if self.portal.connection().is_some() && !self.remote.session.is_empty() {
            self.portal.close_session(&self.remote.session);
        }
        self.remote = RemoteDesktop::default();
    }

    fn portal_ready_for_input(&mut self, context: &Context) -> Result<bool, Error> {
        context.check()?;
        if !self.wayland {
            return Ok(true);
        }
        if self.remote.started && self.remote.stream.valid && self.remote.stream.node_id != u32::MAX
        {
            if self.remote.eis_selected && !self.eis.as_ref().is_some_and(EisClient::connected) {
                return Err(eis_disconnected());
            }
            return Ok(true);
        }
        if !self.remote.session.is_empty() || self.eis.is_some() {
            self.close_portal_sessions();
        }
        if !self.portal.available() {
            return Err(Error::new(
                "setup_required",
                "Wayland RemoteDesktop portal is unavailable",
            ));
        }

        // Keep all response-derived values local until the complete portal
        // and input transport setup succeeds. This makes a denied source,
        // cancelled permission request, or failed EIS handshake leave no
        // usable-looking partial session behind.
        let mut new_session = String::new();
        let mut new_eis = None;
        match self.start_remote_desktop(context, &mut new_session, &mut new_eis) {
            Ok((stream, eis_selected)) => {
                self.remote = RemoteDesktop {
                    session: new_session,
                    started: true,
                    eis_selected,
                    stream,
                };
                self.eis = new_eis;
                Ok(true)
            }
            Err(error) => {
                if let Some(mut eis) = new_eis {
                    eis.close();
                }
                if !new_session.is_empty() {
                    self.portal.close_session(&new_session);
                }
                self.remote = RemoteDesktop::default();
                if let Some(mut eis) = self.eis.take() {
                    eis.close();
                }
                Err(error)
            }
        }
    }

    fn start_remote_desktop(
        &self,
        context: &Context,
        new_session: &mut String,
        new_eis: &mut Option<EisClient>,
    ) -> Result<(PortalStream, bool), Error> {
        let request_token = format!("dcu_{}", uuid()?);
        let session_token = format!("dcu_session_{}", uuid()?);
        *new_session = self
            .portal
            .create_session(&request_token, &session_token, Some(context))?;
        let session = object_path(new_session)?;
        let portal_timeout = Duration::from_millis(30000);

        let devices = HashMap::from([("types", Value::U32(3))]);
        self.portal.request_dict(
            "org.freedesktop.portal.RemoteDesktop",
            "SelectDevices",
            &(&session, devices),
            Some(context),
            |_| {},
            portal_timeout,
        )?;

        // The ScreenCast source selection is tied to the same portal session,
        // so the returned stream and RemoteDesktop input transport describe
        // one coordinate space.
        let sources = HashMap::from([
            ("types", Value::U32(3)),
            ("multiple", Value::Bool(false)),
            ("cursor_mode", Value::U32(2)),
        ]);
        self.portal.request_dict(
            "org.freedesktop.portal.ScreenCast",
            "SelectSources",
            &(&session, sources),
            Some(context),
            |_| {},
            portal_timeout,
        )?;

        let mut stream = PortalStream::default();
        self.portal.request_dict(
            "org.freedesktop.portal.RemoteDesktop",
            "Start",
            &(&session, "", empty_options()),
            Some(context),
            |values| stream = first_stream(values),
            portal_timeout,
        )?;
        if !stream.valid || stream.node_id == u32::MAX {
            return Err(Error::new(
                "capture_unavailable",
                "RemoteDesktop returned no valid ScreenCast stream",
            ));
        }

        // EIS is preferred when both the portal and the runtime expose it. A
        // failed ConnectToEIS request means no EIS fd was authorized, so the
        // legacy Notify* transport remains valid. Once an fd is returned,
        // however, a handshake failure is fatal and cannot fall back safely.
        let portal_supports_eis = match self
            .portal
            .interface_version("org.freedesktop.portal.RemoteDesktop", Some(context))
        {
            Ok(version) => version >= 2,
            Err(error) if error.code == "cancelled" => return Err(error),
            Err(_) => false,
        };
        let mut eis_selected = false;
        if libei_runtime_available() && portal_supports_eis {
            let descriptor = match self.portal.connect_to_eis(new_session, Some(context)) {
                Ok(descriptor) => Some(descriptor),
                Err(error) if error.code == "cancelled" => return Err(error),
                // No descriptor was returned, so this is an older or
                // unavailable EIS portal and Notify* is the supported path.
                Err(_) => None,
            };
            if let Some(descriptor) = descriptor {
                let client = new_eis.insert(EisClient::new());
                client.connect(descriptor.as_fd(), context)?;
                eis_selected = true;
            }
        }
        Ok((stream, eis_selected))
    }

    fn portal_notify<B>(&self, method: &str, body: &B) -> Result<(), Error>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
    {
        let connection = self
            .portal
            .connection()
            .ok_or_else(|| Error::new("setup_required", "D-Bus session bus is unavailable"))?;
        portal_bus::call(
            connection,
            PORTAL_BUS_NAME,
            PORTAL_DESKTOP_PATH,
            "org.freedesktop.portal.RemoteDesktop",
            method,
            body,
            INPUT_NOTIFY_TIMEOUT,
            None,
        )
        .and_then(|reply| portal_bus::expect_reply_type(reply, method, "()"))
        .map(|_| ())
        .map_err(|message| Error::new("input_unavailable", message))
    }

    /// Prepares a legacy Notify* call: false when this is no Wayland session.
    fn portal_notify_ready(&mut self, context: &Context, input: &str) -> Result<bool, Error> {
        context.check()?;
        if !self.portal_ready_for_input(context)? {
            return Ok(false);
        }
        if self.remote.eis_selected {
            return Err(Error::new(
                "eis_disconnected",
                format!("The authorized EIS session must carry {input} input"),
            ));
        }
        Ok(true)
    }

    fn portal_notify_motion_absolute(
        &mut self,
        x: f64,
        y: f64,
        target: Option<&WindowInfo>,
        context: &Context,
    ) -> Result<bool, Error> {
        if !self.portal_notify_ready(context, "absolute pointer")? {
            return Ok(false);
        }
        let stream = self.remote.stream.clone();
        if !stream.valid || stream.node_id == u32::MAX {
            return Err(Error::new(
                "capture_unavailable",
                "Wayland absolute input has no ScreenCast stream",
            ));
        }
        let mut stream_x = x - f64::from(stream.position_x);
        let mut stream_y = y - f64::from(stream.position_y);
        let has_stream_size = stream.compositor_width > 0 && stream.compositor_height > 0;
        if !has_stream_size && let Some(target) = target {
            stream_x = x - f64::from(target.x);
            stream_y = y - f64::from(target.y);
        }
        let logical_extent = |logical: i32, compositor: i32, window: Option<i32>| {
            if logical > 0 {
                f64::from(logical)
            } else if compositor > 0 {
                f64::from(compositor)
            } else {
                f64::from(window.unwrap_or(0))
            }
        };
        let logical_width = logical_extent(
            stream.logical_width,
            stream.compositor_width,
            target.map(|t| t.width),
        );
        let logical_height = logical_extent(
            stream.logical_height,
            stream.compositor_height,
            target.map(|t| t.height),
        );
        if let Some(target) =
            target.filter(|_| stream.compositor_width <= 0 && stream.compositor_height <= 0)
        {
            stream_x = x - f64::from(target.x);
            stream_y = y - f64::from(target.y);
        } else if has_stream_size && stream.logical_width > 0 && stream.logical_height > 0 {
            stream_x *= logical_width / f64::from(stream.compositor_width);
            stream_y *= logical_height / f64::from(stream.compositor_height);
        }
        let below_one = 1.0f64.next_down();
        let normalize = |value: f64, extent: f64| {
            if extent > 0.0 {
                (value / extent).clamp(0.0, below_one)
            } else {
                0.0
            }
        };
        let session = object_path(&self.remote.session)?;
        let body = (
            session,
            empty_options(),
            stream.node_id,
            normalize(stream_x, logical_width),
            normalize(stream_y, logical_height),
        );
        self.portal_notify("NotifyPointerMotionAbsolute", &body)?;
        Ok(true)
    }

    fn portal_notify_button(
        &mut self,
        button: &str,
        pressed: bool,
        context: &Context,
    ) -> Result<bool, Error> {
        if !self.portal_notify_ready(context, "button")? {
            return Ok(false);
        }
        // RemoteDesktop uses Linux evdev button codes, not X11 button numbers.
        let code = evdev_button(button) as i32;
        let session = object_path(&self.remote.session)?;
        self.portal_notify(
            "NotifyPointerButton",
            &(session, empty_options(), code, u32::from(pressed)),
        )?;
        Ok(true)
    }

    fn portal_notify_axis(&mut self, dx: f64, dy: f64, context: &Context) -> Result<bool, Error> {
        if !self.portal_notify_ready(context, "scroll")? {
            return Ok(false);
        }
        let session = object_path(&self.remote.session)?;
        self.portal_notify("NotifyPointerAxis", &(session, empty_options(), dx, dy))?;
        Ok(true)
    }

    fn portal_notify_key(
        &mut self,
        keysym: u32,
        pressed: bool,
        context: &Context,
    ) -> Result<bool, Error> {
        if !self.portal_notify_ready(context, "keyboard")? {
            return Ok(false);
        }
        let session = object_path(&self.remote.session)?;
        self.portal_notify(
            "NotifyKeyboardKeysym",
            &(session, empty_options(), keysym as i32, u32::from(pressed)),
        )?;
        Ok(true)
    }

    /// The EIS client of a session that selected EIS, or None for the legacy
    /// Notify* transport.
    fn selected_eis(&mut self) -> Result<Option<&mut EisClient>, Error> {
        if self.remote.eis_selected {
            return match self.eis.as_mut().filter(|eis| eis.connected()) {
                Some(eis) => Ok(Some(eis)),
                None => Err(eis_disconnected()),
            };
        }
        if self.eis.as_ref().is_some_and(EisClient::connected) {
            return Err(Error::new(
                "protocol_error",
                "Connected EIS transport is missing its session selection state",
            ));
        }
        Ok(None)
    }

    fn require_display(&self) -> Result<&X11Display, Error> {
        self.display
            .as_ref()
            .ok_or_else(|| Error::new("input_unavailable", "X11 display is unavailable"))
    }

    fn activate(&self, window: &WindowInfo) -> Result<(), Error> {
        if self.shell().available() {
            return self.shell().activate(&window.id);
        }
        let x11_window = window
            .id
            .strip_prefix("x11:")
            .and_then(|id| id.parse::<u32>().ok());
        if let (Some(display), Some(x11_window)) = (&self.display, x11_window) {
            display.raise_and_focus(x11_window);
            return Ok(());
        }
        Err(Error::new(
            "input_unavailable",
            "The target window could not be activated",
        ))
    }

    fn move_pointer(
        &mut self,
        x: i32,
        y: i32,
        context: &Context,
        target: Option<&WindowInfo>,
    ) -> Result<(), Error> {
        context.check()?;
        if self.wayland {
            self.portal_ready_for_input(context)?;
            match self.selected_eis()? {
                Some(eis) => eis.absolute_global(f64::from(x), f64::from(y), context)?,
                None => {
                    self.portal_notify_motion_absolute(
                        f64::from(x),
                        f64::from(y),
                        target,
                        context,
                    )?;
                }
            }
            if self.shell().available() {
                self.shell().pointer(f64::from(x), f64::from(y))?;
            }
            self.mark_input_complete();
            return Ok(());
        }
        let display = self.require_display()?;
        display.fake_motion(x, y);
        display.flush();
        self.mark_input_complete();
        Ok(())
    }

    fn button_event(
        &mut self,
        button: &str,
        pressed: bool,
        context: &Context,
    ) -> Result<(), Error> {
        if pressed {
            context.check()?;
        }
        if self.wayland {
            self.portal_ready_for_input(context)?;
            match self.selected_eis()? {
                Some(eis) => eis.button(evdev_button(button), pressed, context)?,
                None => {
                    self.portal_notify_button(button, pressed, context)?;
                }
            }
            self.mark_input_complete();
            return Ok(());
        }
        let display = self.require_display()?;
        display.fake_button(x11_button(button), pressed);
        display.flush();
        self.mark_input_complete();
        Ok(())
    }

    fn scroll_event(
        &mut self,
        amount: i32,
        direction: &str,
        context: &Context,
    ) -> Result<(), Error> {
        context.check()?;
        let sign = if direction == "up" || direction == "left" {
            -1
        } else {
            1
        };
        let horizontal = direction == "left" || direction == "right";
        let (dx, dy) = if horizontal {
            (f64::from(sign), 0.0)
        } else {
            (0.0, f64::from(sign))
        };
        for _ in 0..amount.unsigned_abs().max(1) {
            context.check()?;
            if self.wayland {
                self.portal_ready_for_input(context)?;
                match self.selected_eis()? {
                    Some(eis) => eis.scroll(dx, dy, context)?,
                    None => {
                        self.portal_notify_axis(dx, dy, context)?;
                    }
                }
            } else {
                let button = match (horizontal, sign < 0) {
                    (false, true) => 4,
                    (false, false) => 5,
                    (true, true) => 6,
                    (true, false) => 7,
                };
                let display = self.require_display()?;
                display.fake_button(button, true);
                display.fake_button(button, false);
                display.flush();
            }
            self.mark_input_complete();
            thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    }

    /// GNOME's stop accelerator cannot tell injected input from the keyboard,
    /// so an injected Escape would be swallowed and counted toward the user's
    /// emergency stop. Refuse the action rather than risk either outcome.
    fn suspend_stop_key_for_escape(&self) -> Result<(), Error> {
        let reason = match self.shell().suspend_stop_key(STOP_KEY_SUSPEND_MS) {
            Ok(true) => return Ok(()),
            Ok(false) => "the GNOME extension reported no active session".to_owned(),
            Err(error) => error.message,
        };
        Err(Error::new(
            "stop_key_conflict",
            format!(
                "Escape is the emergency stop key and could not be released to the application ({reason}); run \
                 `dcu setup` to update the GNOME extension, then log in again so GNOME Shell loads it"
            ),
        ))
    }

    fn key_event(&mut self, key: &str, pressed: bool, context: &Context) -> Result<(), Error> {
        if pressed {
            context.check()?;
        }
        let canonical = canonical_key(key);
        let unknown_key = || Error::invalid_argument(format!("Unknown key: {key}"));
        // Suspend immediately before injection so portal setup cannot consume
        // the suspension window.
        let injects_escape_press = pressed && canonical == "escape";
        if self.wayland {
            self.portal_ready_for_input(context)?;
            if self.selected_eis()?.is_some() {
                let code = evdev_keycode(&canonical);
                if code == 0 {
                    return Err(unknown_key());
                }
                if injects_escape_press {
                    self.suspend_stop_key_for_escape()?;
                }
                self.selected_eis()?
                    .expect("EIS was selected above")
                    .key(code, pressed, context)?;
            } else {
                let keysym = common_keysym(&canonical);
                if keysym == 0 {
                    return Err(unknown_key());
                }
                if injects_escape_press {
                    self.suspend_stop_key_for_escape()?;
                }
                self.portal_notify_key(keysym, pressed, context)?;
            }
            self.mark_input_complete();
            return Ok(());
        }
        let display = self.require_display()?;
        let keysym = x11::keysym(&canonical);
        if keysym == 0 {
            return Err(unknown_key());
        }
        let keycode = display.keycode_for(keysym);
        if keycode == 0 {
            return Err(Error::invalid_argument(format!(
                "Key has no X11 keycode: {key}"
            )));
        }
        if injects_escape_press {
            self.suspend_stop_key_for_escape()?;
        }
        let display = self.require_display()?;
        display.fake_key(keycode, pressed);
        display.flush();
        self.mark_input_complete();
        Ok(())
    }

    fn hotkey_event(&mut self, keys: &[String], context: &Context) -> Result<(), Error> {
        if keys.is_empty() {
            return Err(Error::invalid_argument("hotkey requires at least one key"));
        }
        let (modifiers, regular): (Vec<String>, Vec<String>) = keys
            .iter()
            .map(|key| canonical_key(key))
            .partition(|key| is_modifier(key));
        let [key] = regular.as_slice() else {
            return Err(Error::invalid_argument(
                "hotkey requires exactly one non-modifier key",
            ));
        };
        self.require_not_toggled(key)?;
        let release_context = Context::new();
        let mut pressed = Vec::new();
        let outcome = self.press_hotkey(&modifiers, key, context, &release_context, &mut pressed);
        if outcome.is_err() {
            for modifier in pressed.iter().rev() {
                let _ = self.key_event(modifier, false, &release_context);
            }
        }
        outcome
    }

    fn press_hotkey(
        &mut self,
        modifiers: &[String],
        key: &str,
        context: &Context,
        release_context: &Context,
        pressed: &mut Vec<String>,
    ) -> Result<(), Error> {
        for modifier in modifiers {
            if self.toggled.contains(modifier) {
                continue;
            }
            self.key_event(modifier, true, context)?;
            pressed.push(modifier.clone());
        }
        self.key_event(key, true, context)?;
        self.key_event(key, false, release_context)?;
        for modifier in pressed.iter().rev() {
            self.key_event(modifier, false, release_context)?;
        }
        Ok(())
    }

    fn type_text_impl(&mut self, text: &str, context: &Context) -> Result<(), Error> {
        if !self.toggled.is_empty() {
            // Keycode typing is changed by any held key (Shift alters case,
            // Ctrl, Alt and Super turn text into shortcuts).
            let names: Vec<&str> = self
                .toggled
                .iter()
                .map(|key| if key == "super" { "win" } else { key.as_str() })
                .collect();
            return Err(Error::new(
                "toggles_active",
                format!(
                    "type-text is refused while {} is toggled on; release it with `dcu toggle off --all` first or \
                     use paste-text",
                    names.join(", ")
                ),
            ));
        }
        for byte in text.bytes() {
            context.check()?;
            let key = match byte {
                0x20..=0x7e => char::from(byte).to_string(),
                b'\n' => "enter".to_owned(),
                b'\t' => "tab".to_owned(),
                _ => {
                    return Err(Error::new(
                        "unsupported",
                        "type-text currently accepts printable ASCII, tab, and newline; use paste-text for Unicode",
                    ));
                }
            };
            self.key_event(&key, true, context)?;
            self.key_event(&key, false, context)?;
        }
        Ok(())
    }

    fn release_inputs(&mut self) {
        // EIS tracks and releases every key it pressed, toggled keys included.
        if let Some(eis) = &mut self.eis {
            eis.release();
        }
        if self.wayland && self.remote.started && !self.remote.eis_selected {
            // The legacy Notify* transport keeps no key state of its own.
            for key in self.toggled.clone() {
                let _ = self.portal_notify_key(common_keysym(&key), false, &Context::new());
            }
        }
        if let Some(display) = self.display.as_ref().filter(|_| !self.wayland) {
            for button in 1..=3 {
                display.fake_button(button, false);
            }
            let space = self.toggled.contains("space").then_some(x11::XK_SPACE);
            for keysym in [
                x11::XK_CONTROL_L,
                x11::XK_SHIFT_L,
                x11::XK_ALT_L,
                x11::XK_SUPER_L,
            ]
            .into_iter()
            .chain(space)
            {
                let keycode = display.keycode_for(keysym);
                if keycode != 0 {
                    display.fake_key(keycode, false);
                }
            }
            display.flush();
        }
        self.toggled.clear();
    }

    fn release_toggled_keys(&mut self) {
        for key in std::mem::take(&mut self.toggled) {
            let _ = self.key_event(&key, false, &Context::new());
        }
    }

    fn require_not_toggled(&self, key: &str) -> Result<(), Error> {
        let canonical = canonical_key(key);
        if !self.toggled.contains(&canonical) {
            return Ok(());
        }
        let name = if canonical == "super" {
            "win"
        } else {
            canonical.as_str()
        };
        Err(Error::new(
            "toggles_active",
            format!(
                "{name} is toggled on; pressing it again would release it. Use `dcu toggle off --key {name}` instead"
            ),
        ))
    }

    fn set_toggle(&mut self, key: &str, down: bool, context: &Context) -> Result<(), Error> {
        if !self.shared.session_active.load(Ordering::Acquire) {
            return Err(session_required());
        }
        if self.shared.interrupted.load(Ordering::Acquire) {
            return Err(session_interrupted());
        }
        let canonical = canonical_key(key);
        if down {
            // Track before the press so every release path includes the key.
            self.toggled.insert(canonical.clone());
            if let Err(error) = self.key_event(&canonical, true, context) {
                let _ = self.key_event(&canonical, false, &Context::new());
                self.toggled.remove(&canonical);
                return Err(error);
            }
        } else {
            self.key_event(&canonical, false, &Context::new())?;
            self.toggled.remove(&canonical);
        }
        self.last_activity = Instant::now();
        Ok(())
    }

    fn coordinate_signature(&self) -> String {
        let mut signature = String::from(if self.wayland {
            "wayland"
        } else if self.x11 {
            "x11"
        } else {
            "none"
        });
        if let Some(display) = &self.display {
            let (width, height) = display.root_size();
            signature.push_str(&format!(":root={width}x{height}"));
            for crtc in display.crtcs() {
                signature.push_str(&format!(
                    ":crtc={},{},{},{},{},{}",
                    crtc.id, crtc.x, crtc.y, crtc.width, crtc.height, crtc.rotation
                ));
            }
        }
        let stream = &self.remote.stream;
        signature.push_str(&format!(
            ":portal={},{},{},{},{},{},{},{},{},{}",
            self.remote.session,
            stream.node_id,
            stream.pipewire_serial,
            stream.mapping_id,
            stream.position_x,
            stream.position_y,
            stream.compositor_width,
            stream.compositor_height,
            stream.logical_width,
            stream.logical_height
        ));
        #[cfg(feature = "pipewire")]
        if let Some(capture) = &self.pipewire {
            signature.push_str(&format!(":framegen={}", capture.coordinate_generation()));
        }
        if let Some(eis) = &self.eis {
            signature.push_str(&format!(
                ":eis={},{}",
                u8::from(eis.connected()),
                u8::from(eis.ready())
            ));
            for region in eis.regions() {
                signature.push_str(&format!(
                    ":{},{:?},{:?},{:?},{:?}",
                    region.mapping_id, region.x, region.y, region.width, region.height
                ));
            }
        }
        signature
    }

    fn refresh_coordinate_state(&mut self) {
        let current = self.coordinate_signature();
        if self.coordinate_signature.is_empty() {
            self.coordinate_signature = current;
            self.coordinate_revision = 1;
            return;
        }
        if current == self.coordinate_signature {
            return;
        }
        self.coordinate_signature = current;
        self.coordinate_revision += 1;
        self.clear_observations();
    }

    fn mark_input_complete(&self) {
        #[cfg(feature = "pipewire")]
        if let Some(capture) = &self.pipewire {
            capture.mark_input_complete();
        }
    }

    fn capture_window(
        &mut self,
        window: &WindowInfo,
        context: &Context,
    ) -> Result<RgbaImage, Error> {
        context.check()?;
        if self.wayland {
            return self.capture_wayland(window, context);
        }
        let Some(display) = self.display.as_ref().filter(|_| self.x11) else {
            return Err(Error::new(
                "capture_unavailable",
                "No supported Linux capture backend is available",
            ));
        };
        if window.width <= 0 || window.height <= 0 {
            return Err(Error::new(
                "capture_unavailable",
                "X11 window geometry is unavailable",
            ));
        }
        let capture = display
            .capture(window.x, window.y, window.width, window.height)
            .ok_or_else(|| {
                Error::new(
                    "capture_unavailable",
                    "XGetImage could not capture the target window",
                )
            })?;
        let mut image = RgbaImage::new(capture.width, capture.height, capture.rgba);
        image.fresh_frame = true;
        Ok(image)
    }

    #[cfg(feature = "pipewire")]
    fn capture_wayland(
        &mut self,
        window: &WindowInfo,
        context: &Context,
    ) -> Result<RgbaImage, Error> {
        let capture = self.pipewire.get_or_insert_with(PipeWireCapture::new);
        if !capture.started() {
            self.portal_ready_for_input(context)?;
            let stream = self.remote.stream.clone();
            if !stream.valid || stream.node_id == u32::MAX {
                return Err(Error::new(
                    "capture_unavailable",
                    "RemoteDesktop did not return a ScreenCast stream",
                ));
            }
            let remote = self
                .portal
                .open_pipewire_remote(&self.remote.session, Some(context))?;
            let capture = self.pipewire.as_mut().expect("created above");
            capture.start(
                remote,
                stream.node_id,
                stream.pipewire_serial,
                window.width,
                window.height,
            )?;
        }
        let stream = &self.remote.stream;
        let origin = if stream.compositor_width <= 0 || stream.compositor_height <= 0 {
            (window.x, window.y)
        } else {
            (stream.position_x, stream.position_y)
        };
        let capture = self.pipewire.as_ref().expect("created above");
        capture.capture(
            [window.x, window.y, window.width, window.height],
            origin,
            context,
        )
    }

    #[cfg(not(feature = "pipewire"))]
    fn capture_wayland(
        &mut self,
        _window: &WindowInfo,
        _context: &Context,
    ) -> Result<RgbaImage, Error> {
        Err(Error::new(
            "capture_unavailable",
            "Wayland capture requires PipeWire support",
        ))
    }

    /// The accessibility bus connection, reopened when the bus went away,
    /// as libatspi does.
    fn accessibility(&mut self) -> Option<Connection> {
        let alive = self.accessibility.as_ref().is_some_and(|connection| {
            portal_bus::call(
                connection,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.Peer",
                "Ping",
                &(),
                Duration::from_millis(800),
                None,
            )
            .is_ok()
        });
        if !alive {
            self.accessibility = atspi::connect_accessibility_bus();
        }
        self.accessibility.clone()
    }

    fn accessibility_snapshot(
        &mut self,
        window: &WindowInfo,
        context: &Context,
    ) -> Result<Json, Error> {
        context.check()?;
        let Some(connection) = self.accessibility() else {
            return Err(window_not_on_atspi_bus());
        };
        atspi_actions::snapshot(&connection, window, context)
    }

    fn element_for_index(
        &mut self,
        window: &WindowInfo,
        params: &Json,
        context: &Context,
    ) -> Result<Accessible, Error> {
        let requested = integer(params, "elementIndex", -1)?;
        if requested < 0 {
            return Err(Error::invalid_argument("elementIndex must be non-negative"));
        }
        if string_value(params, "observationId", "")?.is_empty() {
            return Err(Error::invalid_argument(
                "element actions require observationId",
            ));
        }
        let Some(connection) = self.accessibility() else {
            return Err(window_not_on_atspi_bus());
        };
        atspi_actions::element_for_index(&connection, window, requested as usize, context)
    }

    /// The window-local center of the element's drawn rectangle.
    fn element_center(
        &mut self,
        window: &WindowInfo,
        params: &Json,
        context: &Context,
    ) -> Result<Option<(i32, i32)>, Error> {
        let element = self.element_for_index(window, params, context)?;
        Ok(atspi_actions::drawn_rect(&element)
            .map(|[x, y, width, height]| (x - window.x + width / 2, y - window.y + height / 2)))
    }

    fn make_observation(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        context.check()?;
        let window = self.select_window(params)?;
        self.refresh_coordinate_state();
        if !string_value(params, "observationId", "")?.is_empty() {
            self.validate_observation(params, &window)?;
        }
        let mut record = ObservationRecord {
            owner: string_value(params, "agentSessionId", "")?,
            window: window.clone(),
            created: Instant::now(),
            coordinate_revision: 0,
            has_screenshot: false,
            mime: String::new(),
            reduced: ObservationImage::default(),
            full: ObservationImage::default(),
        };
        let observation_id = uuid()?;
        match self.observe(params, &window, &observation_id, &mut record, context) {
            Ok(observation) => {
                self.store_observation(observation_id, record);
                Ok(observation)
            }
            Err(error) => {
                // Images written before the failure are not cached.
                record.remove_files();
                Err(error)
            }
        }
    }

    fn observe(
        &mut self,
        params: &Json,
        window: &WindowInfo,
        observation_id: &str,
        record: &mut ObservationRecord,
        context: &Context,
    ) -> Result<Json, Error> {
        let include_screenshot = flag(params, "includeScreenshot", true);
        let include_text = flag(params, "includeText", false);
        let started = Instant::now();
        let mut observation = json!({
            "observationId": observation_id,
            "capturedAt": utc_timestamp(SystemTime::now()),
            "window": window.to_json(),
            "coordinateSpace": "window",
            "coordinateRevision": self.coordinate_revision,
            "timings": {},
            "overlayRegions": [],
        });
        if let Some(modal) = self.blocking_modal(window) {
            observation["modal"] =
                json!({"windowId": modal.id, "title": modal.title, "app": modal.app});
            observation["notice"] = json!(format!(
                "This window is blocked by modal dialog {} \"{}\". Its controls are not in this window's \
                 accessibility tree; observe and act on --window-id {} instead.",
                modal.id, modal.title, modal.id
            ));
        }
        if include_screenshot {
            self.observe_screenshot(
                params,
                window,
                observation_id,
                record,
                &mut observation,
                context,
            )?;
        }
        if include_text {
            let text_started = Instant::now();
            observation["accessibility"] = self.accessibility_snapshot(window, context)?;
            observation["timings"]["accessibilityMs"] = json!(elapsed_ms(text_started));
        }

        // Capture and accessibility traversal can take long enough for a
        // window to move or resize. Do not publish an observation whose image
        // or text is paired with geometry that is no longer true at
        // completion time.
        context.check()?;
        let final_window = self.current_window(window)?;
        context.check()?;
        if !final_window.same_geometry(window) {
            return Err(Error::new(
                "stale_observation",
                "Target window geometry changed during observation; observe again",
            ));
        }
        observation["timings"]["totalMs"] = json!(elapsed_ms(started));
        self.refresh_coordinate_state();
        observation["coordinateRevision"] = json!(self.coordinate_revision);
        record.created = Instant::now();
        record.coordinate_revision = self.coordinate_revision;
        Ok(observation)
    }

    fn observe_screenshot(
        &mut self,
        params: &Json,
        window: &WindowInfo,
        observation_id: &str,
        record: &mut ObservationRecord,
        observation: &mut Json,
        context: &Context,
    ) -> Result<(), Error> {
        let capture_started = Instant::now();
        let image = self.capture_window(window, context)?;
        let capture_ms = elapsed_ms(capture_started);
        if image.captured_at != UNIX_EPOCH {
            observation["capturedAt"] = json!(utc_timestamp(image.captured_at));
        }
        let format = string_value(params, "format", "jpeg")?.to_ascii_lowercase();
        let quality = integer(params, "quality", 85)?;
        // maxEdge is an optional cap on the reduced image only; 0 means no cap.
        let max_edge = integer(params, "maxEdge", 0)?;
        let extension = if format == "png" { ".png" } else { ".jpg" };
        // Capture already resamples to the logical window size, so
        // image-to-window ratios are normally 1; they are kept so the
        // transform stays exact.
        let window_scale_x = f64::from(image.width) / f64::from(window.width.max(1));
        let window_scale_y = f64::from(image.height) / f64::from(window.height.max(1));
        record.full = ObservationImage {
            path: self
                .image_directory
                .join(format!("{observation_id}-full{extension}")),
            width: image.width,
            height: image.height,
            scale_x: window_scale_x,
            scale_y: window_scale_y,
        };
        image::save(&image, &record.full.path, &format, quality)?;

        // Reduced image: 0.5x above 1280x720 (2x2 box average), otherwise 1x,
        // then optionally capped by maxEdge.
        let mut reduced = None;
        let (mut factor_x, mut factor_y) = (1.0, 1.0);
        if image.width > 1280 || image.height > 720 {
            reduced = Some(image::downscale_half(&image));
            (factor_x, factor_y) = (0.5, 0.5);
        }
        let current = reduced.as_ref().unwrap_or(&image);
        let longest = current.width.max(current.height);
        if max_edge > 0 && longest > max_edge {
            let cap = f64::from(max_edge) / f64::from(longest);
            let width = ((f64::from(current.width) * cap).round() as i32).max(1);
            let height = ((f64::from(current.height) * cap).round() as i32).max(1);
            factor_x *= f64::from(width) / f64::from(current.width);
            factor_y *= f64::from(height) / f64::from(current.height);
            reduced = Some(image::resize(current, width, height));
        }
        let reduced = reduced.as_ref().unwrap_or(&image);
        record.reduced = ObservationImage {
            path: self
                .image_directory
                .join(format!("{observation_id}{extension}")),
            width: reduced.width,
            height: reduced.height,
            scale_x: factor_x * window_scale_x,
            scale_y: factor_y * window_scale_y,
        };
        let mime = image::save(reduced, &record.reduced.path, &format, quality)?;
        record.has_screenshot = true;
        record.mime = mime.to_owned();
        observation["screenshot"] = json!({
            "path": record.reduced.path,
            "mimeType": mime,
            "width": record.reduced.width,
            "height": record.reduced.height,
            "variant": "reduced",
            "actionTransform": record.reduced.action_transform(),
            "fullAvailable": true,
            "fullWidth": record.full.width,
            "fullHeight": record.full.height,
            "scale": record.reduced.scale_x,
            "scaleX": record.reduced.scale_x,
            "scaleY": record.reduced.scale_y,
            "sourceWidth": image.width,
            "sourceHeight": image.height,
            "frameSequence": image.frame_sequence,
            "sourceFrameSequence": image.source_frame_sequence,
            "frameTimestampNs": image.frame_timestamp_ns,
            "frameGeneration": image.frame_generation,
            "freshFrame": image.fresh_frame,
            "cachedFrame": image.cached_frame,
        });
        observation["timings"]["captureMs"] = json!(capture_ms);

        // X11 root capture and a monitor-wide PipeWire stream both include
        // Shell actors. Report their screen-logical rectangles so callers can
        // avoid mistaking the activity indicator for application pixels.
        if (self.x11 || self.wayland) && self.shell().available() {
            observation["overlayRegionsSpace"] = json!("screen-logical");
            // Overlay metadata is advisory; retain the screenshot when a
            // concurrently restarting extension cannot answer this call.
            if let Ok(regions) = self.shell().get_overlay_regions() {
                observation["overlayRegions"] = valid_overlay_regions(&regions);
            }
        }
        Ok(())
    }

    /// The window and its observation, revalidated for an action.
    fn action_target(&mut self, params: &Json) -> Result<WindowInfo, Error> {
        let window = self.current_window(&self.select_window(params)?)?;
        self.validate_observation(params, &window)?;
        Ok(window)
    }

    fn click(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        // The dispatcher only lets a primary click on an element through headless.
        if flag(params, "headless", false) {
            let element = self.element_for_index(&window, params, context)?;
            let Some(action) = atspi_actions::do_primary_action(&element) else {
                return Err(Error::new(
                    "headless_unavailable",
                    "The element exposes no AT-SPI action; pass headless false to click it with the pointer",
                ));
            };
            self.mark_input_complete();
            let mut result = action_result();
            result["delivery"] = json!("semantic");
            result["semanticAction"] = json!(action);
            return Ok(result);
        }
        let by_element = has(params, "elementIndex");
        let transform = if by_element {
            None
        } else {
            Some(self.coordinate_transform(params, &window)?)
        };
        self.activate(&window)?;
        let local = match &transform {
            Some(transform) => point_from_json(params, "x", "y", &window, transform)?,
            None => self
                .element_center(&window, params, context)?
                .ok_or_else(|| {
                    Error::new(
                        "element_not_found",
                        "The selected accessibility element has no screen rectangle",
                    )
                })?,
        };
        self.move_pointer(
            window.x + local.0,
            window.y + local.1,
            context,
            Some(&window),
        )?;
        let modifiers = if has(params, "modifiers") {
            key_list(&json!({"key": string_value(params, "modifiers", "")?}))?
        } else {
            Vec::new()
        };
        let button = button_name(params)?;
        let release_context = Context::new();
        let mut held = Vec::new();
        let outcome =
            self.click_with_modifiers(&modifiers, &button, context, &release_context, &mut held);
        if outcome.is_err() {
            for modifier in held.iter().rev() {
                let _ = self.key_event(modifier, false, &release_context);
            }
        }
        outcome?;
        self.mark_input_complete();
        let mut result = action_result();
        if let Some(transform) = transform {
            result["coordinateSpace"] = json!(transform.space);
            result["windowPoint"] = point_json(local);
        }
        Ok(result)
    }

    fn click_with_modifiers(
        &mut self,
        modifiers: &[String],
        button: &str,
        context: &Context,
        release_context: &Context,
        held: &mut Vec<String>,
    ) -> Result<(), Error> {
        for modifier in modifiers {
            // A toggled key is already down and must stay down after the click.
            if self.toggled.contains(&canonical_key(modifier)) {
                continue;
            }
            self.key_event(modifier, true, context)?;
            held.push(modifier.clone());
        }
        self.button_event(button, true, context)?;
        let released = wait_checked(context, 30)
            .and_then(|()| self.button_event(button, false, release_context));
        if released.is_err() {
            let _ = self.button_event(button, false, release_context);
            return released;
        }
        for modifier in held.iter().rev() {
            self.key_event(modifier, false, release_context)?;
        }
        Ok(())
    }

    fn drag(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        let by_elements = has(params, "fromElementIndex") || has(params, "toElementIndex");
        let mut transform = if by_elements {
            None
        } else {
            Some(self.coordinate_transform(params, &window)?)
        };
        self.activate(&window)?;
        let (from, to) = if by_elements {
            if !has(params, "fromElementIndex") || !has(params, "toElementIndex") {
                return Err(Error::invalid_argument(
                    "fromElementIndex and toElementIndex must be supplied together",
                ));
            }
            let element_params = |key: &str| {
                let mut element_params = params.clone();
                element_params["elementIndex"] = params[key].clone();
                element_params
            };
            let from =
                self.element_center(&window, &element_params("fromElementIndex"), context)?;
            let to = self.element_center(&window, &element_params("toElementIndex"), context)?;
            let (Some(from), Some(to)) = (from, to) else {
                return Err(Error::new(
                    "element_not_found",
                    "Drag elements have no screen rectangle",
                ));
            };
            (from, to)
        } else {
            let points = self.coordinate_transform(params, &window)?;
            let from = point_from_json(params, "fromX", "fromY", &window, &points)?;
            let to = point_from_json(params, "toX", "toY", &window, &points)?;
            transform = Some(points);
            (from, to)
        };
        let duration = integer(params, "durationMs", DEFAULT_DRAG_DURATION_MS)?;
        let steps = integer(params, "steps", DEFAULT_DRAG_STEPS)?;
        let hold_before = integer(params, "holdBeforeMs", DEFAULT_HOLD_BEFORE_MS)?;
        let hold_after = integer(params, "holdAfterMs", DEFAULT_HOLD_AFTER_MS)?;
        if duration < 0 || steps < 1 || hold_before < 0 || hold_after < 0 {
            return Err(Error::invalid_argument(
                "durationMs, steps, and hold values must be non-negative (steps >= 1)",
            ));
        }
        let button = button_name(params)?;
        self.move_pointer(window.x + from.0, window.y + from.1, context, Some(&window))?;
        let release_context = Context::new();
        let mut held = HeldInput::default();
        let path = DragPath {
            from,
            to,
            duration,
            steps,
            hold_before,
            hold_after,
        };
        let outcome = self.drag_with_held_input(
            params,
            &window,
            &button,
            &path,
            context,
            &release_context,
            &mut held,
        );
        // The button is released first, then the modifiers held for the whole drag.
        if held.button {
            let _ = self.button_event(&button, false, &release_context);
        }
        for modifier in held.modifiers.iter().rev() {
            let _ = self.key_event(modifier, false, &release_context);
        }
        outcome?;
        self.mark_input_complete();
        let mut result = action_result();
        if let Some(transform) = transform {
            result["coordinateSpace"] = json!(transform.space);
            result["windowFrom"] = point_json(from);
            result["windowTo"] = point_json(to);
        }
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn drag_with_held_input(
        &mut self,
        params: &Json,
        window: &WindowInfo,
        button: &str,
        path: &DragPath,
        context: &Context,
        release_context: &Context,
        held: &mut HeldInput,
    ) -> Result<(), Error> {
        if has(params, "modifiers") {
            for modifier in key_list(&json!({"key": string_value(params, "modifiers", "")?}))? {
                if self.toggled.contains(&canonical_key(&modifier)) {
                    continue;
                }
                self.key_event(&modifier, true, context)?;
                held.modifiers.push(modifier);
            }
        }
        self.button_event(button, true, context)?;
        held.button = true;
        wait_checked(context, path.hold_before)?;
        let (from, to) = (path.from, path.to);
        for step in 1..=path.steps {
            context.check()?;
            let fraction = f64::from(step) / f64::from(path.steps);
            let x = (f64::from(from.0) + f64::from(to.0 - from.0) * fraction).round() as i32;
            let y = (f64::from(from.1) + f64::from(to.1 - from.1) * fraction).round() as i32;
            self.move_pointer(window.x + x, window.y + y, context, Some(window))?;
            let duration = f64::from(path.duration);
            let elapsed = (duration * f64::from(step) / f64::from(path.steps)).round() as i32;
            let previous = (duration * f64::from(step - 1) / f64::from(path.steps)).round() as i32;
            wait_checked(context, elapsed - previous)?;
        }
        wait_checked(context, path.hold_after)?;
        self.button_event(button, false, release_context)?;
        held.button = false;
        Ok(())
    }

    fn scroll(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        if flag(params, "headless", false) {
            return Err(Error::new(
                "headless_unavailable",
                "AT-SPI has no directional scroll, so Linux scrolls only with the pointer; pass headless false",
            ));
        }
        let window = self.action_target(params)?;
        let mut coordinate_space = None;
        let point = if has(params, "elementIndex") {
            self.element_center(&window, params, context)?
                .ok_or_else(|| {
                    Error::new(
                        "element_not_found",
                        "The selected accessibility element has no screen rectangle",
                    )
                })?
        } else {
            let transform = self.coordinate_transform(params, &window)?;
            coordinate_space = Some(transform.space);
            point_from_json(params, "x", "y", &window, &transform)?
        };
        self.activate(&window)?;
        self.move_pointer(
            window.x + point.0,
            window.y + point.1,
            context,
            Some(&window),
        )?;
        let direction = string_value(params, "direction", "")?.to_ascii_lowercase();
        if !matches!(direction.as_str(), "up" | "down" | "left" | "right") {
            return Err(Error::invalid_argument(
                "direction must be up, down, left, or right",
            ));
        }
        self.scroll_event(integer(params, "amount", 3)?.max(1), &direction, context)?;
        self.mark_input_complete();
        let mut result = action_result();
        if let Some(space) = coordinate_space {
            result["coordinateSpace"] = json!(space);
        }
        result["windowPoint"] = point_json(point);
        Ok(result)
    }

    fn type_text(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        self.activate(&window)?;
        let text = string_value(params, "text", "")?;
        if text.is_empty() {
            return Err(Error::invalid_argument("text is required"));
        }
        self.type_text_impl(&text, context)?;
        self.mark_input_complete();
        Ok(action_result())
    }

    fn press_key(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        self.activate(&window)?;
        let key = string_value(params, "key", "")?;
        if key.is_empty() {
            return Err(Error::invalid_argument("key is required"));
        }
        self.require_not_toggled(&key)?;
        self.key_event(&key, true, context)?;
        self.key_event(&key, false, &Context::new())?;
        self.mark_input_complete();
        Ok(action_result())
    }

    fn hotkey(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        self.activate(&window)?;
        self.hotkey_event(&key_list(params)?, context)?;
        self.mark_input_complete();
        Ok(action_result())
    }

    fn set_value(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        if !flag(params, "headless", false) {
            self.activate(&window)?;
        }
        if !has(params, "elementIndex") {
            return Err(Error::invalid_argument("set-value requires elementIndex"));
        }
        let Some(requested) = params.get("value") else {
            return Err(Error::invalid_argument("value is required"));
        };
        let element = self.element_for_index(&window, params, context)?;
        let text = match requested.as_str() {
            Some(text) => text.to_owned(),
            None => requested.to_string(),
        };
        let mut changed = element.has_editable_text() && element.set_text_contents(&text);
        if !changed
            && element.has_value()
            && let Some(number) = requested.as_f64()
        {
            changed = element.set_current_value(number);
        }
        if !changed {
            return Err(Error::new(
                "unsupported",
                "The selected AT-SPI element does not expose an editable value",
            ));
        }
        self.mark_input_complete();
        let mut result = action_result();
        result["delivery"] = json!("semantic");
        Ok(result)
    }

    fn paste_text(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.ensure_active(params)?;
        let window = self.action_target(params)?;
        self.activate(&window)?;
        let text = string_value(params, "text", "")?;
        if text.is_empty() {
            return Err(Error::invalid_argument("text is required"));
        }
        let old_clipboard = read_clipboard(self.wayland);
        let pasted = write_clipboard(self.wayland, &text)
            .and_then(|()| self.hotkey_event(&["ctrl".to_owned(), "v".to_owned()], context))
            .and_then(|()| wait_checked(context, 150));
        let _ = write_clipboard(self.wayland, old_clipboard.as_deref().unwrap_or(""));
        pasted?;
        self.mark_input_complete();
        Ok(action_result())
    }

    // set-state, scroll-to, and select-option never activate the window, even
    // outside headless mode, because the dispatcher runs them without the
    // physical-input turn.

    fn set_state(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        let window = self.action_target(params)?;
        let state = string_value(params, "state", "")?;
        let element = self.element_for_index(&window, params, context)?;
        let Some(change) = atspi_actions::set_state(&element, &state, context)? else {
            return Err(Error::new(
                "pattern_unavailable",
                format!("The target element exposes no AT-SPI state and action for state {state}"),
            ));
        };
        self.mark_input_complete();
        let mut result = state_change_result(&change);
        result["state"] = json!(state);
        Ok(result)
    }

    fn scroll_to(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        let window = self.action_target(params)?;
        let element = self.element_for_index(&window, params, context)?;
        let window_bounds = [window.x, window.y, window.width, window.height];
        let (action, visible) = atspi_actions::scroll_to(&element, window_bounds, context)?;
        self.mark_input_complete();
        let mut result = semantic_result(&action);
        result["visible"] = json!(visible);
        Ok(result)
    }

    fn select_option(&mut self, params: &Json, context: &Context) -> Result<Json, Error> {
        let window = self.action_target(params)?;
        let option = string_value(params, "option", "")?;
        let container = self.element_for_index(&window, params, context)?;
        let change = atspi_actions::select_named(&container, &option, context)?;
        self.mark_input_complete();
        let mut result = state_change_result(&change);
        result["option"] = json!(option);
        Ok(result)
    }
}

#[derive(Default)]
struct HeldInput {
    button: bool,
    modifiers: Vec<String>,
}

struct DragPath {
    from: (i32, i32),
    to: (i32, i32),
    duration: i32,
    steps: i32,
    hold_before: i32,
    hold_after: i32,
}

fn spawn_heartbeat(shared: Arc<Shared>) -> JoinHandle<()> {
    thread::spawn(move || {
        while !shared.heartbeat_stop.load(Ordering::Acquire) {
            let deadline = Instant::now() + HEARTBEAT_INTERVAL;
            while Instant::now() < deadline && !shared.heartbeat_stop.load(Ordering::Acquire) {
                thread::sleep(MONITOR_POLL_INTERVAL);
            }
            if shared.heartbeat_stop.load(Ordering::Acquire) {
                break;
            }
            if shared.session_active.load(Ordering::Acquire)
                && !shared.shell.call_bool("Heartbeat", 1500).unwrap_or(false)
            {
                shared.end_session();
            }
        }
    })
}

/// Watches the extension's Stopped signal. A disappearing session bus is
/// equivalent to a stopped indicator.
fn spawn_stop_monitor(shared: Arc<Shared>) -> JoinHandle<()> {
    thread::spawn(move || {
        if !shared.shell.available() {
            return;
        }
        let Ok(mut subscription) = ShellStopSubscription::new(shared.shell.connection()) else {
            shared.end_session();
            return;
        };
        while !shared.stop_monitor_stop.load(Ordering::Acquire) {
            if subscription.poll_stopped() {
                shared.end_session();
                return;
            }
            thread::sleep(MONITOR_POLL_INTERVAL);
        }
    })
}

fn session_required() -> Error {
    Error::new(
        "session_required",
        "Start a computer-use session before input actions",
    )
}

fn session_interrupted() -> Error {
    Error::new("cancelled", "Session interrupted")
}

fn eis_disconnected() -> Error {
    Error::new(
        "eis_disconnected",
        "The authorized EIS input session disconnected",
    )
}

fn window_not_on_atspi_bus() -> Error {
    Error::new(
        "accessibility_unavailable",
        "The target window is not exposed on the AT-SPI bus",
    )
}

fn object_path(path: &str) -> Result<ObjectPath<'_>, Error> {
    ObjectPath::try_from(path).map_err(|error| Error::new("protocol_error", error.to_string()))
}

/// One window of the extension's ListWindows payload. None when an item or a
/// present field has the wrong type.
fn extension_window(item: &Json) -> Option<WindowInfo> {
    let item = item.as_object()?;
    let text = |key: &str| match item.get(key) {
        None => Some(String::new()),
        Some(value) => value.as_str().map(str::to_owned),
    };
    let number = |key: &str| match item.get(key) {
        None => Some(0),
        Some(value) => json_int(value),
    };
    let boolean = |key: &str| match item.get(key) {
        None => Some(false),
        Some(value) => value.as_bool(),
    };
    let owner_id = text("ownerWindowId")?;
    let modal = !owner_id.is_empty() && boolean("modal")?;
    Some(WindowInfo {
        id: text("id")?,
        title: text("title")?,
        app: text("app")?,
        pid: number("pid")?,
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
        active: boolean("active")?,
        owner_id,
        modal,
    })
}

/// Accepts any JSON number, truncated toward zero, or a boolean as 0 or 1,
/// as earlier releases converted these fields.
fn json_int(value: &Json) -> Option<i32> {
    if let Some(number) = value.as_i64() {
        return Some(number as i32);
    }
    if let Some(number) = value.as_u64() {
        return Some(number as i32);
    }
    if let Some(number) = value.as_f64() {
        return Some(number as i32);
    }
    value.as_bool().map(i32::from)
}

/// A transient-for parent can be a group leader, an unmapped window, or a
/// filtered shell window; only a listed owner is a targetable id.
fn drop_unlisted_owners(windows: &mut [WindowInfo]) {
    let listed: BTreeSet<String> = windows.iter().map(|window| window.id.clone()).collect();
    for window in windows {
        if !window.owner_id.is_empty() && !listed.contains(&window.owner_id) {
            window.owner_id.clear();
            window.modal = false;
        }
    }
}

fn valid_overlay_regions(candidate: &Json) -> Json {
    let Some(items) = candidate.as_array() else {
        return json!([]);
    };
    let regions: Vec<Json> = items
        .iter()
        .filter_map(|item| {
            let kind = item.get("kind").filter(|kind| kind.is_string())?;
            let [x, y, width, height] =
                ["x", "y", "width", "height"].map(|key| item.get(key).and_then(Json::as_f64));
            let (x, y, width, height) = (x?, y?, width?, height?);
            let finite = x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite();
            (finite && width > 0.0 && height > 0.0)
                .then(|| json!({"kind": kind, "x": x, "y": y, "width": width, "height": height}))
        })
        .collect();
    json!(regions)
}

fn action_result() -> Json {
    json!({"delivered": true, "verification": {"state": "unverified"}})
}

fn semantic_result(action: &str) -> Json {
    let mut result = action_result();
    result["delivery"] = json!("semantic");
    if !action.is_empty() {
        result["semanticAction"] = json!(action);
    }
    result
}

fn state_change_result(change: &StateChange) -> Json {
    let mut result = semantic_result(&change.action);
    result["changed"] = json!(change.changed);
    result["verified"] = json!(change.verified);
    result
}

fn point_json((x, y): (i32, i32)) -> Json {
    json!({"x": x, "y": y})
}

/// x/y are pixels of the selected screenshot; the transform converts them to
/// a window-local point.
fn point_from_json(
    params: &Json,
    x_key: &str,
    y_key: &str,
    window: &WindowInfo,
    transform: &CoordinateTransform,
) -> Result<(i32, i32), Error> {
    if !has(params, x_key) || !has(params, y_key) {
        return Err(Error::invalid_argument(format!(
            "{x_key} and {y_key} are required"
        )));
    }
    let x = number(params, x_key, 0.0)?;
    let y = number(params, y_key, 0.0)?;
    let image = &transform.image;
    let inside = x.is_finite()
        && y.is_finite()
        && x >= 0.0
        && y >= 0.0
        && x < f64::from(image.width)
        && y < f64::from(image.height);
    if !inside {
        return Err(Error::invalid_argument(format!(
            "Input coordinates must be finite and inside the {} screenshot",
            transform.space
        )));
    }
    let local_x = ((x / image.scale_x).round() as i32).clamp(0, (window.width - 1).max(0));
    let local_y = ((y / image.scale_y).round() as i32).clamp(0, (window.height - 1).max(0));
    Ok((local_x, local_y))
}

fn button_name(params: &Json) -> Result<String, Error> {
    let button = string_value(params, "button", "left")?.to_ascii_lowercase();
    if !matches!(button.as_str(), "left" | "right" | "middle") {
        return Err(Error::invalid_argument(
            "button must be left, right, or middle",
        ));
    }
    Ok(button)
}

fn key_list(params: &Json) -> Result<Vec<String>, Error> {
    if let Some(keys) = params.get("keys") {
        let keys = keys
            .as_array()
            .ok_or_else(|| Error::invalid_argument("keys must be an array"))?;
        return keys
            .iter()
            .map(|key| {
                key.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::invalid_argument("keys must contain strings"))
            })
            .collect();
    }
    let hotkey = string_value(params, "key", "")?;
    if hotkey.is_empty() {
        return Err(Error::invalid_argument("key or keys is required"));
    }
    hotkey
        .split('+')
        .map(|key| {
            if key.is_empty() {
                Err(Error::invalid_argument("key contains an empty component"))
            } else {
                Ok(key.to_owned())
            }
        })
        .collect()
}

fn is_modifier(key: &str) -> bool {
    matches!(key, "ctrl" | "shift" | "alt" | "super")
}

fn canonical_key(key: &str) -> String {
    let key = key.to_ascii_lowercase();
    let canonical = match key.as_str() {
        "control" | "commandorcontrol" | "cmdorctrl" => "ctrl",
        "command" | "cmd" | "meta" | "win" | "super" => "super",
        "option" => "alt",
        "return" => "enter",
        "esc" => "escape",
        "page_up" => "pageup",
        "page_down" => "pagedown",
        _ => return key,
    };
    canonical.to_owned()
}

/// The X11 keysym the RemoteDesktop Notify* transport takes.
fn common_keysym(key: &str) -> u32 {
    let key = canonical_key(key);
    match key.as_str() {
        "ctrl" => 0xffe3,
        "shift" => 0xffe1,
        "alt" => 0xffe9,
        "super" => 0xffeb,
        "enter" => 0xff0d,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "space" => 0x20,
        "left" => 0xff51,
        "right" => 0xff53,
        "up" => 0xff52,
        "down" => 0xff54,
        "home" => 0xff50,
        "end" => 0xff57,
        "insert" => 0xff63,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "pause" => 0xff13,
        _ if key.len() == 1 => u32::from(key.as_bytes()[0]),
        _ => 0,
    }
}

/// The Linux evdev keycode libei takes; 0 for an unknown key.
fn evdev_keycode(key: &str) -> u32 {
    match canonical_key(key).as_str() {
        "escape" => 1,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "minus" => 12,
        "=" => 13,
        "backspace" => 14,
        "tab" => 15,
        "q" => 16,
        "w" => 17,
        "e" => 18,
        "r" => 19,
        "t" => 20,
        "y" => 21,
        "u" => 22,
        "i" => 23,
        "o" => 24,
        "p" => 25,
        "[" => 26,
        "]" => 27,
        "enter" => 28,
        "ctrl" => 29,
        "a" => 30,
        "s" => 31,
        "d" => 32,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        ";" => 39,
        "'" => 40,
        "`" => 41,
        "shift" => 42,
        "\\" => 43,
        "z" => 44,
        "x" => 45,
        "c" => 46,
        "v" => 47,
        "b" => 48,
        "n" => 49,
        "m" => 50,
        "," => 51,
        "." => 52,
        "/" => 53,
        "alt" => 56,
        "space" => 57,
        "capslock" => 58,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "numlock" => 69,
        "scrolllock" => 70,
        "f11" => 87,
        "f12" => 88,
        "rightctrl" => 97,
        "rightalt" => 100,
        "home" => 102,
        "up" => 103,
        "pageup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" => 109,
        "insert" => 110,
        "delete" => 111,
        "pause" => 119,
        "super" => 125,
        _ => 0,
    }
}

fn evdev_button(button: &str) -> u32 {
    match button {
        "left" => 272,
        "middle" => 274,
        _ => 273,
    }
}

fn x11_button(button: &str) -> u8 {
    match button {
        "left" => 1,
        "middle" => 2,
        _ => 3,
    }
}

fn has(params: &Json, key: &str) -> bool {
    params.get(key).is_some()
}

fn present(params: &Json, key: &str) -> Option<Json> {
    params.get(key).filter(|value| !value.is_null()).cloned()
}

fn integer(params: &Json, key: &str, fallback: i32) -> Result<i32, Error> {
    let Some(value) = present(params, key) else {
        return Ok(fallback);
    };
    let number = value
        .as_i64()
        .or_else(|| value.as_u64().map(|number| number as i64));
    number
        .map(|number| number as i32)
        .ok_or_else(|| Error::invalid_argument(format!("{key} must be an integer")))
}

fn number(params: &Json, key: &str, fallback: f64) -> Result<f64, Error> {
    let Some(value) = present(params, key) else {
        return Ok(fallback);
    };
    value
        .as_f64()
        .ok_or_else(|| Error::invalid_argument(format!("{key} must be a number")))
}

fn string_value(params: &Json, key: &str, fallback: &str) -> Result<String, Error> {
    let Some(value) = present(params, key) else {
        return Ok(fallback.to_owned());
    };
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_argument(format!("{key} must be a string")))
}

/// A boolean the dispatcher has already validated.
fn flag(params: &Json, key: &str, fallback: bool) -> bool {
    params.get(key).and_then(Json::as_bool).unwrap_or(fallback)
}

/// Waits while checking for cancellation every 10 ms.
pub fn wait_checked(context: &Context, milliseconds: i32) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_millis(milliseconds.max(0) as u64);
    while Instant::now() < deadline {
        context.check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.clamp(Duration::from_millis(1), Duration::from_millis(10)));
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> i64 {
    started.elapsed().as_millis() as i64
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn command_exists(command: &str) -> bool {
    ["/usr/bin/", "/bin/"].iter().any(|directory| {
        let path =
            CString::new(format!("{directory}{command}")).expect("command names contain no NUL");
        // SAFETY: access only reads the NUL-terminated path.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    })
}

fn readable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: access only reads the NUL-terminated path.
    unsafe { libc::access(path.as_ptr(), libc::R_OK) == 0 }
}

pub fn strerror(code: i32) -> String {
    // SAFETY: strerror returns a NUL-terminated message for any code.
    unsafe {
        CStr::from_ptr(libc::strerror(code))
            .to_string_lossy()
            .into_owned()
    }
}

/// 32 random hexadecimal digits.
fn uuid() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| Error::new("provider_error", error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn utc_timestamp(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs() as libc::time_t;
    // SAFETY: gmtime_r fills the caller's tm from a valid time_t.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&seconds, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        since_epoch.subsec_millis()
    )
}

/// $XDG_RUNTIME_DIR/desktop-computer-use (or under /tmp), private to the user.
fn runtime_directory() -> PathBuf {
    let base = env("XDG_RUNTIME_DIR");
    let directory = PathBuf::from(if base.is_empty() {
        "/tmp".to_owned()
    } else {
        base
    })
    .join("desktop-computer-use");
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&directory);
    let _ = std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700));
    directory
}

fn command_with_stdin(argv: &[&str], input: &str) -> i32 {
    let Ok(mut child) = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
    else {
        return 127;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(-1)
}

fn read_command(argv: &[&str]) -> Option<String> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(1024 * 1024).read_to_end(&mut output);
    }
    let status = child.wait().ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&output).into_owned())
}

fn read_clipboard(wayland: bool) -> Option<String> {
    if wayland && command_exists("wl-paste") {
        return read_command(&["wl-paste", "--no-newline"]);
    }
    if command_exists("xclip") {
        return read_command(&["xclip", "-selection", "clipboard", "-o"]);
    }
    if command_exists("xsel") {
        return read_command(&["xsel", "--clipboard", "--output"]);
    }
    if command_exists("wl-paste") {
        return read_command(&["wl-paste", "--no-newline"]);
    }
    None
}

fn write_clipboard(wayland: bool, text: &str) -> Result<(), Error> {
    let commands: [(&[&str], &str); 3] = [
        (&["wl-copy"], "wl-copy failed"),
        (&["xclip", "-selection", "clipboard"], "xclip failed"),
        (&["xsel", "--clipboard", "--input"], "xsel failed"),
    ];
    for (index, (argv, failure)) in commands.iter().enumerate() {
        let usable = (index != 0 || wayland) && command_exists(argv[0]);
        if !usable {
            continue;
        }
        if command_with_stdin(argv, text) != 0 {
            return Err(Error::new("input_unavailable", *failure));
        }
        return Ok(());
    }
    Err(Error::new(
        "unsupported",
        "paste-text requires wl-copy, xclip, or xsel",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_string_splits_on_plus_and_rejects_empty_parts() {
        assert_eq!(
            key_list(&json!({"key": "ctrl+shift+t"})).unwrap(),
            ["ctrl", "shift", "t"]
        );
        assert_eq!(
            key_list(&json!({"key": "ctrl++"})).unwrap_err().message,
            "key contains an empty component"
        );
    }

    #[test]
    fn key_aliases_map_to_canonical_names() {
        assert_eq!(canonical_key("CmdOrCtrl"), "ctrl");
        assert_eq!(canonical_key("Win"), "super");
        assert_eq!(canonical_key("Return"), "enter");
        assert_eq!(canonical_key("F5"), "f5");
    }

    #[test]
    fn notify_keysyms_cover_named_keys_and_single_characters() {
        assert_eq!(common_keysym("Escape"), 0xff1b);
        assert_eq!(common_keysym("a"), 0x61);
        assert_eq!(common_keysym("f5"), 0);
    }

    #[test]
    fn unlisted_owner_is_cleared_with_its_modal_flag() {
        let mut windows = vec![WindowInfo {
            id: "x11:2".into(),
            owner_id: "x11:1".into(),
            modal: true,
            ..WindowInfo::default()
        }];

        drop_unlisted_owners(&mut windows);

        assert_eq!(windows[0].owner_id, "");
        assert!(!windows[0].modal);
    }

    #[test]
    fn extension_window_with_a_string_pid_fails_the_whole_list() {
        assert!(extension_window(&json!({"id": "1", "pid": "12"})).is_none());
        assert_eq!(
            extension_window(&json!({"id": "1", "pid": 12.9}))
                .unwrap()
                .pid,
            12
        );
    }

    #[test]
    fn overlay_regions_drop_invalid_items_and_keep_numbers_as_doubles() {
        let regions = valid_overlay_regions(&json!([
            {"kind": "indicator", "x": 10, "y": 0, "width": 40, "height": 20},
            {"kind": "indicator", "x": 0, "y": 0, "width": 0, "height": 20},
            {"x": 0, "y": 0, "width": 5, "height": 5},
        ]));

        assert_eq!(
            regions.to_string(),
            r#"[{"height":20.0,"kind":"indicator","width":40.0,"x":10.0,"y":0.0}]"#
        );
    }

    #[test]
    fn screenshot_point_maps_through_the_scale_and_rounds_half_away_from_zero() {
        let window = WindowInfo {
            width: 100,
            height: 50,
            ..WindowInfo::default()
        };
        let transform = CoordinateTransform {
            space: "reduced",
            image: ObservationImage {
                path: PathBuf::new(),
                width: 50,
                height: 25,
                scale_x: 0.5,
                scale_y: 0.5,
            },
        };

        let point = point_from_json(
            &json!({"x": 10.25, "y": 24.9}),
            "x",
            "y",
            &window,
            &transform,
        )
        .unwrap();

        assert_eq!(point, (21, 49));
    }

    #[test]
    fn timestamp_has_millisecond_utc_format() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);

        assert_eq!(utc_timestamp(time), "2023-11-14T22:13:20.123Z");
    }
}

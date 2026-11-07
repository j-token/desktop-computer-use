//! Sender side of libei, used to talk to the EIS server that the
//! RemoteDesktop portal authorizes.
//!
//! ABI provenance and licensing: the opaque types, enum values, and function
//! signatures below are a minimal ABI shim for libei, taken from Ubuntu 24.04
//! (noble) libei-dev 1.2.1, /usr/include/libei-1.0/libei.h, whose upstream
//! source is https://gitlab.freedesktop.org/libinput/libei
//! (SPDX-License-Identifier: MIT, Copyright 2020 Red Hat, Inc.). The header is
//! not used and libei is not linked at build time: every symbol is resolved
//! with dlsym so the same binary runs on Ubuntu 22.04 installations that only
//! provide the runtime library or do not provide libei at all.
//!
//! Permission is granted, free of charge, to any person obtaining a copy of
//! this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the
//! rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
//! sell copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions: the above
//! copyright notice and this permission notice shall be included in all copies
//! or substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
//! WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
//! TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
//! CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

use dcu_core::{Context, Error};

use crate::backend::strerror;

#[repr(C)]
struct Ei {
    _private: [u8; 0],
}
#[repr(C)]
struct EiEvent {
    _private: [u8; 0],
}
#[repr(C)]
struct EiSeat {
    _private: [u8; 0],
}
#[repr(C)]
struct EiDevice {
    _private: [u8; 0],
}
#[repr(C)]
struct EiRegion {
    _private: [u8; 0],
}

const EI_DEVICE_CAP_POINTER: c_int = 1 << 0;
const EI_DEVICE_CAP_POINTER_ABSOLUTE: c_int = 1 << 1;
const EI_DEVICE_CAP_KEYBOARD: c_int = 1 << 2;
const EI_DEVICE_CAP_SCROLL: c_int = 1 << 4;
const EI_DEVICE_CAP_BUTTON: c_int = 1 << 5;

const EI_EVENT_CONNECT: c_int = 1;
const EI_EVENT_DISCONNECT: c_int = 2;
const EI_EVENT_SEAT_ADDED: c_int = 3;
const EI_EVENT_SEAT_REMOVED: c_int = 4;
const EI_EVENT_DEVICE_ADDED: c_int = 5;
const EI_EVENT_DEVICE_REMOVED: c_int = 6;
const EI_EVENT_DEVICE_PAUSED: c_int = 7;
const EI_EVENT_DEVICE_RESUMED: c_int = 8;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DISPATCH_POLL_INTERVAL_MS: u128 = 50;
const MAX_REGIONS_PER_DEVICE: usize = 256;
const FIRST_EVDEV_BUTTON_CODE: u32 = 0x110;

/// libei.so.1 is the stable SONAME used by Ubuntu 22.04 and 24.04. The second
/// name helps development images with an unversioned linker name without
/// creating a build-time dependency.
const LIBRARY_NAMES: [&CStr; 2] = [c"libei.so.1", c"libei.so"];

struct Api {
    handle: *mut c_void,
    new_sender: unsafe extern "C" fn(*mut c_void) -> *mut Ei,
    configure_name: unsafe extern "C" fn(*mut Ei, *const c_char),
    setup_backend_fd: unsafe extern "C" fn(*mut Ei, c_int) -> c_int,
    get_fd: unsafe extern "C" fn(*mut Ei) -> c_int,
    dispatch: unsafe extern "C" fn(*mut Ei),
    get_event: unsafe extern "C" fn(*mut Ei) -> *mut EiEvent,
    event_unref: unsafe extern "C" fn(*mut EiEvent) -> *mut EiEvent,
    event_get_type: unsafe extern "C" fn(*mut EiEvent) -> c_int,
    event_get_seat: unsafe extern "C" fn(*mut EiEvent) -> *mut EiSeat,
    event_get_device: unsafe extern "C" fn(*mut EiEvent) -> *mut EiDevice,
    seat_ref: unsafe extern "C" fn(*mut EiSeat) -> *mut EiSeat,
    seat_unref: unsafe extern "C" fn(*mut EiSeat) -> *mut EiSeat,
    seat_has_capability: unsafe extern "C" fn(*mut EiSeat, c_int) -> bool,
    seat_bind_capabilities: unsafe extern "C" fn(*mut EiSeat, ...),
    device_ref: unsafe extern "C" fn(*mut EiDevice) -> *mut EiDevice,
    device_unref: unsafe extern "C" fn(*mut EiDevice) -> *mut EiDevice,
    device_has_capability: unsafe extern "C" fn(*mut EiDevice, c_int) -> bool,
    device_get_region: unsafe extern "C" fn(*mut EiDevice, usize) -> *mut EiRegion,
    region_get_x: unsafe extern "C" fn(*mut EiRegion) -> u32,
    region_get_y: unsafe extern "C" fn(*mut EiRegion) -> u32,
    region_get_width: unsafe extern "C" fn(*mut EiRegion) -> u32,
    region_get_height: unsafe extern "C" fn(*mut EiRegion) -> u32,
    /// Added to the public API in libei 1.1; Ubuntu 22.04 may not export it.
    region_get_mapping_id: Option<unsafe extern "C" fn(*mut EiRegion) -> *const c_char>,
    device_start_emulating: unsafe extern "C" fn(*mut EiDevice, u32),
    device_stop_emulating: unsafe extern "C" fn(*mut EiDevice),
    device_frame: unsafe extern "C" fn(*mut EiDevice, u64),
    device_pointer_motion_absolute: unsafe extern "C" fn(*mut EiDevice, f64, f64),
    device_button_button: unsafe extern "C" fn(*mut EiDevice, u32, bool),
    device_scroll_delta: unsafe extern "C" fn(*mut EiDevice, f64, f64),
    device_keyboard_key: unsafe extern "C" fn(*mut EiDevice, u32, bool),
    now: unsafe extern "C" fn(*mut Ei) -> u64,
    device_close: unsafe extern "C" fn(*mut EiDevice),
    unref: unsafe extern "C" fn(*mut Ei) -> *mut Ei,
}

// SAFETY: the library handle and the function pointers are immutable after
// loading; libei objects are only used by the one serialized worker.
unsafe impl Send for Api {}

impl Api {
    #[allow(clippy::missing_transmute_annotations)]
    fn load() -> Result<Self, String> {
        // SAFETY: dlopen and dlerror are called with valid NUL-terminated names.
        let handle = LIBRARY_NAMES
            .iter()
            .map(|name| unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) })
            .find(|handle| !handle.is_null())
            .ok_or_else(|| {
                let detail = unsafe { libc::dlerror() };
                if detail.is_null() {
                    "libei.so.1 could not be loaded".to_owned()
                } else {
                    unsafe { CStr::from_ptr(detail) }
                        .to_string_lossy()
                        .into_owned()
                }
            })?;
        let symbol = |name: &CStr| -> *mut c_void { unsafe { libc::dlsym(handle, name.as_ptr()) } };
        let required = |name: &CStr| -> Result<*mut c_void, String> {
            let address = symbol(name);
            if address.is_null() {
                unsafe { libc::dlclose(handle) };
                return Err(format!("libei is missing {}", name.to_string_lossy()));
            }
            Ok(address)
        };
        macro_rules! required {
            ($name:literal) => {
                // SAFETY: the symbol has the signature declared in libei.h.
                unsafe { std::mem::transmute::<*mut c_void, _>(required($name)?) }
            };
        }
        let mapping_id_address = symbol(c"ei_region_get_mapping_id");
        Ok(Self {
            handle,
            new_sender: required!(c"ei_new_sender"),
            configure_name: required!(c"ei_configure_name"),
            setup_backend_fd: required!(c"ei_setup_backend_fd"),
            get_fd: required!(c"ei_get_fd"),
            dispatch: required!(c"ei_dispatch"),
            get_event: required!(c"ei_get_event"),
            event_unref: required!(c"ei_event_unref"),
            event_get_type: required!(c"ei_event_get_type"),
            event_get_seat: required!(c"ei_event_get_seat"),
            event_get_device: required!(c"ei_event_get_device"),
            seat_ref: required!(c"ei_seat_ref"),
            seat_unref: required!(c"ei_seat_unref"),
            seat_has_capability: required!(c"ei_seat_has_capability"),
            seat_bind_capabilities: required!(c"ei_seat_bind_capabilities"),
            device_ref: required!(c"ei_device_ref"),
            device_unref: required!(c"ei_device_unref"),
            device_has_capability: required!(c"ei_device_has_capability"),
            device_get_region: required!(c"ei_device_get_region"),
            region_get_x: required!(c"ei_region_get_x"),
            region_get_y: required!(c"ei_region_get_y"),
            region_get_width: required!(c"ei_region_get_width"),
            region_get_height: required!(c"ei_region_get_height"),
            // SAFETY: the symbol, when exported, has the signature declared in libei.h.
            region_get_mapping_id: (!mapping_id_address.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, _>(mapping_id_address) }),
            device_start_emulating: required!(c"ei_device_start_emulating"),
            device_stop_emulating: required!(c"ei_device_stop_emulating"),
            device_frame: required!(c"ei_device_frame"),
            device_pointer_motion_absolute: required!(c"ei_device_pointer_motion_absolute"),
            device_button_button: required!(c"ei_device_button_button"),
            device_scroll_delta: required!(c"ei_device_scroll_delta"),
            device_keyboard_key: required!(c"ei_device_keyboard_key"),
            now: required!(c"ei_now"),
            device_close: required!(c"ei_device_close"),
            unref: required!(c"ei_unref"),
        })
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        // SAFETY: the handle came from dlopen and every libei object is gone.
        unsafe { libc::dlclose(self.handle) };
    }
}

/// Whether the installed libei exports the sender-side symbols this client
/// uses. Probing for the removed ei_new/ei_connect pair would advertise EIS
/// although the transport could never be initialized.
pub fn libei_runtime_available() -> bool {
    // SAFETY: dlopen, dlsym, and dlclose are called with valid names and handles.
    unsafe {
        let mut handle = libc::dlopen(c"libei.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if handle.is_null() {
            handle = libc::dlopen(c"libei.so".as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        }
        if handle.is_null() {
            return false;
        }
        let available = !libc::dlsym(handle, c"ei_new_sender".as_ptr()).is_null()
            && !libc::dlsym(handle, c"ei_setup_backend_fd".as_ptr()).is_null();
        libc::dlclose(handle);
        available
    }
}

/// One EIS region in logical desktop coordinates; width and height use the
/// same logical-pixel units as x and y.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub mapping_id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

struct DeviceState {
    device: *mut EiDevice,
    has_absolute_pointer: bool,
    has_button: bool,
    has_keyboard: bool,
    has_scroll: bool,
    resumed: bool,
    emulating: bool,
    sequence: u32,
    regions: Vec<Region>,
    buttons_down: BTreeSet<u32>,
    keys_down: BTreeSet<u32>,
}

impl DeviceState {
    fn is_active(&self) -> bool {
        !self.device.is_null() && self.resumed && self.emulating
    }

    fn supports(&self, capability: InputCapability) -> bool {
        match capability {
            InputCapability::Button => self.has_button,
            InputCapability::Keyboard => self.has_keyboard,
            InputCapability::Scroll => self.has_scroll,
        }
    }
}

#[derive(Clone, Copy)]
enum InputCapability {
    Button,
    Keyboard,
    Scroll,
}

struct Connected {
    api: Api,
    context: *mut Ei,
}

/// The sender side of one EIS connection, used by one serialized backend
/// worker. A Context is checked before every potentially blocking operation
/// and while waiting for the handshake. `close` and `release` never fail.
pub struct EisClient {
    connection: Option<Connected>,
    connected: bool,
    disconnected: bool,
    mapping_ids_available: bool,
    seats: Vec<*mut EiSeat>,
    devices: Vec<DeviceState>,
}

// SAFETY: libei objects are only touched through `&mut self`, by the one
// serialized backend worker that owns this client.
unsafe impl Send for EisClient {}

impl Default for EisClient {
    fn default() -> Self {
        Self::new()
    }
}

impl EisClient {
    pub fn new() -> Self {
        Self {
            connection: None,
            connected: false,
            disconnected: false,
            mapping_ids_available: false,
            seats: Vec::new(),
            devices: Vec::new(),
        }
    }

    /// Attaches to an already-authorized EIS socket returned by ConnectToEIS:
    /// performs the EI handshake, binds pointer, button, keyboard, and scroll
    /// capabilities, and waits for at least one resumed absolute device. The
    /// descriptor is duplicated, so the caller keeps ownership of `fd`.
    pub fn connect(&mut self, fd: BorrowedFd<'_>, context: &Context) -> Result<(), Error> {
        if self.connection.is_some() {
            return Err(Error::new(
                "already_connected",
                "EisClient is already connected",
            ));
        }
        context.check()?;
        let api = Api::load()
            .map_err(|error| Error::new("unsupported", format!("libei is unavailable: {error}")))?;
        let owned_fd = duplicate_nonblocking(fd)?;
        // SAFETY: the API was loaded from libei and owned_fd is a valid descriptor.
        let ei = unsafe { (api.new_sender)(std::ptr::null_mut()) };
        if ei.is_null() {
            // SAFETY: owned_fd is ours and was not handed to libei.
            unsafe { libc::close(owned_fd) };
            return Err(Error::new("eis_transport", "ei_new_sender failed"));
        }
        // SAFETY: ei is a live context.
        let result = unsafe {
            (api.configure_name)(ei, c"desktop-computer-use".as_ptr());
            (api.setup_backend_fd)(ei, owned_fd)
        };
        if result < 0 {
            // ei_setup_backend_fd owns and cleans the descriptor according to
            // libei's public contract, and ei_unref tears down the partially
            // initialized context.
            unsafe { (api.unref)(ei) };
            return Err(Error::new(
                "eis_transport",
                format!("ei_setup_backend_fd failed: {}", errno_text(result)),
            ));
        }
        self.connection = Some(Connected { api, context: ei });
        if let Err(error) = self.wait_until_ready(context) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    pub fn connected(&self) -> bool {
        self.connection.is_some() && self.connected && !self.disconnected
    }

    pub fn ready(&self) -> bool {
        self.connected() && self.has_ready_absolute()
    }

    pub fn regions(&self) -> Vec<Region> {
        self.devices
            .iter()
            .filter(|state| state.is_active() && state.has_absolute_pointer)
            .flat_map(|state| state.regions.iter().cloned())
            .collect()
    }

    /// Sends one absolute point in the selected region's local coordinates.
    #[cfg(test)]
    pub fn absolute(
        &mut self,
        x: f64,
        y: f64,
        mapping_id: &str,
        context: &Context,
    ) -> Result<(), Error> {
        context.check()?;
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::invalid_argument(
                "Absolute EIS coordinates must be finite",
            ));
        }
        self.require_connected(context)?;
        if !self.has_ready_absolute() {
            return Err(Error::new(
                "eis_not_ready",
                "EIS has no resumed absolute device",
            ));
        }
        let selected = self
            .devices
            .iter()
            .filter(|state| state.is_active() && state.has_absolute_pointer)
            .find_map(|state| {
                let region = state.regions.iter().find(|region| {
                    (!mapping_id.is_empty() && region.mapping_id == mapping_id)
                        || (mapping_id.is_empty() && state.regions.len() == 1)
                })?;
                Some((state.device, region.clone()))
            });
        let Some((device, region)) = selected else {
            if !mapping_id.is_empty() && !self.mapping_ids_available {
                return Err(Error::new(
                    "unsupported",
                    "This libei version does not expose region mapping IDs",
                ));
            }
            return Err(Error::invalid_argument(
                "mappingId does not identify a resumed EIS region",
            ));
        };
        if x < 0.0 || y < 0.0 || x >= region.width || y >= region.height {
            return Err(Error::invalid_argument(
                "Absolute EIS coordinates are outside the selected region",
            ));
        }
        let connection = self.live();
        // SAFETY: device is a referenced, resumed device of this context.
        unsafe {
            (connection.api.device_pointer_motion_absolute)(device, region.x + x, region.y + y);
            (connection.api.device_frame)(device, (connection.api.now)(connection.context));
        }
        self.flush_writes();
        Ok(())
    }

    /// Sends a desktop-logical point through the region that contains it.
    pub fn absolute_global(&mut self, x: f64, y: f64, context: &Context) -> Result<(), Error> {
        context.check()?;
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::invalid_argument(
                "Absolute EIS coordinates must be finite",
            ));
        }
        self.require_connected(context)?;
        if !self.has_ready_absolute() {
            return Err(Error::new(
                "eis_not_ready",
                "EIS has no resumed absolute device",
            ));
        }
        let device = self
            .devices
            .iter()
            .filter(|state| state.is_active() && state.has_absolute_pointer)
            .find(|state| {
                state.regions.iter().any(|region| {
                    x >= region.x
                        && x < region.x + region.width
                        && y >= region.y
                        && y < region.y + region.height
                })
            })
            .map(|state| state.device)
            .ok_or_else(|| {
                Error::invalid_argument("Absolute EIS coordinates are outside all regions")
            })?;
        let connection = self.live();
        // SAFETY: device is a referenced, resumed device of this context.
        unsafe {
            (connection.api.device_pointer_motion_absolute)(device, x, y);
            (connection.api.device_frame)(device, (connection.api.now)(connection.context));
        }
        self.flush_writes();
        Ok(())
    }

    pub fn button(
        &mut self,
        evdev_code: u32,
        pressed: bool,
        context: &Context,
    ) -> Result<(), Error> {
        context.check()?;
        if evdev_code < FIRST_EVDEV_BUTTON_CODE {
            return Err(Error::invalid_argument(
                "button code must be an evdev BTN_* code",
            ));
        }
        self.require_connected(context)?;
        let index = self.require_capability(InputCapability::Button, "button")?;
        let connection = self.live();
        let device = self.devices[index].device;
        // SAFETY: device is a referenced, resumed device of this context.
        unsafe {
            (connection.api.device_button_button)(device, evdev_code, pressed);
            (connection.api.device_frame)(device, (connection.api.now)(connection.context));
        }
        self.flush_writes();
        let held = &mut self.devices[index].buttons_down;
        if pressed {
            held.insert(evdev_code)
        } else {
            held.remove(&evdev_code)
        };
        Ok(())
    }

    pub fn key(
        &mut self,
        evdev_keycode: u32,
        pressed: bool,
        context: &Context,
    ) -> Result<(), Error> {
        context.check()?;
        self.require_connected(context)?;
        let index = self.require_capability(InputCapability::Keyboard, "keyboard")?;
        let connection = self.live();
        let device = self.devices[index].device;
        // SAFETY: device is a referenced, resumed device of this context.
        unsafe {
            (connection.api.device_keyboard_key)(device, evdev_keycode, pressed);
            (connection.api.device_frame)(device, (connection.api.now)(connection.context));
        }
        self.flush_writes();
        let held = &mut self.devices[index].keys_down;
        if pressed {
            held.insert(evdev_keycode)
        } else {
            held.remove(&evdev_keycode)
        };
        Ok(())
    }

    pub fn scroll(&mut self, dx: f64, dy: f64, context: &Context) -> Result<(), Error> {
        context.check()?;
        if !dx.is_finite() || !dy.is_finite() {
            return Err(Error::invalid_argument("EIS scroll deltas must be finite"));
        }
        self.require_connected(context)?;
        let index = self.require_capability(InputCapability::Scroll, "scroll")?;
        let connection = self.live();
        let device = self.devices[index].device;
        // SAFETY: device is a referenced, resumed device of this context.
        unsafe {
            (connection.api.device_scroll_delta)(device, dx, dy);
            (connection.api.device_frame)(device, (connection.api.now)(connection.context));
        }
        self.flush_writes();
        Ok(())
    }

    /// Releases held keys and buttons while keeping the EIS connection.
    pub fn release(&mut self) {
        let Some(connection) = self.connection.as_ref().filter(|_| !self.disconnected) else {
            for state in &mut self.devices {
                state.buttons_down.clear();
                state.keys_down.clear();
            }
            return;
        };
        for state in &mut self.devices {
            if state.device.is_null() {
                continue;
            }
            let can_send = state.resumed && state.emulating;
            let mut changed = false;
            // SAFETY: state.device is a referenced device of this context.
            unsafe {
                if can_send && state.has_button {
                    for &code in &state.buttons_down {
                        (connection.api.device_button_button)(state.device, code, false);
                        changed = true;
                    }
                }
                if can_send && state.has_keyboard {
                    for &code in &state.keys_down {
                        (connection.api.device_keyboard_key)(state.device, code, false);
                        changed = true;
                    }
                }
                if changed {
                    (connection.api.device_frame)(
                        state.device,
                        (connection.api.now)(connection.context),
                    );
                }
            }
            state.buttons_down.clear();
            state.keys_down.clear();
        }
        self.flush_writes();
    }

    /// Releases input and tears down the EI context.
    pub fn close(&mut self) {
        if self.connection.is_none() {
            self.seats.clear();
            self.devices.clear();
            return;
        }
        self.release();
        let connection = self.connection.take().expect("connection checked above");
        for state in self.devices.drain(..) {
            if state.device.is_null() {
                continue;
            }
            // DEVICE_REMOVED events are not needed during context teardown.
            // SAFETY: state.device is a referenced device of this context.
            unsafe {
                if state.emulating && !self.disconnected {
                    (connection.api.device_stop_emulating)(state.device);
                }
                if !self.disconnected {
                    (connection.api.device_close)(state.device);
                }
                (connection.api.device_unref)(state.device);
            }
        }
        for seat in self.seats.drain(..) {
            // SAFETY: seat is a referenced seat of this context.
            unsafe { (connection.api.seat_unref)(seat) };
        }
        // SAFETY: every device and seat reference was dropped above.
        unsafe { (connection.api.unref)(connection.context) };
        self.connected = false;
        self.disconnected = false;
        self.mapping_ids_available = false;
    }

    fn live(&self) -> &Connected {
        self.connection
            .as_ref()
            .expect("require_connected checked the connection")
    }

    fn has_ready_absolute(&self) -> bool {
        self.devices.iter().any(|state| {
            state.is_active() && state.has_absolute_pointer && !state.regions.is_empty()
        })
    }

    fn find_device(&mut self, device: *mut EiDevice) -> Option<&mut DeviceState> {
        self.devices.iter_mut().find(|state| state.device == device)
    }

    fn require_capability(&self, capability: InputCapability, name: &str) -> Result<usize, Error> {
        self.devices
            .iter()
            .position(|state| state.is_active() && state.supports(capability))
            .ok_or_else(|| Error::new("unsupported", format!("EIS has no resumed {name} device")))
    }

    fn bind_seat(&mut self, seat: *mut EiSeat) {
        // Bind each capability only when the EIS seat advertises it. The
        // public API permits binding an unavailable capability, but checking
        // first makes the negotiation explicit and avoids requesting touch or
        // other device classes that this transport does not consume.
        let connection = self.live();
        let wanted = [
            EI_DEVICE_CAP_POINTER,
            EI_DEVICE_CAP_POINTER_ABSOLUTE,
            EI_DEVICE_CAP_BUTTON,
            EI_DEVICE_CAP_KEYBOARD,
            EI_DEVICE_CAP_SCROLL,
        ];
        // SAFETY: seat comes from a SEAT_ADDED event of this context; the
        // capability list is terminated by 0 as ei_seat_bind_capabilities expects.
        let seat_reference = unsafe {
            for capability in wanted {
                if (connection.api.seat_has_capability)(seat, capability) {
                    (connection.api.seat_bind_capabilities)(seat, capability, 0 as c_int);
                }
            }
            (connection.api.seat_ref)(seat)
        };
        self.seats.push(seat_reference);
    }

    fn add_device(&mut self, device: *mut EiDevice) {
        if device.is_null() || self.find_device(device).is_some() {
            return;
        }
        let connection = self.live();
        let api = &connection.api;
        // SAFETY: device comes from a DEVICE_ADDED event of this context.
        let mut state = unsafe {
            DeviceState {
                device: (api.device_ref)(device),
                has_absolute_pointer: (api.device_has_capability)(
                    device,
                    EI_DEVICE_CAP_POINTER_ABSOLUTE,
                ),
                has_button: (api.device_has_capability)(device, EI_DEVICE_CAP_BUTTON),
                has_keyboard: (api.device_has_capability)(device, EI_DEVICE_CAP_KEYBOARD),
                has_scroll: (api.device_has_capability)(device, EI_DEVICE_CAP_SCROLL),
                resumed: false,
                emulating: false,
                sequence: 0,
                regions: Vec::new(),
                buttons_down: BTreeSet::new(),
                keys_down: BTreeSet::new(),
            }
        };
        let mut mapping_ids_available = false;
        if state.has_absolute_pointer {
            // Region indices are stable for a device lifetime. A malformed
            // server must not make a client loop forever, hence the bound.
            for index in 0..MAX_REGIONS_PER_DEVICE {
                // SAFETY: device is live; a null region ends the list.
                let region = unsafe { (api.device_get_region)(device, index) };
                if region.is_null() {
                    break;
                }
                // SAFETY: region belongs to the live device.
                let value = unsafe {
                    let mapping_id = api
                        .region_get_mapping_id
                        .map(|get_mapping_id| get_mapping_id(region))
                        .filter(|id| !id.is_null())
                        .map(|id| CStr::from_ptr(id).to_string_lossy().into_owned())
                        .unwrap_or_default();
                    Region {
                        mapping_id,
                        x: f64::from((api.region_get_x)(region)),
                        y: f64::from((api.region_get_y)(region)),
                        width: f64::from((api.region_get_width)(region)),
                        height: f64::from((api.region_get_height)(region)),
                    }
                };
                state.regions.push(value);
            }
            mapping_ids_available = api.region_get_mapping_id.is_some();
        }
        if mapping_ids_available {
            self.mapping_ids_available = true;
        }
        self.devices.push(state);
    }

    fn remove_device(&mut self, device: *mut EiDevice) {
        let Some(position) = self.devices.iter().position(|state| state.device == device) else {
            return;
        };
        let state = self.devices.remove(position);
        if !state.device.is_null() {
            // SAFETY: the device reference was taken in add_device.
            unsafe { (self.live().api.device_unref)(state.device) };
        }
    }

    fn remove_seat(&mut self, seat: *mut EiSeat) {
        let Some(position) = self.seats.iter().position(|candidate| *candidate == seat) else {
            return;
        };
        let seat = self.seats.remove(position);
        if !seat.is_null() {
            // SAFETY: the seat reference was taken in bind_seat.
            unsafe { (self.live().api.seat_unref)(seat) };
        }
    }

    fn on_event(&mut self, event: *mut EiEvent) {
        let api = &self.live().api;
        let (event_get_seat, event_get_device, start_emulating) = (
            api.event_get_seat,
            api.event_get_device,
            api.device_start_emulating,
        );
        // SAFETY: event is a live event of this context, and the seat or
        // device getter is only called for an event that carries one.
        let event_type = unsafe { (api.event_get_type)(event) };
        let seat = || unsafe { event_get_seat(event) };
        let device = || unsafe { event_get_device(event) };
        match event_type {
            EI_EVENT_CONNECT => {
                self.connected = true;
                self.disconnected = false;
            }
            EI_EVENT_DISCONNECT => {
                self.connected = false;
                self.disconnected = true;
            }
            EI_EVENT_SEAT_ADDED => {
                let seat = seat();
                if !seat.is_null() {
                    self.bind_seat(seat);
                }
            }
            EI_EVENT_SEAT_REMOVED => self.remove_seat(seat()),
            EI_EVENT_DEVICE_ADDED => self.add_device(device()),
            EI_EVENT_DEVICE_REMOVED => self.remove_device(device()),
            EI_EVENT_DEVICE_PAUSED => {
                if let Some(state) = self.find_device(device()) {
                    state.resumed = false;
                    state.emulating = false;
                }
            }
            EI_EVENT_DEVICE_RESUMED => {
                let Some(state) = self.find_device(device()) else {
                    return;
                };
                if state.device.is_null() || state.emulating {
                    return;
                }
                state.resumed = true;
                state.sequence = if state.sequence == u32::MAX {
                    1
                } else {
                    state.sequence + 1
                };
                // SAFETY: state.device is a referenced device of this context.
                unsafe { start_emulating(state.device, state.sequence) };
                state.emulating = true;
            }
            // A sender should not receive input events, but ignoring an
            // extension event keeps the transport forward-compatible.
            _ => {}
        }
    }

    fn dispatch_events(&mut self) -> bool {
        let (dispatch, get_event, event_unref, ei) = {
            let connection = self.live();
            (
                connection.api.dispatch,
                connection.api.get_event,
                connection.api.event_unref,
                connection.context,
            )
        };
        // SAFETY: ei is the live context; each event is unreferenced once handled.
        unsafe { dispatch(ei) };
        let mut event_received = false;
        loop {
            let event = unsafe { get_event(ei) };
            if event.is_null() {
                break;
            }
            event_received = true;
            self.on_event(event);
            unsafe { event_unref(event) };
        }
        event_received
    }

    fn flush_writes(&self) {
        if let Some(connection) = &self.connection
            && !self.disconnected
        {
            // SAFETY: the context is live.
            unsafe { (connection.api.dispatch)(connection.context) };
        }
    }

    fn pump_once(&mut self, context: &Context, timeout_ms: c_int) -> Result<bool, Error> {
        context.check()?;
        let connection = self.live();
        // SAFETY: the context is live.
        let fd = unsafe { (connection.api.get_fd)(connection.context) };
        if fd < 0 {
            return Err(Error::new(
                "eis_disconnected",
                "libei did not expose a transport fd",
            ));
        }
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        let result = loop {
            // SAFETY: poll_fd is a valid pollfd array of length one.
            let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if result < 0 && last_errno() == libc::EINTR {
                continue;
            }
            break result;
        };
        if result < 0 {
            return Err(Error::new(
                "eis_transport",
                format!("poll failed: {}", errno_text(last_errno())),
            ));
        }
        let transport_closed =
            result > 0 && (poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0;
        // libei still queues EI_EVENT_DISCONNECT where possible. Dispatch once
        // before surfacing a poll error to the caller. Dispatching on a
        // timeout also drains events queued by ei_setup_backend_fd() itself.
        let event_received = self.dispatch_events();
        self.flush_writes();
        if transport_closed && !self.disconnected {
            self.connected = false;
            self.disconnected = true;
        }
        Ok(result != 0 || event_received)
    }

    fn require_connected(&mut self, context: &Context) -> Result<(), Error> {
        if self.connection.is_none() || self.disconnected {
            return Err(Error::new("eis_not_ready", "EIS is not connected"));
        }
        // EIS can pause or remove a device asynchronously. Drain readable
        // notifications before every action so the capability and emulation
        // state below reflects the server rather than the previous request.
        self.pump_once(context, 0)?;
        if self.disconnected || !self.connected {
            return Err(Error::new(
                "eis_disconnected",
                "The EIS server disconnected the client",
            ));
        }
        Ok(())
    }

    fn wait_until_ready(&mut self, context: &Context) -> Result<(), Error> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        while (!self.connected || !self.has_ready_absolute()) && !self.disconnected {
            context.check()?;
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::new(
                    "timeout",
                    "Timed out waiting for a resumed libei absolute device",
                ));
            }
            let remaining_ms = (deadline - now)
                .as_millis()
                .clamp(1, DISPATCH_POLL_INTERVAL_MS);
            self.pump_once(context, remaining_ms as c_int)?;
        }
        if self.disconnected {
            return Err(Error::new(
                "permission_required",
                "The EIS server disconnected the client",
            ));
        }
        if !self.connected {
            return Err(Error::new(
                "eis_disconnected",
                "The EIS client did not connect",
            ));
        }
        Ok(())
    }
}

impl Drop for EisClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn duplicate_nonblocking(fd: BorrowedFd<'_>) -> Result<c_int, Error> {
    // SAFETY: fcntl and dup are called on a borrowed, open descriptor.
    let mut owned_fd = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if owned_fd < 0 && last_errno() == libc::EINVAL {
        owned_fd = unsafe { libc::dup(fd.as_raw_fd()) };
    }
    if owned_fd < 0 {
        return Err(Error::new(
            "eis_transport",
            format!("Could not duplicate EIS fd: {}", errno_text(last_errno())),
        ));
    }
    // SAFETY: owned_fd is the descriptor duplicated above.
    let flags = unsafe { libc::fcntl(owned_fd, libc::F_GETFL, 0) };
    if flags < 0 || unsafe { libc::fcntl(owned_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        let saved_errno = last_errno();
        unsafe { libc::close(owned_fd) };
        return Err(Error::new(
            "eis_transport",
            format!(
                "Could not make EIS fd non-blocking: {}",
                errno_text(saved_errno)
            ),
        ));
    }
    Ok(owned_fd)
}

fn last_errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn errno_text(value: c_int) -> String {
    let code = value.abs();
    format!("{} ({code})", strerror(code))
}

#[cfg(test)]
#[path = "eis_client_tests.rs"]
mod tests;

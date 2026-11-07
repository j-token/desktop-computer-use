use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use dcu_core::{Backend, Context, Error, Json, WindowRelation, find_blocking_modal};
use parking_lot::Mutex;
use serde_json::json;
use windows::Win32::Foundation::{
    CloseHandle, FILETIME, GENERIC_READ, GetLastError, HANDLE, HWND, LPARAM, POINT, RECT,
    RPC_E_CHANGED_MODE, SYSTEMTIME,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Storage::FileSystem::GetTempPathW;
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoCreateGuid, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, OpenInputDesktop,
};
use windows::Win32::System::SystemInformation::{GetSystemTime, GetTickCount64};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Time::FileTimeToSystemTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, IsWindowEnabled, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, SendInput, SetActiveWindow, VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN,
    VK_MENU, VK_NEXT, VK_NUMLOCK, VK_PAUSE, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT,
    VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP, VkKeyScanW,
};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetLastActivePopup, GetSystemMetrics, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, MSG, PM_NOREMOVE, PeekMessageW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, SetCursorPos,
    SetForegroundWindow, ShowWindow, WHEEL_DELTA, WS_EX_NOACTIVATE,
};
use windows::core::{BOOL, PWSTR};

use crate::Mta;
use crate::capture::{CaptureRequest, CapturedImage, WindowCapture};
use crate::indicator::{self, SessionIndicator};
use crate::uia::{self, ElementRecord, ScrollPosition, UiaChangeWatcher, intersect};
use crate::watchdog::{InputWatchdog, keyboard_input, mouse_input};

const STOP_KEY: &str = "Escape x2";
const OBSERVATION_LIFETIME: Duration = Duration::from_secs(120);
// Several agents share the observation cache, so it keeps more than one agent needs.
const MAXIMUM_OBSERVATIONS: usize = 64;
const MAXIMUM_PENDING_WINDOWS: usize = 8;
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(750);

/// A top-level window handle, kept as its value so it can cross threads.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct WindowHandle(isize);

impl WindowHandle {
    fn of(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }

    fn id(self) -> String {
        format!("hwnd:{}", self.0 as usize)
    }

    fn exists(self) -> bool {
        unsafe { IsWindow(Some(self.hwnd())) }.as_bool()
    }
}

/// Initializes COM for the calling request thread, which may be a fresh
/// connection worker, and leaves an existing apartment as it is.
struct ComScope {
    initialized: bool,
    usable: bool,
}

impl ComScope {
    fn enter() -> Self {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            initialized: result.is_ok(),
            usable: result.is_ok() || result == RPC_E_CHANGED_MODE,
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

fn utf8(value: &[u16]) -> String {
    String::from_utf16(value).unwrap_or_default()
}

fn json_string(params: &Json, key: &str) -> String {
    params
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn json_string_or(params: &Json, key: &str, fallback: &str) -> String {
    params
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

/// Truncates integers wider than `i32` instead of rejecting them, as earlier
/// releases did, so existing clients get the same coordinates.
fn json_int(params: &Json, key: &str, fallback: i32) -> i32 {
    let Some(value) = params.get(key) else {
        return fallback;
    };
    value
        .as_i64()
        .map(|number| number as i32)
        .or_else(|| value.as_u64().map(|number| number as i32))
        .unwrap_or(fallback)
}

fn json_bool(params: &Json, key: &str, fallback: bool) -> bool {
    params.get(key).and_then(Json::as_bool).unwrap_or(fallback)
}

/// `std::stoul`/`std::stoull` semantics: leading whitespace, an optional
/// sign (a minus wraps), decimal digits up to the first non-digit; `None`
/// without digits or when the magnitude exceeds `maximum`.
fn parse_leading_unsigned(text: &str, maximum: u64) -> Option<u64> {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digit_count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let magnitude = digits[..digit_count]
        .bytes()
        .try_fold(0u64, |value, digit| {
            value.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
        })
        .filter(|&magnitude| magnitude <= maximum)?;
    if !negative {
        return Some(magnitude);
    }
    Some(if maximum == u64::from(u32::MAX) {
        u64::from(0u32.wrapping_sub(magnitude as u32))
    } else {
        0u64.wrapping_sub(magnitude)
    })
}

/// Drops a trailing empty part ("ctrl+") but keeps a leading or inner one
/// ("+a", "a++b"), which is how earlier releases split key chords.
fn split_on_plus(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    while start < text.len() {
        match text[start..].find('+') {
            Some(offset) => {
                parts.push(&text[start..start + offset]);
                start += offset + 1;
            }
            None => {
                parts.push(&text[start..]);
                break;
            }
        }
    }
    parts
}

fn guid_string() -> String {
    let Ok(guid) = (unsafe { CoCreateGuid() }) else {
        return format!("observation-{}", unsafe { GetTickCount64() });
    };
    let [d0, d1, d2, d3, d4, d5, d6, d7] = guid.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{d0:02X}{d1:02X}-{d2:02X}{d3:02X}{d4:02X}{d5:02X}{d6:02X}{d7:02X}}}",
        guid.data1, guid.data2, guid.data3
    )
}

fn iso8601(time: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}

fn iso8601_now() -> String {
    iso8601(&unsafe { GetSystemTime() })
}

fn iso8601_from_unix_ms(unix_ms: i64) -> String {
    const UNIX_EPOCH_AS_FILETIME_100NS: i64 = 116_444_736_000_000_000;
    let Some(intervals) = unix_ms
        .checked_mul(10_000)
        .and_then(|value| value.checked_add(UNIX_EPOCH_AS_FILETIME_100NS))
    else {
        return iso8601_now();
    };
    if unix_ms <= 0 {
        return iso8601_now();
    }
    let file_time = FILETIME {
        dwLowDateTime: intervals as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    };
    let mut time = SYSTEMTIME::default();
    if unsafe { FileTimeToSystemTime(&file_time, &mut time) }.is_err() {
        return iso8601_now();
    }
    iso8601(&time)
}

fn process_name(pid: u32) -> String {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut path = [0u16; 260 * 4];
    let mut length = path.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if queried.is_err() {
        return String::new();
    }
    let path = &path[..length as usize];
    let file_name_start = path
        .iter()
        .rposition(|&unit| unit == u16::from(b'\\') || unit == u16::from(b'/'))
        .map_or(0, |slash| slash + 1);
    utf8(&path[file_name_start..])
}

fn class_name(hwnd: HWND, capacity: usize) -> Vec<u16> {
    let mut buffer = vec![0u16; capacity];
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    buffer.truncate(length.max(0) as usize);
    buffer
}

fn is_session_indicator_window(hwnd: HWND) -> bool {
    String::from_utf16_lossy(&class_name(hwnd, 128)) == indicator::WINDOW_CLASS
}

#[derive(Clone)]
struct WindowInfo {
    handle: WindowHandle,
    pid: u32,
    app: String,
    title: String,
    rect: RECT,
    minimized: bool,
    enabled: bool,
    /// Nearest visible owner. Dialogs (MessageBox, common file dialogs, app
    /// dialogs) are owned top-level windows; a hidden owner such as a
    /// framework parking window is skipped so the id stays targetable.
    owner: Option<WindowHandle>,
    /// The owner is disabled while this window is shown: a modal dialog.
    modal: bool,
}

fn owner_of(hwnd: HWND) -> Option<HWND> {
    unsafe { GetWindow(hwnd, GW_OWNER) }
        .ok()
        .filter(|owner| !owner.is_invalid())
}

/// Owned windows also include tooltips, IME and console helper windows that a
/// user never interacts with; only activatable, uncloaked ones are listed.
fn listed_owned_window(hwnd: HWND) -> bool {
    if unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } & WS_EX_NOACTIVATE.0 as isize != 0 {
        return false;
    }
    let mut cloaked = 0u32;
    let cloaked_queried = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    if cloaked_queried.is_ok() && cloaked != 0 {
        return false;
    }
    String::from_utf16_lossy(&class_name(hwnd, 64)) != "tooltips_class32"
}

fn visible_owner(hwnd: HWND) -> Option<WindowHandle> {
    let mut owner = owner_of(hwnd);
    for _ in 0..16 {
        let current = owner?;
        if unsafe { IsWindowVisible(current) }.as_bool() {
            return Some(WindowHandle::of(current));
        }
        owner = owner_of(current);
    }
    None
}

fn inspect_window(handle: WindowHandle) -> Option<WindowInfo> {
    let hwnd = handle.hwnd();
    if !handle.exists()
        || !unsafe { IsWindowVisible(hwnd) }.as_bool()
        || is_session_indicator_window(hwnd)
    {
        return None;
    }
    if owner_of(hwnd).is_some() && !listed_owned_window(hwnd) {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err()
        || rect.right <= rect.left
        || rect.bottom <= rect.top
    {
        return None;
    }
    // GetWindowRect includes the invisible resize border on modern DWM
    // windows. WGC and the pixels a user sees use the extended frame, so a
    // single canonical rectangle keeps screenshots, UIA bounds, and native
    // input in the same coordinate space. Older/non-DWM windows fall back to
    // the Win32 rectangle above.
    let mut frame = RECT::default();
    let frame_queried = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            std::ptr::from_mut(&mut frame).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if frame_queried.is_ok() && frame.right > frame.left && frame.bottom > frame.top {
        rect = frame;
    }
    let title_length = unsafe { GetWindowTextLengthW(hwnd) };
    let mut title = vec![0u16; title_length.max(0) as usize + 1];
    let copied = if title_length > 0 {
        unsafe { GetWindowTextW(hwnd, &mut title) }
    } else {
        0
    };
    title.truncate(copied.max(0) as usize);

    let owner = visible_owner(hwnd);
    Some(WindowInfo {
        handle,
        pid,
        app: process_name(pid),
        title: utf8(&title),
        rect,
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        enabled: unsafe { IsWindowEnabled(hwnd) }.as_bool(),
        owner,
        modal: owner.is_some_and(|owner| !unsafe { IsWindowEnabled(owner.hwnd()) }.as_bool()),
    })
}

unsafe extern "system" fn collect_window(hwnd: HWND, windows: LPARAM) -> BOOL {
    // SAFETY: `windows` is the Vec passed by `top_level_windows` for this call.
    let windows = unsafe { &mut *(windows.0 as *mut Vec<WindowHandle>) };
    windows.push(WindowHandle::of(hwnd));
    BOOL(1)
}

/// Every top-level window in Z order, front to back.
fn top_level_windows() -> Vec<WindowHandle> {
    let mut windows: Vec<WindowHandle> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(std::ptr::from_mut(&mut windows) as isize),
        );
    }
    windows
}

fn enumerate_windows() -> Vec<WindowInfo> {
    top_level_windows()
        .into_iter()
        .filter_map(inspect_window)
        .collect()
}

fn app_name_matches(window: &WindowInfo, requested: &str) -> bool {
    let name = window.app.to_ascii_lowercase();
    name == requested || (name.len() > 4 && name.strip_suffix(".exe") == Some(requested))
}

fn parse_window_id(id: &str) -> Option<WindowHandle> {
    let value = parse_leading_unsigned(id.strip_prefix("hwnd:")?, u64::MAX)?;
    Some(WindowHandle(value as isize))
}

fn foreground_matches(handle: WindowHandle) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground == handle.hwnd() {
        return true;
    }
    !foreground.is_invalid() && unsafe { GetAncestor(foreground, GA_ROOT) } == handle.hwnd()
}

fn window_json(info: &WindowInfo) -> Json {
    let mut result = json!({
        "id": info.handle.id(),
        "app": info.app,
        "pid": info.pid,
        "title": info.title,
        "x": info.rect.left,
        "y": info.rect.top,
        "width": (info.rect.right - info.rect.left).max(0),
        "height": (info.rect.bottom - info.rect.top).max(0),
        "isMinimized": info.minimized,
        "isForeground": foreground_matches(info.handle),
        "isOffscreen": false,
        "isEnabled": info.enabled,
    });
    if let Some(owner) = info.owner {
        result["ownerWindowId"] = json!(owner.id());
        result["modal"] = json!(info.modal);
    }
    result
}

/// The dialog that receives input while `target` is disabled by a modal.
/// Windows disables only the owner, so an enabled window cannot be blocked
/// and needs no enumeration.
fn blocking_modal(target: &WindowInfo) -> Option<WindowInfo> {
    if unsafe { IsWindowEnabled(target.handle.hwnd()) }.as_bool() {
        return None;
    }
    // An owner may also have enabled palettes; the dialog the user last
    // activated is the modal one, so it is considered first.
    let last_active = WindowHandle::of(unsafe { GetLastActivePopup(target.handle.hwnd()) });
    let (mut windows, others): (Vec<WindowInfo>, Vec<WindowInfo>) = enumerate_windows()
        .into_iter()
        .partition(|info| info.handle == last_active);
    windows.extend(others);
    let relations: Vec<WindowRelation> = windows
        .iter()
        .map(|info| WindowRelation {
            id: info.handle.id(),
            owner_id: info.owner.map(WindowHandle::id).unwrap_or_default(),
            modal: info.modal,
        })
        .collect();
    let index = find_blocking_modal(&relations, &target.handle.id())?;
    windows.into_iter().nth(index)
}

/// Owned popups such as Chrome's permission and download bubbles leave the
/// owner enabled, so `blocking_modal` misses them, and the per-window capture
/// leaves them out of the owner's screenshot.
fn modeless_owned_windows(owner: WindowHandle) -> Vec<WindowInfo> {
    top_level_windows()
        .into_iter()
        .filter(|&handle| visible_owner(handle.hwnd()) == Some(owner))
        .filter_map(inspect_window)
        .filter(|info| !info.modal)
        .collect()
}

fn modal_json(modal: &WindowInfo) -> Json {
    json!({"windowId": modal.handle.id(), "title": modal.title, "app": modal.app})
}

fn pattern_for_state(state: &str) -> &'static str {
    match state {
        "selected" => "SelectionItemPattern",
        "expanded" | "collapsed" => "ExpandCollapsePattern",
        _ => "TogglePattern",
    }
}

fn reject_blocked_target(target: &WindowInfo) -> Result<(), Error> {
    let Some(modal) = blocking_modal(target) else {
        return Ok(());
    };
    let id = modal.handle.id();
    Err(Error::new(
        "modal_active",
        format!(
            "Window {} is blocked by modal dialog {id} \"{}\"; observe and act on --window-id {id} instead",
            target.handle.id(),
            modal.title
        ),
    ))
}

fn capture_directory() -> Result<PathBuf, Error> {
    let base = match unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
    {
        Ok(known) => {
            let path = unsafe { known.to_string() }.map(PathBuf::from);
            unsafe { CoTaskMemFree(Some(known.as_ptr().cast())) };
            path.ok()
        }
        Err(_) => None,
    };
    let base = match base {
        Some(base) => base,
        None => {
            let mut temporary = [0u16; 260];
            let length = unsafe { GetTempPathW(Some(&mut temporary)) } as usize;
            if length == 0 {
                return Err(Error::new(
                    "capture_path",
                    "Cannot determine a private capture directory",
                ));
            }
            PathBuf::from(String::from_utf16_lossy(
                &temporary[..length.min(temporary.len())],
            ))
        }
    };
    Ok(base.join("DesktopComputerUse").join("captures"))
}

fn private_capture_path(observation_id: &str, format: &str, suffix: &str) -> Result<String, Error> {
    let directory = capture_directory()?;
    std::fs::create_dir_all(&directory).map_err(|_| {
        Error::new(
            "capture_path",
            "Cannot create the private capture directory",
        )
    })?;
    let extension = if format == "png" { ".png" } else { ".jpg" };
    let path = directory.join(format!("{observation_id}{suffix}{extension}"));
    Ok(path.to_str().unwrap_or_default().to_owned())
}

fn remove_capture_file(path: &str) {
    if !path.is_empty() {
        let _ = std::fs::remove_file(path);
    }
}

fn action_transform(scale_x: f64, scale_y: f64) -> Json {
    json!({"scaleX": scale_x, "scaleY": scale_y, "offsetX": 0, "offsetY": 0})
}

// Bits shared with the input watchdog's modifiers field.
const SHIFT_BIT: u32 = 1;
const CTRL_BIT: u32 = 2;
const ALT_BIT: u32 = 4;
const WIN_BIT: u32 = 8;
const SPACE_BIT: u32 = 16;
const HELD_KEY_BITS: [(u32, VIRTUAL_KEY); 5] = [
    (SHIFT_BIT, VK_SHIFT),
    (CTRL_BIT, VK_CONTROL),
    (ALT_BIT, VK_MENU),
    (WIN_BIT, VK_LWIN),
    (SPACE_BIT, VK_SPACE),
];

fn key_bit(key: VIRTUAL_KEY) -> u32 {
    HELD_KEY_BITS
        .iter()
        .find(|(_, held)| *held == key)
        .map_or(0, |(bit, _)| *bit)
}

fn modifier_mask(keys: &[VIRTUAL_KEY]) -> u32 {
    keys.iter().fold(0, |mask, &key| mask | key_bit(key))
}

/// Toggle names are canonicalized by the dispatcher.
fn toggle_vk(key: &str) -> Option<VIRTUAL_KEY> {
    match key {
        "shift" => Some(VK_SHIFT),
        "ctrl" => Some(VK_CONTROL),
        "alt" => Some(VK_MENU),
        "win" => Some(VK_LWIN),
        "space" => Some(VK_SPACE),
        _ => None,
    }
}

fn toggle_names(mask: u32) -> String {
    [
        (SHIFT_BIT, "shift"),
        (CTRL_BIT, "ctrl"),
        (ALT_BIT, "alt"),
        (WIN_BIT, "win"),
        (SPACE_BIT, "space"),
    ]
    .iter()
    .filter(|(bit, _)| mask & bit != 0)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join(", ")
}

fn same_rect(first: &RECT, second: &RECT) -> bool {
    first.left == second.left
        && first.top == second.top
        && first.right == second.right
        && first.bottom == second.bottom
}

/// A request is served on a detached worker thread. Windows allows a
/// foreground change from that thread only when it shares the input queue
/// with the current foreground or target GUI thread. Attachments are
/// temporary: keeping one after activation would merge unrelated keyboard
/// state and would make cancellation and thread teardown unsafe.
#[derive(Default)]
struct ScopedThreadInputAttachments {
    attachments: Vec<(u32, u32)>,
}

impl ScopedThreadInputAttachments {
    const CAPACITY: usize = 2;

    fn attach(&mut self, first_thread: u32, second_thread: u32) -> bool {
        if first_thread == 0
            || second_thread == 0
            || first_thread == second_thread
            || self.attachments.len() == Self::CAPACITY
        {
            return false;
        }
        let already_attached = self.attachments.iter().any(|&(first, second)| {
            (first == first_thread && second == second_thread)
                || (first == second_thread && second == first_thread)
        });
        if already_attached {
            return true;
        }
        if !unsafe { AttachThreadInput(first_thread, second_thread, true) }.as_bool() {
            return false;
        }
        self.attachments.push((first_thread, second_thread));
        true
    }
}

impl Drop for ScopedThreadInputAttachments {
    fn drop(&mut self) {
        for &(first, second) in self.attachments.iter().rev() {
            unsafe {
                let _ = AttachThreadInput(first, second, false);
            }
        }
    }
}

/// AttachThreadInput requires both threads to have message queues. A
/// detached pipe worker may not have created one yet; PM_NOREMOVE leaves the
/// queue and all messages untouched.
fn ensure_message_queue() {
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
    }
}

#[derive(Clone, Copy)]
enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    const ALL: [MouseButton; 3] = [MouseButton::Left, MouseButton::Right, MouseButton::Middle];

    fn parse(name: &str) -> Result<Self, Error> {
        match if name.is_empty() {
            "left".to_owned()
        } else {
            name.to_ascii_lowercase()
        }
        .as_str()
        {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "middle" => Ok(Self::Middle),
            _ => Err(Error::new(
                "invalid_button",
                "button must be left, right, or middle",
            )),
        }
    }

    fn down_flag(self) -> MOUSE_EVENT_FLAGS {
        match self {
            Self::Left => MOUSEEVENTF_LEFTDOWN,
            Self::Right => MOUSEEVENTF_RIGHTDOWN,
            Self::Middle => MOUSEEVENTF_MIDDLEDOWN,
        }
    }

    fn up_flag(self) -> MOUSE_EVENT_FLAGS {
        match self {
            Self::Left => MOUSEEVENTF_LEFTUP,
            Self::Right => MOUSEEVENTF_RIGHTUP,
            Self::Middle => MOUSEEVENTF_MIDDLEUP,
        }
    }

    /// Shared with the input watchdog's buttons field.
    fn bit(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
        }
    }
}

fn modifier_vk(value: &str) -> Option<VIRTUAL_KEY> {
    match value.to_ascii_lowercase().as_str() {
        "shift" => Some(VK_SHIFT),
        "ctrl" | "control" => Some(VK_CONTROL),
        "alt" | "option" => Some(VK_MENU),
        "win" | "meta" | "super" => Some(VK_LWIN),
        _ => None,
    }
}

fn key_vk(value: &str) -> Result<VIRTUAL_KEY, Error> {
    let key = value.to_ascii_lowercase();
    let named = match key.as_str() {
        "backspace" => Some(VK_BACK),
        "tab" => Some(VK_TAB),
        "enter" | "return" => Some(VK_RETURN),
        "escape" | "esc" => Some(VK_ESCAPE),
        "space" => Some(VK_SPACE),
        "left" => Some(VK_LEFT),
        "right" => Some(VK_RIGHT),
        "up" => Some(VK_UP),
        "down" => Some(VK_DOWN),
        "home" => Some(VK_HOME),
        "end" => Some(VK_END),
        "pageup" => Some(VK_PRIOR),
        "pagedown" => Some(VK_NEXT),
        "delete" => Some(VK_DELETE),
        "insert" => Some(VK_INSERT),
        "pause" => Some(VK_PAUSE),
        "capslock" => Some(VK_CAPITAL),
        "numlock" => Some(VK_NUMLOCK),
        "printscreen" => Some(VK_SNAPSHOT),
        _ => None,
    };
    if let Some(named) = named {
        return Ok(named);
    }
    let function_key = |number: u8| VIRTUAL_KEY(VK_F1.0 + u16::from(number) - 1);
    match key.as_bytes() {
        [b'f', digit @ b'1'..=b'9'] => return Ok(function_key(digit - b'0')),
        // f10–f14 and f20–f24 only, the set earlier releases accepted.
        [b'f', tens @ b'1'..=b'2', ones @ b'0'..=b'4'] => {
            return Ok(function_key((tens - b'0') * 10 + ones - b'0'));
        }
        [character] => {
            let translated = unsafe { VkKeyScanW(u16::from(*character)) };
            if translated != -1 {
                return Ok(VIRTUAL_KEY(translated as u16 & 0xFF));
            }
        }
        _ => {}
    }
    Err(Error::new("invalid_key", format!("Unknown key: {value}")))
}

fn extended_key(key: VIRTUAL_KEY) -> bool {
    [
        VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_INSERT,
        VK_DELETE, VK_DIVIDE, VK_NUMLOCK, VK_LWIN, VK_RWIN,
    ]
    .contains(&key)
}

fn extended_flag(key: VIRTUAL_KEY) -> KEYBD_EVENT_FLAGS {
    if extended_key(key) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    }
}

fn last_error_code() -> u32 {
    unsafe { GetLastError() }.0
}

fn send_raw(input: INPUT) -> bool {
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 1 }
}

fn send_input(input: INPUT) -> Result<(), Error> {
    if send_raw(input) {
        return Ok(());
    }
    Err(Error::new(
        "input_failed",
        format!("Windows SendInput failed: {}", last_error_code()),
    ))
}

fn send_key(key: VIRTUAL_KEY, down: bool) -> Result<(), Error> {
    let direction = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    send_input(keyboard_input(key, 0, direction | extended_flag(key)))
}

fn release_key(key: VIRTUAL_KEY) -> bool {
    send_raw(keyboard_input(key, 0, KEYEVENTF_KEYUP | extended_flag(key)))
}

fn send_mouse_button(button: MouseButton, down: bool) -> Result<(), Error> {
    send_input(mouse_input(if down {
        button.down_flag()
    } else {
        button.up_flag()
    }))
}

fn send_mouse_move(point: POINT) -> Result<(), Error> {
    unsafe { SetCursorPos(point.x, point.y) }.map_err(|_| {
        Error::new(
            "input_failed",
            format!("SetCursorPos failed: {}", last_error_code()),
        )
    })
}

/// SetCursorPos only repositions the cursor; pointer-based UI such as WinUI
/// sliders ignores it while a button is held. A drag moves with injected input.
fn send_mouse_move_input(point: POINT) -> Result<(), Error> {
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    // Windows maps back by truncating, so round up to land on the same pixel.
    let normalize = |offset: i32, extent: i32| {
        let clamped = i64::from(offset.clamp(0, extent - 1));
        ((clamped * 65535 + i64::from(extent) - 2) / i64::from(extent - 1)) as i32
    };
    let mut input = mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
    input.Anonymous.mi.dx = normalize(point.x - left, width);
    input.Anonymous.mi.dy = normalize(point.y - top, height);
    send_input(input)
}

fn sleep_context(context: &Context, milliseconds: i32) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_millis(milliseconds.max(0) as u64);
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        context.check()?;
        let remaining_ms = (deadline - now).as_millis() as u64;
        std::thread::sleep(Duration::from_millis(remaining_ms.clamp(1, 10)));
    }
    context.check()
}

/// Holds modifier keys down for one action and releases them in reverse order.
struct ModifierGuard {
    keys: Vec<VIRTUAL_KEY>,
}

impl ModifierGuard {
    fn press(keys: &[VIRTUAL_KEY]) -> Result<Self, Error> {
        let guard = Self {
            keys: keys.to_vec(),
        };
        for &key in keys {
            // Dropping the guard releases every key, pressed or not.
            send_key(key, true)?;
        }
        Ok(guard)
    }
}

impl Drop for ModifierGuard {
    fn drop(&mut self) {
        for &key in self.keys.iter().rev() {
            release_key(key);
        }
    }
}

struct Screenshot {
    mime_type: &'static str,
    reduced: CapturedImage,
    full: CapturedImage,
}

struct Observation {
    /// The agent session that took it; another agent's indexes and screenshot
    /// pixels describe what that agent saw, not this one.
    owner: String,
    handle: WindowHandle,
    rect: RECT,
    created: Instant,
    elements: Vec<ElementRecord>,
    /// Each image's scale maps window-local coordinates to its pixels.
    screenshot: Option<Screenshot>,
}

impl Observation {
    fn capture_paths(&self) -> impl Iterator<Item = &str> {
        self.screenshot
            .iter()
            .flat_map(|shot| [shot.reduced.path.as_str(), shot.full.path.as_str()])
    }
}

/// Window-local point resolved from screenshot pixel coordinates.
struct CoordinateTransform {
    space: &'static str,
    scale_x: f64,
    scale_y: f64,
    image_width: i32,
    image_height: i32,
}

/// Layout changes an input caused, which the next accessibility observation
/// of that window waits for unless a scroll already confirmed them. The
/// watcher is armed before the input so no event the input causes is missed,
/// and becomes pending once the input completes.
struct PendingLayout {
    watcher: UiaChangeWatcher,
    input_at: Instant,
}

/// A press at `from`, a stepped move to `to`, and a release, all in screen pixels.
struct DragPlan {
    from: POINT,
    to: POINT,
    button: MouseButton,
    duration_ms: i32,
    steps: i32,
    hold_before_ms: i32,
    hold_after_ms: i32,
    modifier_keys: Vec<VIRTUAL_KEY>,
}

/// An input that animates or scrolls the page (an accordion, an anchor jump)
/// leaves the accessibility tree on the old layout for a while; Chrome raises
/// layout events about 0.35 seconds after the input and then in batches about
/// half a second apart. No event within the first window means the input
/// changed nothing that is still pending; otherwise the tree is read once the
/// events stop for longer than a batch interval.
fn wait_for_layout_to_settle(
    watcher: &UiaChangeWatcher,
    input_at: Instant,
    context: &Context,
) -> Result<bool, Error> {
    const FIRST_CHANGE_WINDOW: Duration = Duration::from_millis(700);
    const QUIET_PERIOD: Duration = Duration::from_millis(600);
    const MAXIMUM_WAIT: Duration = Duration::from_millis(2500);
    const POLL_MS: i32 = 20;
    loop {
        let now = Instant::now();
        // The watcher is armed before activation, whose own focus events must
        // not stand in for the layout changes of the input itself.
        let last_change = watcher.last_change().filter(|&change| change >= input_at);
        let settled = match last_change {
            Some(change) => now.duration_since(change) >= QUIET_PERIOD,
            None => now.duration_since(input_at) >= FIRST_CHANGE_WINDOW,
        };
        if settled {
            return Ok(true);
        }
        if now.duration_since(input_at) >= MAXIMUM_WAIT {
            return Ok(false);
        }
        sleep_context(context, POLL_MS)?;
    }
}

/// Chrome publishes new accessibility locations about half a second after a
/// wheel scroll, so an observation taken at once still lists the old layout
/// and a click on it lands on whatever scrolled into that place. Waiting
/// until the scroll position of the container under the pointer has changed
/// and held still lets the next observation see the scrolled page. A scroll
/// that moves nothing (the end of the page) waits out the timeout and
/// reports false.
fn wait_for_scroll_to_settle(
    container: Option<&windows::Win32::UI::Accessibility::IUIAutomationScrollPattern>,
    before: ScrollPosition,
    context: &Context,
) -> Result<bool, Error> {
    const TIMEOUT: Duration = Duration::from_millis(1000);
    const STABLE_FOR: Duration = Duration::from_millis(150);
    const POLL_MS: i32 = 50;
    let mut last = before;
    let mut moved_at = None;
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        sleep_context(context, POLL_MS)?;
        let Some(position) = uia::scroll_position(container) else {
            return Ok(false);
        };
        if position != last {
            last = position;
            moved_at = Some(Instant::now());
        } else if moved_at.is_some_and(|moved: Instant| moved.elapsed() >= STABLE_FOR) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A page that scrolled or reflowed since the observation keeps the window
/// geometry, so the observation stays valid while its element bounds point at
/// whatever moved into that place. The point therefore comes from the bounds
/// the provider reports now, clipped to its scroll viewports and the window:
/// only the visible part can take the input, content scrolled under a browser
/// toolbar would hand the click to the toolbar, and a degenerate rectangle
/// would centre on the window corner.
fn visible_element_center(record: &ElementRecord, target: &WindowInfo) -> Result<POINT, Error> {
    let bounds = uia::visible_bounds(&record.element, &target.rect).ok_or_else(|| {
        Error::new(
            "stale_observation",
            "Element is no longer available; observe again",
        )
    })?;
    let window = RECT {
        left: 0,
        top: 0,
        right: target.rect.right - target.rect.left,
        bottom: target.rect.bottom - target.rect.top,
    };
    let visible = intersect(&bounds, &window).ok_or_else(|| {
        Error::new(
            "element_not_actionable",
            "Element has no visible clickable area; scroll it into view and observe again",
        )
    })?;
    Ok(POINT {
        x: (visible.left + visible.right) / 2,
        y: (visible.top + visible.bottom) / 2,
    })
}

fn point_json(point: POINT) -> Json {
    json!({"x": point.x, "y": point.y})
}

fn require_element<'a>(
    params: &Json,
    observation: &'a Observation,
    key: &str,
) -> Result<&'a ElementRecord, Error> {
    let index = params
        .get(key)
        .filter(|value| value.is_i64() || value.is_u64())
        .ok_or_else(|| Error::new("element_required", format!("{key} is required")))?;
    let index = index.as_i64().map_or_else(
        || index.as_u64().unwrap_or_default() as i32,
        |value| value as i32,
    );
    usize::try_from(index)
        .ok()
        .and_then(|index| observation.elements.get(index))
        .ok_or_else(|| {
            Error::new(
                "element_not_found",
                "Element index is not present in the observation",
            )
        })
}

fn doctor() -> Json {
    let access = windows::Win32::System::StationsAndDesktops::DESKTOP_ACCESS_FLAGS(GENERIC_READ.0);
    let desktop = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access) };
    let interactive = desktop.is_ok();
    if let Ok(desktop) = desktop {
        unsafe {
            let _ = CloseDesktop(desktop);
        }
    }
    let notes = if interactive {
        json!([])
    } else {
        json!(["An unlocked interactive desktop is required"])
    };
    json!({
        "platform": "windows",
        "interactiveDesktop": interactive,
        "uia": true,
        "windowsGraphicsCapture": true,
        "d3d11": true,
        "wic": true,
        "stopKey": STOP_KEY,
        "edgeBorder": interactive,
        "indicator": interactive,
        "ready": interactive,
        "notes": notes,
    })
}

fn capabilities() -> Json {
    json!({
        "platform": "windows",
        "supports": {
            "screenshot": true, "accessibility": true, "wgc": true, "jpeg": true, "png": true,
            "drag": true, "nativeApps": true, "games": true, "overlay": true, "hotkeyStop": true,
            "stopKey": STOP_KEY, "edgeBorder": true,
        },
        "coordinateSpace": "reduced",
        "headless": {
            "click": true, "setValue": true, "scroll": true, "setState": true, "scrollTo": true, "selectOption": true,
        },
        "multiAgent": {
            "maxAgents": 16, "concurrentObservation": true, "concurrentHeadless": true, "physicalInput": "one at a time",
        },
        "defaults": {
            "durationMs": 240, "steps": 12, "holdBeforeMs": 50, "holdAfterMs": 50, "quality": 85, "maxEdge": 0,
        },
    })
}

fn read_clipboard_text() -> Vec<u16> {
    unsafe {
        let Ok(data) = GetClipboardData(u32::from(CF_UNICODETEXT.0)) else {
            return Vec::new();
        };
        let memory = windows::Win32::Foundation::HGLOBAL(data.0);
        let locked = GlobalLock(memory).cast::<u16>();
        if locked.is_null() {
            return Vec::new();
        }
        let length = (0..).take_while(|&offset| *locked.add(offset) != 0).count();
        let text = std::slice::from_raw_parts(locked, length).to_vec();
        let _ = GlobalUnlock(memory);
        text
    }
}

/// Copies `text` and a terminating NUL into clipboard-owned memory; false
/// when the memory cannot be allocated.
fn set_clipboard_text(text: &[u16]) -> bool {
    let bytes = (text.len() + 1) * std::mem::size_of::<u16>();
    unsafe {
        let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let locked = GlobalLock(memory).cast::<u16>();
        if !locked.is_null() {
            std::ptr::copy_nonoverlapping(text.as_ptr(), locked, text.len());
            *locked.add(text.len()) = 0;
            let _ = GlobalUnlock(memory);
        }
        let _ = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0)));
    }
    true
}

pub(crate) struct WindowsBackend {
    this: Weak<WindowsBackend>,
    /// Shared so an action keeps its observation alive while another agent's
    /// observation evicts it from the cache.
    observations: Mutex<HashMap<String, Arc<Observation>>>,
    indicator: SessionIndicator,
    /// The capture keeps one frame pool for its current window, so captures
    /// take turns; the slower accessibility walk does not.
    capture: Mutex<Mta<WindowCapture>>,
    running_contexts: Mutex<HashMap<u64, Context>>,
    next_context_id: AtomicU64,
    session_context: Mutex<Option<Context>>,
    session_active: AtomicBool,
    session_id: Mutex<String>,
    held_buttons: AtomicU32,
    /// Modifiers held for the duration of one action. Toggled keys live in
    /// `toggled_keys` so a per-action store never clobbers them; the watchdog
    /// receives the union.
    held_modifiers: AtomicU32,
    toggled_keys: AtomicU32,
    pending_layouts: Mutex<HashMap<WindowHandle, PendingLayout>>,
    held_key: AtomicU16,
    held_key_flags: AtomicU32,
    watchdog: InputWatchdog,
}

pub(crate) fn make_backend() -> Arc<WindowsBackend> {
    Arc::new_cyclic(|this| WindowsBackend {
        this: this.clone(),
        observations: Mutex::new(HashMap::new()),
        indicator: SessionIndicator::new(),
        capture: Mutex::new(Mta(WindowCapture::new())),
        running_contexts: Mutex::new(HashMap::new()),
        next_context_id: AtomicU64::new(1),
        session_context: Mutex::new(None),
        session_active: AtomicBool::new(false),
        session_id: Mutex::new(String::new()),
        held_buttons: AtomicU32::new(0),
        held_modifiers: AtomicU32::new(0),
        toggled_keys: AtomicU32::new(0),
        pending_layouts: Mutex::new(HashMap::new()),
        held_key: AtomicU16::new(0),
        held_key_flags: AtomicU32::new(0),
        watchdog: InputWatchdog::start(),
    })
}

/// Keeps a request's context cancellable by `interrupt` while it runs.
struct RunningContext<'a> {
    backend: &'a WindowsBackend,
    id: u64,
}

impl Drop for RunningContext<'_> {
    fn drop(&mut self) {
        self.backend.running_contexts.lock().remove(&self.id);
    }
}

/// Clears the per-action modifiers when a drag ends, however it ends.
struct HeldModifiersReset<'a>(&'a WindowsBackend);

impl Drop for HeldModifiersReset<'_> {
    fn drop(&mut self) {
        self.0.held_modifiers.store(0, Ordering::Release);
        self.0.publish_input_state();
    }
}

/// Releases a held drag button, however the drag ends.
struct HeldButtonGuard<'a> {
    backend: &'a WindowsBackend,
    button: MouseButton,
}

impl Drop for HeldButtonGuard<'_> {
    fn drop(&mut self) {
        if send_raw(mouse_input(self.button.up_flag())) {
            self.backend
                .held_buttons
                .fetch_and(!self.button.bit(), Ordering::Release);
        }
        self.backend.publish_input_state();
    }
}

impl WindowsBackend {
    fn track(&self, context: &Context) -> RunningContext<'_> {
        let id = self.next_context_id.fetch_add(1, Ordering::Relaxed);
        self.running_contexts.lock().insert(id, context.clone());
        RunningContext { backend: self, id }
    }

    fn start_session(&self, params: &Json, context: &Context) -> Result<Json, Error> {
        if self.session_active.load(Ordering::Acquire) {
            return Err(Error::new(
                "session_active",
                "A computer-use session is already active",
            ));
        }
        let requested = params
            .get("sessionId")
            .and_then(Json::as_str)
            .map_or_else(guid_string, str::to_owned);
        let backend = self.this.clone();
        self.indicator
            .start(Box::new(move || {
                if let Some(backend) = backend.upgrade() {
                    backend.on_stop_key();
                }
            }))
            .map_err(|error| Error::new("setup_required", error))?;
        *self.session_id.lock() = requested.clone();
        *self.session_context.lock() = Some(context.clone());
        self.session_active.store(true, Ordering::Release);
        Ok(json!({
            "ready": true,
            "sessionId": requested,
            "indicator": {
                "ready": true, "hotkey": STOP_KEY, "stopKey": STOP_KEY,
                "clickThrough": true, "cursorHighlight": true, "edgeBorder": true,
            },
        }))
    }

    fn ensure_session(&self, params: &Json) -> Result<(), Error> {
        if !self.session_active.load(Ordering::Acquire) {
            return Err(Error::new(
                "session_required",
                "Start a computer-use session first",
            ));
        }
        let supplied = json_string(params, "sessionId");
        if !supplied.is_empty() && supplied != *self.session_id.lock() {
            return Err(Error::new(
                "wrong_session",
                "The request does not own the active session",
            ));
        }
        Ok(())
    }

    fn target_window(&self, params: &Json) -> Option<WindowInfo> {
        let windows = enumerate_windows();
        let id = json_string(params, "windowId");
        if !id.is_empty() {
            let handle = parse_window_id(&id);
            let info = windows
                .into_iter()
                .find(|info| Some(info.handle) == handle)?;
            let app = json_string(params, "app");
            if !app.is_empty() && !app_name_matches(&info, &app.to_ascii_lowercase()) {
                return None;
            }
            return Some(info);
        }
        // Owned dialogs precede their owner in Z-order; an app or pid selects
        // the main window, and an observation of it reports a blocking modal.
        let first = |matches: &dyn Fn(&WindowInfo) -> bool| {
            windows
                .iter()
                .find(|info| info.owner.is_none() && matches(info))
                .or_else(|| {
                    windows
                        .iter()
                        .find(|info| info.owner.is_some() && matches(info))
                })
                .cloned()
        };
        let app = json_string(params, "app").to_ascii_lowercase();
        if !app.is_empty() {
            if let Some(info) = first(&|info| app_name_matches(info, &app)) {
                return Some(info);
            }
            let pid = app
                .strip_prefix("pid:")
                .and_then(|pid| parse_leading_unsigned(pid, u64::from(u32::MAX)))?;
            return first(&|info| u64::from(info.pid) == pid);
        }
        let foreground = WindowHandle::of(unsafe { GetForegroundWindow() });
        if let Some(info) = windows.iter().find(|info| info.handle == foreground) {
            return Some(info.clone());
        }
        first(&|_| true)
    }

    fn require_target_window(&self, params: &Json) -> Result<WindowInfo, Error> {
        self.target_window(params).ok_or_else(|| {
            Error::new(
                "window_not_found",
                "No matching visible top-level window was found",
            )
        })
    }

    fn purge_observations(&self) {
        let now = Instant::now();
        let mut removed = Vec::new();
        {
            let mut observations = self.observations.lock();
            observations.retain(|_, observation| {
                let expired = now.duration_since(observation.created) > OBSERVATION_LIFETIME;
                if expired {
                    removed.extend(observation.capture_paths().map(str::to_owned));
                }
                !expired
            });
            while observations.len() > MAXIMUM_OBSERVATIONS {
                let Some(oldest) = observations
                    .iter()
                    .min_by_key(|(_, observation)| observation.created)
                    .map(|(id, _)| id.clone())
                else {
                    break;
                };
                if let Some(observation) = observations.remove(&oldest) {
                    removed.extend(observation.capture_paths().map(str::to_owned));
                }
            }
        }
        for path in removed {
            remove_capture_file(&path);
        }
    }

    fn clear_observations(&self) {
        let removed: Vec<Arc<Observation>> = self
            .observations
            .lock()
            .drain()
            .map(|(_, observation)| observation)
            .collect();
        for observation in removed {
            for path in observation.capture_paths() {
                remove_capture_file(path);
            }
        }
    }

    fn coordinate_transform(
        &self,
        params: &Json,
        target: &WindowInfo,
        observation: Option<&Observation>,
    ) -> Result<CoordinateTransform, Error> {
        let full = json_string_or(params, "coords", "reduced") == "full";
        let missing_screenshot = || {
            Error::new(
                "observation_required",
                "Observe the window with a screenshot before clicking by coordinates",
            )
        };
        let transform = |screenshot: &Screenshot| {
            let image = if full {
                &screenshot.full
            } else {
                &screenshot.reduced
            };
            CoordinateTransform {
                space: if full { "full" } else { "reduced" },
                scale_x: image.scale_x,
                scale_y: image.scale_y,
                image_width: image.width,
                image_height: image.height,
            }
        };
        if let Some(observation) = observation {
            return observation
                .screenshot
                .as_ref()
                .map(transform)
                .ok_or_else(missing_screenshot);
        }

        self.purge_observations();
        let observations = self.observations.lock();
        // Without an observationId, pixel coordinates refer to the most recent
        // screenshot this agent took of this window.
        let owner = json_string(params, "agentSessionId");
        let source = observations
            .values()
            .filter(|candidate| {
                candidate.screenshot.is_some()
                    && candidate.handle == target.handle
                    && candidate.owner == owner
            })
            .max_by_key(|candidate| candidate.created)
            .ok_or_else(missing_screenshot)?;
        let old = &source.rect;
        if old.right - old.left != target.rect.right - target.rect.left
            || old.bottom - old.top != target.rect.bottom - target.rect.top
        {
            return Err(Error::new(
                "stale_observation",
                "Window size changed since the last screenshot; observe again",
            ));
        }
        source
            .screenshot
            .as_ref()
            .map(transform)
            .ok_or_else(missing_screenshot)
    }

    fn require_observation(
        &self,
        params: &Json,
        target: &WindowInfo,
    ) -> Result<Arc<Observation>, Error> {
        let id = json_string(params, "observationId");
        if id.is_empty() {
            return Err(Error::new(
                "observation_required",
                "An observationId is required for indexed input",
            ));
        }
        self.purge_observations();
        let observations = self.observations.lock();
        let observation = observations
            .get(&id)
            .ok_or_else(|| Error::new("stale_observation", "Observation is missing or expired"))?;
        if observation.owner != json_string(params, "agentSessionId") {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to another agent session; observe again",
            ));
        }
        if observation.handle != target.handle
            || !same_rect(&observation.rect, &target.rect)
            || !target.handle.exists()
        {
            return Err(Error::new(
                "stale_observation",
                "Window geometry changed; observe again",
            ));
        }
        Ok(observation.clone())
    }

    fn activate(&self, handle: WindowHandle, context: &Context) -> Result<(), Error> {
        let window_gone = || Error::new("window_not_found", "The target window no longer exists");
        if !handle.exists() {
            return Err(window_gone());
        }
        let hwnd = handle.hwnd();
        if unsafe { IsIconic(hwnd) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
        context.check()?;
        let deadline = Instant::now() + ACTIVATION_TIMEOUT;

        // Try the normal path first. It avoids changing input queues when the
        // daemon already owns the foreground permission.
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        if foreground_matches(handle) {
            return Ok(());
        }

        // One condition under which Windows allows a foreground change is that
        // the caller received the last input event. Once the user types in
        // another app, that event no longer belongs to this daemon, so an
        // empty mouse event, which moves and presses nothing, takes it back
        // (issue #15).
        send_raw(mouse_input(MOUSE_EVENT_FLAGS(0)));
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        if foreground_matches(handle) {
            return Ok(());
        }

        // The dispatcher handles requests on detached worker threads.
        // Establish a temporary relationship with the foreground and target
        // GUI threads so SetForegroundWindow can pass the foreground-lock
        // check. The scope detaches on every return path, including
        // cancellation.
        ensure_message_queue();
        let request_thread = unsafe { GetCurrentThreadId() };
        let foreground_window = unsafe { GetForegroundWindow() };
        let foreground_thread = if foreground_window.is_invalid() {
            0
        } else {
            unsafe { GetWindowThreadProcessId(foreground_window, None) }
        };
        let target_thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
        let mut attachments = ScopedThreadInputAttachments::default();
        attachments.attach(request_thread, foreground_thread);
        attachments.attach(request_thread, target_thread);

        context.check()?;
        if !handle.exists() {
            return Err(window_gone());
        }
        unsafe {
            let _ = BringWindowToTop(hwnd);
            let _ = SetActiveWindow(hwnd);
            let _ = SetForegroundWindow(hwnd);
        }
        while Instant::now() < deadline {
            context.check()?;
            if !handle.exists() {
                return Err(window_gone());
            }
            if foreground_matches(handle) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(Error::new(
            "focus_denied",
            "The target window could not be brought to the foreground; Windows may be blocking the change because \
             the current foreground window is elevated or locked. Observe again and retry, or ask the user to switch \
             to the target window",
        ))
    }

    fn observe(&self, params: &Json, context: &Context) -> Result<Json, Error> {
        self.purge_observations();
        let mut target = self.require_target_window(params)?;
        // A disabled owner cannot take focus, so activation is skipped and the
        // observation points at the dialog that does.
        let modal = blocking_modal(&target);
        if modal.is_none() && json_bool(params, "activate", false) {
            // Backgrounded UWP/WinUI windows collapse their UIA tree, so an
            // opt-in activation exists. It restores the window and can move
            // it, so the rectangle every coordinate below is built from must
            // be re-read.
            self.activate(target.handle, context)?;
            if let Some(activated) = inspect_window(target.handle) {
                target = activated;
            }
        }
        let observe_mode = json_string(params, "observe");
        let observe_text = json_bool(params, "includeText", false)
            || observe_mode == "text"
            || observe_mode == "both";
        let include_screenshot =
            json_bool(params, "includeScreenshot", true) && observe_mode != "text";
        let format = if json_string_or(params, "format", "jpeg").eq_ignore_ascii_case("png") {
            "png"
        } else {
            "jpeg"
        };
        let quality = json_int(params, "quality", 85).clamp(1, 100);
        // maxEdge is an optional cap on the reduced image only; 0 means no cap.
        let max_edge = json_int(params, "maxEdge", 0).max(0);
        let id = guid_string();
        let started = Instant::now();

        let mut result = json!({
            "observationId": id,
            "coordinateSpace": "window",
            "window": window_json(&target),
            "overlayRegions": [],
            "timings": {},
        });
        if let Some(modal) = &modal {
            let modal_id = modal.handle.id();
            result["modal"] = modal_json(modal);
            result["notice"] = json!(format!(
                "This window is blocked by modal dialog {modal_id} \"{}\". The dialog is a separate window and is not \
                 in this screenshot or accessibility tree; observe and act on --window-id {modal_id} instead.",
                modal.title
            ));
        }
        // A modal disables the owner, which makes every window it owns modal,
        // so modeless popups exist only when no modal is reported.
        let popups = if modal.is_some() {
            Vec::new()
        } else {
            modeless_owned_windows(target.handle)
        };
        if !popups.is_empty() {
            let names: Vec<String> = popups
                .iter()
                .map(|popup| format!("{} \"{}\"", popup.handle.id(), popup.title))
                .collect();
            result["ownedWindows"] = Json::Array(popups.iter().map(window_json).collect());
            result["notice"] = json!(format!(
                "Owned windows {} are separate windows and may be missing from this window's screenshot; observe \
                 --window-id to read or act on them.",
                names.join(", ")
            ));
        }
        // Waiting comes before the screenshot so both describe the same
        // layout. A screenshot-only observation shows the pixels as they are
        // and never waits.
        let pending = if observe_text {
            self.pending_layouts.lock().remove(&target.handle)
        } else {
            None
        };
        if let Some(pending) = pending.filter(|pending| pending.watcher.watching()) {
            let settle_started = Instant::now();
            result["layoutSettled"] = json!(wait_for_layout_to_settle(
                &pending.watcher,
                pending.input_at,
                context
            )?);
            drop(pending);
            result["timings"]["settleMs"] = json!(settle_started.elapsed().as_millis() as u64);
        }

        let mut captured_at_unix_ms = 0;
        let mut screenshot = None;
        if include_screenshot {
            let capture_started = Instant::now();
            let full_path = private_capture_path(&id, format, "-full")?;
            let reduced_path = private_capture_path(&id, format, "")?;
            let request = CaptureRequest {
                full_path: &full_path,
                reduced_path: &reduced_path,
                format,
                quality,
                max_edge,
            };
            let shot = self.capture.lock().capture(
                target.handle.hwnd(),
                &target.rect,
                &request,
                context,
            )?;
            result["screenshot"] = json!({
                "path": shot.path,
                "mimeType": shot.mime_type,
                "width": shot.width,
                "height": shot.height,
                "variant": "reduced",
                "actionTransform": action_transform(shot.scale_x, shot.scale_y),
                "fullAvailable": true,
                "fullWidth": shot.full.width,
                "fullHeight": shot.full.height,
                "scale": shot.scale,
                "backend": shot.backend,
                "frameTimestamp100ns": shot.frame_timestamp_100ns,
                "freshFrame": shot.fresh_frame,
                "cachedFrame": shot.cached_frame,
                "sourceWidth": shot.source_width,
                "sourceHeight": shot.source_height,
                "scaleX": shot.scale_x,
                "scaleY": shot.scale_y,
            });
            captured_at_unix_ms = shot.captured_at_unix_ms;
            screenshot = Some(Screenshot {
                mime_type: shot.mime_type,
                reduced: shot.reduced,
                full: shot.full,
            });
            result["timings"]["captureMs"] = json!(capture_started.elapsed().as_millis() as u64);
        }
        result["capturedAt"] = json!(iso8601_from_unix_ms(captured_at_unix_ms));
        result["observedAt"] = json!(iso8601_now());
        let mut observation = Observation {
            owner: json_string(params, "agentSessionId"),
            handle: target.handle,
            rect: target.rect,
            created: Instant::now(),
            elements: Vec::new(),
            screenshot,
        };
        if observe_text {
            let accessibility_started = Instant::now();
            match uia::build_snapshot(target.handle.hwnd(), &target.rect, context) {
                Ok(accessibility) => {
                    result["accessibility"] = accessibility.value;
                    observation.elements = accessibility.elements;
                }
                Err(error) => {
                    // The observation is not cached, so nothing else would delete its images.
                    for path in observation.capture_paths() {
                        remove_capture_file(path);
                    }
                    return Err(error);
                }
            }
            result["timings"]["accessibilityMs"] =
                json!(accessibility_started.elapsed().as_millis() as u64);
        }
        result["timings"]["totalMs"] = json!(started.elapsed().as_millis() as u64);
        result["timings"]["accessibilityIncluded"] = json!(observe_text);
        self.observations.lock().insert(id, Arc::new(observation));
        Ok(result)
    }

    fn full_screenshot(&self, params: &Json) -> Result<Json, Error> {
        let id = json_string(params, "observationId");
        if id.is_empty() {
            return Err(Error::new(
                "observation_required",
                "get-full-screenshot requires observationId",
            ));
        }
        self.purge_observations();
        let target = self.target_window(params);
        let observations = self.observations.lock();
        let observation = observations.get(&id).ok_or_else(|| {
            Error::new(
                "stale_observation",
                "Observation is missing or expired; observe again",
            )
        })?;
        if observation.owner != json_string(params, "agentSessionId") {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to another agent session; observe again",
            ));
        }
        if target.is_some_and(|target| target.handle != observation.handle) {
            return Err(Error::new(
                "stale_observation",
                "Observation belongs to a different window; observe again",
            ));
        }
        let screenshot = observation
            .screenshot
            .as_ref()
            .filter(|shot| std::path::Path::new(&shot.full.path).exists())
            .ok_or_else(|| {
                Error::new(
                    "stale_observation",
                    "Observation has no full screenshot; observe again with a screenshot",
                )
            })?;
        Ok(json!({
            "observationId": id,
            "screenshot": {
                "path": screenshot.full.path,
                "mimeType": screenshot.mime_type,
                "width": screenshot.full.width,
                "height": screenshot.full.height,
                "variant": "full",
                "actionTransform": action_transform(screenshot.full.scale_x, screenshot.full.scale_y),
            },
            "notice": "To click a point read from this image, pass --coords full.",
        }))
    }

    fn complete_action(&self, target: WindowHandle, armed: &mut Option<UiaChangeWatcher>) -> Json {
        // The next observation must not consume a frame captured before this
        // action completed. Capture keeps this watermark independently of the
        // dispatcher so a follow-up observe can wait for a new frame.
        self.capture.lock().mark_input_complete();
        if let Some(watcher) = armed.take() {
            let input_at = Instant::now();
            let mut pending = self.pending_layouts.lock();
            // Each pending entry keeps UIA event handlers registered; a window
            // nobody observes again must not hold them forever.
            if pending.len() >= MAXIMUM_PENDING_WINDOWS && !pending.contains_key(&target) {
                let oldest = pending
                    .iter()
                    .min_by_key(|(_, layout)| layout.input_at)
                    .map(|(handle, _)| *handle);
                if let Some(oldest) = oldest {
                    pending.remove(&oldest);
                }
            }
            pending.insert(target, PendingLayout { watcher, input_at });
        }
        json!({"delivered": true, "verification": {"state": "unverified"}})
    }

    fn cancel_running_contexts(&self) {
        for context in self.running_contexts.lock().values() {
            context.cancel();
        }
    }

    fn cancel_session_context(&self) {
        if let Some(context) = &*self.session_context.lock() {
            context.cancel();
        }
    }

    fn modifiers(&self, params: &Json) -> Result<Vec<VIRTUAL_KEY>, Error> {
        let names: Vec<&str> = match params.get("modifiers") {
            Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
            Some(Json::String(value)) => split_on_plus(value),
            _ => Vec::new(),
        };
        let mut keys = Vec::new();
        for name in names {
            let key = modifier_vk(name)
                .ok_or_else(|| Error::new("invalid_key", format!("Unknown modifier: {name}")))?;
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    fn click_at(
        &self,
        handle: WindowHandle,
        point: POINT,
        button: MouseButton,
        params: &Json,
        context: &Context,
    ) -> Result<(), Error> {
        self.activate(handle, context)?;
        send_mouse_move(point)?;
        let modifier_keys = self.without_toggled(self.modifiers(params)?);
        self.held_modifiers
            .store(modifier_mask(&modifier_keys), Ordering::Release);
        self.publish_input_state();
        // Publish before the down event so a process termination in the tiny
        // gap cannot leave a button held without the watchdog knowing about it.
        self.held_buttons.fetch_or(button.bit(), Ordering::Release);
        self.publish_input_state();
        let mut button_down = false;
        let clicked = (|| {
            let _modifiers = ModifierGuard::press(&modifier_keys)?;
            context.check()?;
            send_mouse_button(button, true)?;
            button_down = true;
            send_mouse_button(button, false)?;
            button_down = false;
            self.held_buttons
                .fetch_and(!button.bit(), Ordering::Release);
            self.publish_input_state();
            Ok(())
        })();
        if clicked.is_err() && !button_down {
            self.held_buttons
                .fetch_and(!button.bit(), Ordering::Release);
        }
        self.held_modifiers.store(0, Ordering::Release);
        self.publish_input_state();
        clicked
    }

    fn drag_at(
        &self,
        handle: WindowHandle,
        plan: &DragPlan,
        context: &Context,
    ) -> Result<(), Error> {
        let DragPlan {
            from, to, button, ..
        } = *plan;
        let modifier_keys = plan.modifier_keys.as_slice();
        self.activate(handle, context)?;
        send_mouse_move_input(from)?;
        context.check()?;
        if !foreground_matches(handle) {
            return Err(Error::new(
                "focus_lost",
                "Target window lost foreground focus before drag",
            ));
        }
        // Modifiers are held for the whole drag. Declared before the button
        // guard so the button is released first, then the modifiers.
        self.held_modifiers
            .store(modifier_mask(modifier_keys), Ordering::Release);
        self.publish_input_state();
        let _modifiers_reset = HeldModifiersReset(self);
        let _modifiers = ModifierGuard::press(modifier_keys)?;
        context.check()?;
        self.held_buttons.fetch_or(button.bit(), Ordering::Release);
        self.publish_input_state();
        if let Err(error) = send_mouse_button(button, true) {
            self.held_buttons
                .fetch_and(!button.bit(), Ordering::Release);
            self.publish_input_state();
            return Err(error);
        }
        let _button = HeldButtonGuard {
            backend: self,
            button,
        };
        sleep_context(context, plan.hold_before_ms)?;
        let steps = plan.steps.max(1);
        let duration = f64::from(plan.duration_ms);
        for step in 1..=steps {
            context.check()?;
            if !foreground_matches(handle) {
                return Err(Error::new(
                    "focus_lost",
                    "Target window lost foreground focus during drag",
                ));
            }
            let fraction = f64::from(step) / f64::from(steps);
            let x = (f64::from(from.x) + f64::from(to.x - from.x) * fraction).round() as i32;
            let y = (f64::from(from.y) + f64::from(to.y - from.y) * fraction).round() as i32;
            send_mouse_move_input(POINT { x, y })?;
            let elapsed = (duration * f64::from(step) / f64::from(steps)).round() as i32;
            let previous = (duration * f64::from(step - 1) / f64::from(steps)).round() as i32;
            sleep_context(context, elapsed - previous)?;
        }
        sleep_context(context, plan.hold_after_ms)
    }

    fn send_chord(&self, chord: &str, context: &Context) -> Result<(), Error> {
        let parts = split_on_plus(chord);
        if parts.is_empty() {
            return Err(Error::new("invalid_key", "Empty key chord"));
        }
        let (modifier_keys, bases): (Vec<_>, Vec<_>) = parts
            .iter()
            .map(|part| (modifier_vk(part), *part))
            .partition(|(modifier, _)| modifier.is_some());
        let [(_, base)] = bases.as_slice() else {
            return Err(Error::new(
                "invalid_key",
                "A key chord needs one non-modifier key",
            ));
        };
        let base = key_vk(base)?;
        let toggled = key_bit(base) & self.toggled_keys.load(Ordering::Acquire);
        if toggled != 0 {
            let names = toggle_names(toggled);
            return Err(Error::new(
                "toggles_active",
                format!(
                    "{names} is toggled on; pressing it again would release it. Use `dcu toggle off --key {names}` instead"
                ),
            ));
        }
        let modifier_keys = self.without_toggled(
            modifier_keys
                .into_iter()
                .filter_map(|(modifier, _)| modifier)
                .collect(),
        );
        self.held_modifiers
            .store(modifier_mask(&modifier_keys), Ordering::Release);
        self.publish_input_state();
        let mut base_down = false;
        let pressed = (|| {
            let _modifiers = ModifierGuard::press(&modifier_keys)?;
            context.check()?;
            self.held_key.store(base.0, Ordering::Release);
            self.held_key_flags
                .store(extended_flag(base).0, Ordering::Release);
            self.publish_input_state();
            send_key(base, true)?;
            base_down = true;
            send_key(base, false)?;
            base_down = false;
            self.clear_held_key();
            self.publish_input_state();
            Ok(())
        })();
        if pressed.is_err() && !base_down {
            self.clear_held_key();
        }
        self.held_modifiers.store(0, Ordering::Release);
        self.publish_input_state();
        pressed
    }

    fn clear_held_key(&self) {
        self.held_key.store(0, Ordering::Release);
        self.held_key_flags.store(0, Ordering::Release);
    }

    fn send_text(&self, text: &str, context: &Context) -> Result<(), Error> {
        for character in text.encode_utf16() {
            context.check()?;
            self.held_key.store(character, Ordering::Release);
            self.held_key_flags
                .store(KEYEVENTF_UNICODE.0, Ordering::Release);
            self.publish_input_state();
            send_input(keyboard_input(VIRTUAL_KEY(0), character, KEYEVENTF_UNICODE)).inspect_err(
                |_| {
                    self.clear_held_key();
                    self.publish_input_state();
                },
            )?;
            send_input(keyboard_input(
                VIRTUAL_KEY(0),
                character,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ))?;
            self.clear_held_key();
            self.publish_input_state();
        }
        Ok(())
    }

    fn paste_text(&self, text: &str, context: &Context) -> Result<(), Error> {
        let text: Vec<u16> = text.encode_utf16().collect();
        if unsafe { OpenClipboard(None) }.is_err() {
            return Err(Error::new("clipboard_failed", "OpenClipboard failed"));
        }
        let previous = read_clipboard_text();
        unsafe {
            let _ = EmptyClipboard();
        }
        let copied = set_clipboard_text(&text);
        unsafe {
            let _ = CloseClipboard();
        }
        if !copied {
            return Err(Error::new(
                "clipboard_failed",
                "GlobalAlloc for clipboard text failed",
            ));
        }
        self.send_chord("ctrl+v", context)?;
        sleep_context(context, 150)?;
        if unsafe { OpenClipboard(None) }.is_ok() {
            unsafe {
                let _ = EmptyClipboard();
            }
            if !previous.is_empty() {
                set_clipboard_text(&previous);
            }
            unsafe {
                let _ = CloseClipboard();
            }
        }
        Ok(())
    }

    fn handle_action(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        const INPUT_METHODS: [&str; 11] = [
            "click",
            "drag",
            "scroll",
            "type-text",
            "press-key",
            "hotkey",
            "set-value",
            "paste-text",
            "set-state",
            "scroll-to",
            "select-option",
        ];
        if !INPUT_METHODS.contains(&method) {
            return Err(Error::new(
                "unsupported_method",
                format!("Unsupported Windows computer-use method: {method}"),
            ));
        }
        let target = self.require_target_window(params)?;
        // Windows discards input to a disabled owner, so it would only look delivered.
        reject_blocked_target(&target)?;
        // The dispatcher has already refused headless input that needs the
        // real pointer or keyboard, so a headless branch below never
        // activates the window, moves the cursor, or injects keys.
        self.pending_layouts.lock().remove(&target.handle);
        // An input that fails leaves nothing pending, because only
        // complete_action moves the armed watcher into pending_layouts.
        let mut armed = Some(UiaChangeWatcher::new(target.handle.hwnd()));
        let observation = if json_string(params, "observationId").is_empty() {
            None
        } else {
            Some(self.require_observation(params, &target)?)
        };
        let action = Action {
            backend: self,
            params,
            context,
            target: &target,
            observation: observation.as_deref(),
        };
        let mut complete = || self.complete_action(target.handle, &mut armed);
        match method {
            "click" => action.click(&mut complete),
            "drag" => action.drag(&mut complete),
            "scroll" => action.scroll(&mut complete),
            "type-text" => action.type_text(&mut complete),
            "press-key" | "hotkey" => action.press_key(&mut complete),
            "set-value" => action.set_value(&mut complete),
            // set-state, scroll-to, and select-option never activate the
            // window, even outside headless mode, because the dispatcher runs
            // them without the physical-input turn.
            "select-option" => action.select_option(&mut complete),
            "set-state" => action.set_state(&mut complete),
            "scroll-to" => action.scroll_to(&mut complete),
            _ => action.paste(&mut complete),
        }
    }

    fn list_apps(&self) -> Json {
        let mut apps: BTreeMap<String, Json> = BTreeMap::new();
        for info in enumerate_windows() {
            let key = info.app.to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            let app = apps
                .entry(key)
                .or_insert_with(|| json!({"id": info.app, "name": info.app, "windows": []}));
            if let Some(windows) = app["windows"].as_array_mut() {
                windows.push(window_json(&info));
            }
        }
        json!({"apps": apps.into_values().collect::<Vec<_>>()})
    }

    fn list_windows(&self, params: &Json) -> Json {
        let wanted = json_string(params, "app").to_ascii_lowercase();
        let windows: Vec<Json> = enumerate_windows()
            .iter()
            .filter(|info| wanted.is_empty() || app_name_matches(info, &wanted))
            .map(window_json)
            .collect();
        json!({"windows": windows})
    }

    fn stop_session(&self) {
        self.session_active.store(false, Ordering::Release);
        self.cancel_running_contexts();
        self.cancel_session_context();
        self.release_held_input();
        self.indicator.stop();
        self.clear_observations();
        self.pending_layouts.lock().clear();
        self.session_id.lock().clear();
        *self.session_context.lock() = None;
        self.capture.lock().reset();
    }

    fn on_stop_key(&self) {
        self.session_active.store(false, Ordering::Release);
        self.cancel_running_contexts();
        self.cancel_session_context();
        self.release_held_input();
    }

    fn publish_input_state(&self) {
        self.watchdog.publish(
            self.held_buttons.load(Ordering::Acquire),
            self.held_modifiers.load(Ordering::Acquire) | self.toggled_keys.load(Ordering::Acquire),
            self.held_key.load(Ordering::Acquire),
            self.held_key_flags.load(Ordering::Acquire),
        );
    }

    fn release_held_input(&self) {
        let buttons = self.held_buttons.load(Ordering::Acquire);
        for button in MouseButton::ALL {
            if buttons & button.bit() != 0 && send_raw(mouse_input(button.up_flag())) {
                self.held_buttons.fetch_and(!button.bit(), Ordering::AcqRel);
            }
        }
        let modifiers = self.held_modifiers.load(Ordering::Acquire);
        for (bit, key) in [
            (SHIFT_BIT, VK_SHIFT),
            (CTRL_BIT, VK_CONTROL),
            (ALT_BIT, VK_MENU),
            (WIN_BIT, VK_LWIN),
        ] {
            if modifiers & bit != 0 && release_key(key) {
                self.held_modifiers.fetch_and(!bit, Ordering::AcqRel);
            }
        }
        self.release_toggled_keys();
        let key = self.held_key.load(Ordering::Acquire);
        let key_flags = KEYBD_EVENT_FLAGS(self.held_key_flags.load(Ordering::Acquire));
        if key != 0 {
            let release = if key_flags.contains(KEYEVENTF_UNICODE) {
                keyboard_input(VIRTUAL_KEY(0), key, key_flags | KEYEVENTF_KEYUP)
            } else {
                keyboard_input(VIRTUAL_KEY(key), 0, key_flags | KEYEVENTF_KEYUP)
            };
            if send_raw(release) {
                self.clear_held_key();
            }
        }
        self.publish_input_state();
    }

    /// A toggled key is already down and must stay down after this action.
    fn without_toggled(&self, mut keys: Vec<VIRTUAL_KEY>) -> Vec<VIRTUAL_KEY> {
        let toggled = self.toggled_keys.load(Ordering::Acquire);
        keys.retain(|&key| key_bit(key) & toggled == 0);
        keys
    }

    fn update_toggled(&self, bit: u32, on: bool) {
        let mask = if on {
            self.toggled_keys.fetch_or(bit, Ordering::AcqRel) | bit
        } else {
            self.toggled_keys.fetch_and(!bit, Ordering::AcqRel) & !bit
        };
        // While a key is toggled, the user's Escape still counts toward the
        // stop but is not delivered, so Ctrl+Esc or Win+Esc cannot fire.
        indicator::set_swallow_user_escape(mask != 0);
        self.publish_input_state();
    }

    fn release_toggled_keys(&self) {
        let toggled = self.toggled_keys.load(Ordering::Acquire);
        for (bit, key) in HELD_KEY_BITS {
            if toggled & bit == 0 {
                continue;
            }
            // The state is dropped even if SendInput fails: the session is
            // ending and the watchdog cannot do better than this release attempt.
            release_key(key);
            self.update_toggled(bit, false);
        }
        if self.toggled_keys.load(Ordering::Acquire) == 0 {
            indicator::set_swallow_user_escape(false);
        }
    }
}

/// One input action against a resolved target window.
struct Action<'a> {
    backend: &'a WindowsBackend,
    params: &'a Json,
    context: &'a Context,
    target: &'a WindowInfo,
    observation: Option<&'a Observation>,
}

type Complete<'a> = dyn FnMut() -> Json + 'a;

impl Action<'_> {
    fn headless(&self) -> bool {
        json_bool(self.params, "headless", false)
    }

    fn observation(&self, message: &str) -> Result<&Observation, Error> {
        self.observation
            .ok_or_else(|| Error::new("observation_required", message.to_owned()))
    }

    fn element(&self, missing_observation: &str, key: &str) -> Result<&ElementRecord, Error> {
        require_element(self.params, self.observation(missing_observation)?, key)
    }

    fn target_size(&self) -> (i32, i32) {
        (
            self.target.rect.right - self.target.rect.left,
            self.target.rect.bottom - self.target.rect.top,
        )
    }

    fn screen_point(&self, local: POINT) -> POINT {
        POINT {
            x: self.target.rect.left + local.x,
            y: self.target.rect.top + local.y,
        }
    }

    fn transform(&self) -> Result<CoordinateTransform, Error> {
        self.backend
            .coordinate_transform(self.params, self.target, self.observation)
    }

    /// x/y are screenshot pixels (reduced by default, full with coords=full);
    /// the observation's transform converts them to window-local points.
    fn window_point(
        &self,
        x_key: &str,
        y_key: &str,
        transform: &CoordinateTransform,
    ) -> Result<POINT, Error> {
        let (target_width, target_height) = self.target_size();
        let read = |key: &str, image_limit: i32, scale: f64, window_limit: i32| {
            let value = self
                .params
                .get(key)
                .and_then(Json::as_f64)
                .ok_or_else(|| Error::invalid_argument(format!("{key} is required")))?;
            if !value.is_finite() || value < 0.0 || value > f64::from(image_limit) {
                return Err(Error::invalid_argument(format!(
                    "{key} is outside the {} screenshot",
                    transform.space
                )));
            }
            Ok(((value / scale).round() as i32).clamp(0, window_limit))
        };
        Ok(POINT {
            x: read(
                x_key,
                transform.image_width,
                transform.scale_x,
                target_width,
            )?,
            y: read(
                y_key,
                transform.image_height,
                transform.scale_y,
                target_height,
            )?,
        })
    }

    fn semantic_result(complete: &mut Complete<'_>, action: &str) -> Json {
        let mut result = complete();
        result["delivery"] = json!("semantic");
        if !action.is_empty() {
            result["semanticAction"] = json!(action);
        }
        result
    }

    fn button(&self) -> Result<MouseButton, Error> {
        MouseButton::parse(&json_string(self.params, "button"))
    }

    fn click(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let (backend, context) = (self.backend, self.context);
        if self.params.get("elementIndex").is_some() {
            let record = self.element("elementIndex requires observationId", "elementIndex")?;
            if self.headless() {
                let action = uia::perform_semantic_action(record, context)?.ok_or_else(|| {
                    Error::new(
                        "headless_unavailable",
                        "The element exposes no accessibility action (Invoke, SelectionItem, Toggle, ExpandCollapse, \
                         or a LegacyIAccessible default action); pass headless false to click it with the pointer",
                    )
                })?;
                return Ok(Self::semantic_result(complete, action));
            }
            backend.activate(self.target.handle, context)?;
            if let Some(action) = uia::invoke(record, context)? {
                return Ok(Self::semantic_result(complete, action));
            }
            let local = visible_element_center(record, self.target)?;
            let button = self.button()?;
            backend.click_at(
                self.target.handle,
                self.screen_point(local),
                button,
                self.params,
                context,
            )?;
            let mut result = complete();
            result["windowPoint"] = point_json(local);
            return Ok(result);
        }
        let transform = self.transform()?;
        let local = self.window_point("x", "y", &transform)?;
        let button = self.button()?;
        backend.click_at(
            self.target.handle,
            self.screen_point(local),
            button,
            self.params,
            context,
        )?;
        let mut result = complete();
        result["coordinateSpace"] = json!(transform.space);
        result["windowPoint"] = point_json(local);
        Ok(result)
    }

    fn drag(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let mut transform = None;
        let mut coordinate =
            |x_key: &str, y_key: &str, element_key: &str| -> Result<POINT, Error> {
                if self.params.get(element_key).is_some() {
                    let record =
                        self.element("Element drag requires observationId", element_key)?;
                    return visible_element_center(record, self.target);
                }
                if transform.is_none() {
                    transform = Some(self.transform()?);
                }
                let transform = transform.as_ref().expect("transform resolved above");
                self.window_point(x_key, y_key, transform)
            };
        let local_from = coordinate("fromX", "fromY", "fromElementIndex")?;
        let local_to = coordinate("toX", "toY", "toElementIndex")?;
        let plan = DragPlan {
            from: self.screen_point(local_from),
            to: self.screen_point(local_to),
            button: self.button()?,
            duration_ms: json_int(self.params, "durationMs", 240).max(0),
            steps: json_int(self.params, "steps", 12).max(1),
            hold_before_ms: json_int(self.params, "holdBeforeMs", 50).max(0),
            hold_after_ms: json_int(self.params, "holdAfterMs", 50).max(0),
            modifier_keys: self
                .backend
                .without_toggled(self.backend.modifiers(self.params)?),
        };
        self.backend
            .drag_at(self.target.handle, &plan, self.context)?;
        let mut result = complete();
        if let Some(transform) = transform {
            result["coordinateSpace"] = json!(transform.space);
            result["windowFrom"] = point_json(local_from);
            result["windowTo"] = point_json(local_to);
        }
        Ok(result)
    }

    fn scroll(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let (backend, context) = (self.backend, self.context);
        let direction = json_string_or(self.params, "direction", "down").to_ascii_lowercase();
        let amount = json_int(self.params, "amount", 3).clamp(-100, 100);
        let horizontal = direction == "left" || direction == "right";
        let (target_width, target_height) = self.target_size();
        let mut local = POINT {
            x: target_width / 2,
            y: target_height / 2,
        };
        let mut transform = None;
        let mut record = None;
        if self.params.get("elementIndex").is_some() {
            record = Some(self.element("elementIndex requires observationId", "elementIndex")?);
        } else if self.params.get("x").is_some() || self.params.get("y").is_some() {
            let resolved = self.transform()?;
            local = self.window_point("x", "y", &resolved)?;
            transform = Some(resolved);
        }
        if self.headless() {
            let container = match record {
                Some(record) => uia::scroll_container_of(&record.element),
                None => uia::scroll_container_at(self.screen_point(local)),
            };
            // One wheel notch scrolls three lines by default; a small increment is one line.
            const LINES_PER_NOTCH: i32 = 3;
            let position_before = uia::scroll_position(container.as_ref());
            let forward = direction == "down" || direction == "right";
            let applied = uia::scroll(container.as_ref(), horizontal, forward, amount * LINES_PER_NOTCH, context)?
                .ok_or_else(|| {
                    Error::new(
                        "headless_unavailable",
                        format!(
                            "No container there exposes a ScrollPattern that scrolls {}; pass headless false to scroll \
                             with the mouse wheel",
                            if horizontal { "horizontally" } else { "vertically" }
                        ),
                    )
                })?;
            let settled = match position_before {
                Some(before) if applied > 0 => {
                    wait_for_scroll_to_settle(container.as_ref(), before, context)?
                }
                _ => false,
            };
            let mut result = Self::semantic_result(complete, "scroll");
            // The scroll already watched its container, or nothing moved.
            backend.pending_layouts.lock().remove(&self.target.handle);
            result["layoutSettled"] = json!(settled);
            if let Some(transform) = transform {
                result["coordinateSpace"] = json!(transform.space);
            }
            return Ok(result);
        }
        if let Some(record) = record {
            local = visible_element_center(record, self.target)?;
        }
        backend.activate(self.target.handle, context)?;
        let screen_point = self.screen_point(local);
        send_mouse_move(screen_point)?;
        let container = uia::scroll_container_at(screen_point);
        let position_before = uia::scroll_position(container.as_ref());
        let (flags, sign) = if horizontal {
            (MOUSEEVENTF_HWHEEL, if direction == "left" { -1 } else { 1 })
        } else {
            (MOUSEEVENTF_WHEEL, if direction == "up" { 1 } else { -1 })
        };
        let mut input = mouse_input(flags);
        input.Anonymous.mi.mouseData = (sign * amount * WHEEL_DELTA as i32) as u32;
        send_input(input)?;
        // Without a scrollable container (games, canvases) there is nothing to
        // watch, so the scroll returns at once and reports no settle state.
        let layout_settled = match position_before {
            Some(before) => Some(wait_for_scroll_to_settle(
                container.as_ref(),
                before,
                context,
            )?),
            None => None,
        };
        let mut result = complete();
        if let Some(transform) = transform {
            result["coordinateSpace"] = json!(transform.space);
        }
        result["windowPoint"] = point_json(local);
        if let Some(settled) = layout_settled {
            // The scroll already watched its container for up to a second.
            backend.pending_layouts.lock().remove(&self.target.handle);
            result["layoutSettled"] = json!(settled);
        }
        Ok(result)
    }

    fn type_text(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let shortcut_toggles =
            self.backend.toggled_keys.load(Ordering::Acquire) & (CTRL_BIT | ALT_BIT | WIN_BIT);
        if shortcut_toggles != 0 {
            return Err(Error::new(
                "toggles_active",
                format!(
                    "type-text is refused while {} is toggled on because the text would become shortcuts; release it \
                     with `dcu toggle off --key KEY` first",
                    toggle_names(shortcut_toggles)
                ),
            ));
        }
        self.backend.activate(self.target.handle, self.context)?;
        self.backend
            .send_text(&json_string(self.params, "text"), self.context)?;
        Ok(complete())
    }

    fn press_key(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        self.backend.activate(self.target.handle, self.context)?;
        let mut chord = json_string(self.params, "key");
        if chord.is_empty() {
            chord = json_string(self.params, "keys");
        }
        self.backend.send_chord(&chord, self.context)?;
        Ok(complete())
    }

    fn set_value(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let record = self.element("set-value requires observationId", "elementIndex")?;
        if !self.headless() {
            self.backend.activate(self.target.handle, self.context)?;
        }
        if !uia::set_value(record, &json_string(self.params, "value"), self.context)? {
            return Err(Error::new(
                "pattern_unavailable",
                "The target element has no writable ValuePattern or, for a number, RangeValuePattern",
            ));
        }
        Ok(Self::semantic_result(complete, ""))
    }

    fn select_option(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let record = self.element("select-option requires observationId", "elementIndex")?;
        let option = json_string(self.params, "option");
        let change = uia::select_option(record, &option, self.context)?;
        let mut result = Self::semantic_result(complete, change.pattern);
        result["option"] = json!(option);
        result["changed"] = json!(change.changed);
        result["verified"] = json!(change.verified);
        Ok(result)
    }

    fn set_state(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let record = self.element("set-state requires observationId", "elementIndex")?;
        let state = json_string(self.params, "state");
        let change = uia::set_state(record, &state, self.context)?.ok_or_else(|| {
            Error::new(
                "pattern_unavailable",
                format!(
                    "The target element has no {} for state {state}",
                    pattern_for_state(&state)
                ),
            )
        })?;
        let mut result = Self::semantic_result(complete, change.pattern);
        result["state"] = json!(state);
        result["changed"] = json!(change.changed);
        result["verified"] = json!(change.verified);
        Ok(result)
    }

    fn scroll_to(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        let context = self.context;
        let record = self.element("scroll-to requires observationId", "elementIndex")?;
        let (target_width, target_height) = self.target_size();
        let window = RECT {
            left: 0,
            top: 0,
            right: target_width,
            bottom: target_height,
        };
        let visible_in_window = || {
            uia::visible_bounds(&record.element, &self.target.rect)
                .and_then(|bounds| intersect(&bounds, &window))
                .is_some()
        };
        // ScrollIntoView can return before the provider publishes the new location.
        let wait_until_visible = || -> Result<bool, Error> {
            const SETTLE_MS: i32 = 800;
            const POLL_MS: i32 = 25;
            let mut visible = visible_in_window();
            let mut waited = 0;
            while !visible && waited < SETTLE_MS {
                sleep_context(context, POLL_MS)?;
                visible = visible_in_window();
                waited += POLL_MS;
            }
            Ok(visible)
        };

        let has_scroll_item = uia::scroll_into_view(record, context)?;
        let mut visible = has_scroll_item && wait_until_visible()?;
        let mut action = "scrollIntoView";
        if !visible {
            let shown_by_ancestor = uia::scroll_ancestor_into_view(record, context)?;
            if shown_by_ancestor.is_none() && !has_scroll_item {
                return Err(Error::new(
                    "pattern_unavailable",
                    "The target element has no ScrollItemPattern and no ancestor with a ScrollPattern",
                ));
            }
            if let Some(shown) = shown_by_ancestor {
                action = "scrollPattern";
                visible = shown && wait_until_visible()?;
            }
        }
        let mut result = Self::semantic_result(complete, action);
        result["visible"] = json!(visible);
        Ok(result)
    }

    fn paste(&self, complete: &mut Complete<'_>) -> Result<Json, Error> {
        self.backend.activate(self.target.handle, self.context)?;
        self.backend
            .paste_text(&json_string(self.params, "text"), self.context)?;
        Ok(complete())
    }
}

impl Backend for WindowsBackend {
    /// Observations and headless actions of different agents run on their
    /// own request threads at once; the dispatcher keeps physical input,
    /// toggles, and session start/stop to one call at a time.
    fn supports_concurrent_execute(&self) -> bool {
        true
    }

    fn execute(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error> {
        let _running = self.track(context);
        let com = ComScope::enter();
        if !com.usable {
            return Err(Error::new("com_unavailable", "COM initialization failed"));
        }
        match method {
            "doctor" => Ok(doctor()),
            "capabilities" => Ok(capabilities()),
            "session.status" => Ok(json!({
                "active": self.session_active.load(Ordering::Acquire),
                "sessionId": *self.session_id.lock(),
                "indicatorReady": self.indicator.ready(),
                "hotkey": STOP_KEY,
                "stopKey": STOP_KEY,
            })),
            "session.start" => self.start_session(params, context),
            "session.stop" | "daemon.shutdown" => {
                self.stop_session();
                Ok(json!({"stopped": true}))
            }
            _ => {
                self.ensure_session(params)?;
                match method {
                    "list-apps" => Ok(self.list_apps()),
                    "list-windows" => Ok(self.list_windows(params)),
                    "get-app-state" => self.observe(params, context),
                    "get-full-screenshot" => self.full_screenshot(params),
                    _ => self.handle_action(method, params, context),
                }
            }
        }
    }

    fn interrupt(&self) {
        self.cancel_running_contexts();
        self.cancel_session_context();
        self.release_held_input();
        self.indicator.request_stop_async();
    }

    fn set_toggle(&self, key: &str, down: bool, context: &Context) -> Result<(), Error> {
        if !self.session_active.load(Ordering::Acquire) {
            return Err(Error::new(
                "session_required",
                "Start a computer-use session first",
            ));
        }
        let virtual_key = toggle_vk(key)
            .ok_or_else(|| Error::invalid_argument(format!("Unknown toggle key: {key}")))?;
        let bit = key_bit(virtual_key);
        if !down {
            send_key(virtual_key, false)?;
            self.update_toggled(bit, false);
            return Ok(());
        }
        context.check()?;
        // Publish before the down event so a concurrent stop or a crash always
        // knows to release the key.
        self.update_toggled(bit, true);
        send_key(virtual_key, true).inspect_err(|_| self.update_toggled(bit, false))
    }

    fn release_toggles(&self) {
        self.release_toggled_keys();
    }
}

impl Drop for WindowsBackend {
    fn drop(&mut self) {
        self.interrupt();
        self.indicator.stop();
        self.release_held_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_splitting_drops_only_a_trailing_empty_part() {
        assert_eq!(split_on_plus("ctrl+shift+a"), ["ctrl", "shift", "a"]);
        assert_eq!(split_on_plus("ctrl+"), ["ctrl"]);
        assert_eq!(split_on_plus("+a"), ["", "a"]);
        assert_eq!(split_on_plus("a++b"), ["a", "", "b"]);
        assert!(split_on_plus("").is_empty());
    }

    #[test]
    fn window_and_pid_numbers_parse_like_stoull_and_stoul() {
        assert_eq!(parse_leading_unsigned("123abc", u64::MAX), Some(123));
        assert_eq!(parse_leading_unsigned("  +42", u64::MAX), Some(42));
        assert_eq!(parse_leading_unsigned("-1", u64::MAX), Some(u64::MAX));
        assert_eq!(
            parse_leading_unsigned("-1", u64::from(u32::MAX)),
            Some(u64::from(u32::MAX))
        );
        assert_eq!(
            parse_leading_unsigned("4294967296", u64::from(u32::MAX)),
            None
        );
        assert_eq!(parse_leading_unsigned("abc", u64::MAX), None);
        assert_eq!(parse_leading_unsigned("", u64::MAX), None);
    }

    #[test]
    fn function_keys_map_f1_to_f14_and_f20_to_f24_only() {
        assert_eq!(key_vk("F1").unwrap(), VK_F1);
        assert_eq!(key_vk("f12").unwrap(), VIRTUAL_KEY(VK_F1.0 + 11));
        assert_eq!(key_vk("f24").unwrap(), VIRTUAL_KEY(VK_F1.0 + 23));
        assert_eq!(key_vk("f15").unwrap_err().code, "invalid_key");
        assert_eq!(key_vk("f25").unwrap_err().message, "Unknown key: f25");
    }

    #[test]
    fn toggle_names_list_held_keys_in_canonical_order() {
        assert_eq!(
            toggle_names(SPACE_BIT | SHIFT_BIT | WIN_BIT),
            "shift, win, space"
        );
        assert_eq!(toggle_names(0), "");
    }

    #[test]
    fn capture_timestamps_format_as_utc_iso8601_with_milliseconds() {
        assert_eq!(
            iso8601_from_unix_ms(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
    }

    #[test]
    fn observation_ids_look_like_braced_uppercase_guids() {
        let id = guid_string();

        assert_eq!(id.len(), 38);
        assert!(id.starts_with('{') && id.ends_with('}'));
        assert_eq!(id, id.to_ascii_uppercase());
    }
}

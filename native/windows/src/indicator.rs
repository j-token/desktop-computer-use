//! A non-activating, click-through topmost overlay that makes an active
//! computer-use session visible to the person at the desktop. The overlay is
//! kept on its own message thread so the low-level Escape hook and repainting
//! never block the daemon's operation threads.

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreatePen, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, Ellipse, EnumDisplayMonitors, FF_DONTCARE, FW_SEMIBOLD, GetDC, GetMonitorInfoW,
    GetStockObject, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO, NULL_BRUSH, OUT_DEFAULT_PRECIS,
    PS_SOLID, Polygon, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWLP_ID, GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics,
    GetWindowLongPtrW, HC_ACTION, HHOOK, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, IsWindow,
    KBDLLHOOKSTRUCT, KillTimer, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LoadCursorW,
    MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, TranslateMessage, ULW_ALPHA,
    UnhookWindowsHookEx, UnregisterClassW, UpdateLayeredWindow, WH_KEYBOARD_LL, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_KEYDOWN, WM_KEYUP, WM_MOUSEACTIVATE,
    WM_NCCREATE, WM_NCHITTEST, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, w};

/// Shared with the window enumeration, which hides the overlay from agents.
pub(crate) const WINDOW_CLASS: &str = "DesktopComputerUse.SessionIndicator.1";
const WINDOW_CLASS_WIDE: PCWSTR = w!("DesktopComputerUse.SessionIndicator.1");
const TIMER_ID: usize = 1;
const REPAINT_INTERVAL_MS: u32 = 33;
const ESCAPE_MESSAGE: u32 = WM_APP + 0x51;
const CURSOR_WINDOW_ID: isize = 100;
const EDGE_THICKNESS: i32 = 24;
const BANNER_WIDTH: i32 = 420;
const BANNER_HEIGHT: i32 = 72;
const CURSOR_SIZE: i32 = 56;
/// Two physical Escape presses within this window stop the session.
const DOUBLE_ESCAPE_WINDOW_MS: u32 = 1000;

const BANNER_TEXT: &str = "Computer use active  |  Esc ×2 to stop";
const ESCAPE_AGAIN_TEXT: &str = "Press Esc again to stop";

/// Banner of the running indicator, which the keyboard hook posts Escape to.
static ACTIVE_BANNER: AtomicIsize = AtomicIsize::new(0);
static ESCAPE_DOWN: AtomicBool = AtomicBool::new(false);
// While set, a physical Escape is still counted toward the emergency stop but
// is not passed on to applications, so a toggled Ctrl or Win cannot turn it
// into an OS shortcut.
static SWALLOW_USER_ESCAPE: AtomicBool = AtomicBool::new(false);

/// Enabled while any modifier toggle is on.
pub(crate) fn set_swallow_user_escape(enabled: bool) {
    SWALLOW_USER_ESCAPE.store(enabled, Ordering::Release);
}

fn hwnd_value(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

fn hwnd_from(value: isize) -> HWND {
    HWND(value as *mut _)
}

struct TransientText {
    text: String,
    until: Instant,
}

struct Shared {
    banner: AtomicIsize,
    transient: Mutex<TransientText>,
    running: AtomicBool,
    ready: AtomicBool,
}

impl Shared {
    fn show_transient_text(&self, text: &str, duration: Duration) {
        let mut transient = self.transient.lock();
        transient.text = text.to_owned();
        transient.until = Instant::now() + duration;
    }

    fn banner_text(&self) -> String {
        let mut transient = self.transient.lock();
        if !transient.text.is_empty() && Instant::now() < transient.until {
            return transient.text.clone();
        }
        transient.text.clear();
        BANNER_TEXT.to_owned()
    }

    fn post_close(&self) {
        let banner = self.banner.load(Ordering::Acquire);
        if banner != 0 {
            unsafe {
                let _ = PostMessageW(Some(hwnd_from(banner)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub(crate) type StopCallback = Box<dyn Fn() + Send>;

pub(crate) struct SessionIndicator {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SessionIndicator {
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                banner: AtomicIsize::new(0),
                transient: Mutex::new(TransientText {
                    text: String::new(),
                    until: Instant::now(),
                }),
                running: AtomicBool::new(false),
                ready: AtomicBool::new(false),
            }),
            thread: Mutex::new(None),
        }
    }

    /// `on_stop` runs on the indicator thread for the second physical Escape
    /// press within the double-press window. It must be short and nonblocking.
    pub(crate) fn start(&self, on_stop: StopCallback) -> Result<(), String> {
        self.stop();
        let (started_sender, started) = mpsc::channel();
        let shared = self.shared.clone();
        let handle = thread::spawn(move || run_overlay_thread(shared, on_stop, started_sender));
        *self.thread.lock() = Some(handle);
        match started.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.stop();
                Err(error)
            }
            Err(_) => {
                self.stop();
                Err("Unable to create session indicator".to_owned())
            }
        }
    }

    pub(crate) fn stop(&self) {
        self.shared.post_close();
        let handle = self.thread.lock().take();
        // The stop callback runs on the indicator thread, and a backend it
        // drops there stops this indicator; that thread cannot join itself.
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
        self.shared.running.store(false, Ordering::Release);
        self.shared.ready.store(false, Ordering::Release);
    }

    /// Posts the close instead of joining, so it is safe from any thread,
    /// including the hook callback on the indicator thread.
    pub(crate) fn request_stop_async(&self) {
        self.shared.post_close();
    }

    pub(crate) fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn banner_text(&self) -> String {
        self.shared.banner_text()
    }
}

impl Drop for SessionIndicator {
    fn drop(&mut self) {
        self.stop();
    }
}

/// State owned by the indicator thread. Window procedures reach it through
/// GWLP_USERDATA, possibly reentrantly, so it is only borrowed shared and its
/// cells are never borrowed across a Win32 call.
struct Overlay {
    shared: Arc<Shared>,
    on_stop: StopCallback,
    instance: HINSTANCE,
    banner: Cell<HWND>,
    cursor: Cell<HWND>,
    edges: RefCell<Vec<HWND>>,
    // The pending first Escape of a double press.
    escape_armed: Cell<bool>,
    first_escape_time_ms: Cell<u32>,
}

const OVERLAY_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(
    WS_EX_LAYERED.0
        | WS_EX_TRANSPARENT.0
        | WS_EX_NOACTIVATE.0
        | WS_EX_TOOLWINDOW.0
        | WS_EX_TOPMOST.0,
);

impl Overlay {
    fn create_window(&self, title: PCWSTR, rect: RECT) -> Option<HWND> {
        unsafe {
            CreateWindowExW(
                OVERLAY_STYLE,
                WINDOW_CLASS_WIDE,
                title,
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(self.instance),
                Some(std::ptr::from_ref(self).cast()),
            )
        }
        .ok()
    }

    fn destroy_edges(&self) {
        let edges = std::mem::take(&mut *self.edges.borrow_mut());
        for edge in edges {
            destroy_if_window(edge);
        }
    }

    fn rebuild_edges(&self) -> bool {
        let monitors = monitor_rectangles();
        if monitors.is_empty() {
            return false;
        }
        self.destroy_edges();

        let mut created = Vec::with_capacity(monitors.len() * 4);
        for monitor in &monitors {
            for side in Side::ALL {
                let geometry = edge_rect(monitor, side);
                let has_area = geometry.right > geometry.left && geometry.bottom > geometry.top;
                let edge = if has_area {
                    self.create_window(w!("Desktop computer use edge"), geometry)
                } else {
                    None
                };
                let Some(edge) = edge else {
                    for window in created {
                        destroy_if_window(window);
                    }
                    return false;
                };
                unsafe {
                    SetWindowLongPtrW(edge, GWLP_ID, created.len() as isize + 1);
                }
                created.push(edge);
            }
        }
        *self.edges.borrow_mut() = created;
        true
    }

    fn repaint(&self) {
        let banner = self.banner.get();
        let cursor = self.cursor.get();
        if banner.is_invalid() || cursor.is_invalid() {
            return;
        }
        let monitors = monitor_rectangles();
        if monitors.is_empty() {
            return;
        }
        if self.edges.borrow().len() != monitors.len() * 4 && !self.rebuild_edges() {
            return;
        }
        let edges = self.edges.borrow().clone();

        paint_banner(banner, &self.shared.banner_text());
        paint_cursor(cursor);
        for (edge, (monitor, side)) in edges.iter().zip(
            monitors
                .iter()
                .flat_map(|monitor| Side::ALL.map(|side| (monitor, side))),
        ) {
            paint_edge(*edge, monitor, side);
        }
    }

    fn handle_escape(&self, event_time_ms: u32) {
        // Wrapping subtraction stays correct across the 49.7-day tick wrap.
        let within_window =
            event_time_ms.wrapping_sub(self.first_escape_time_ms.get()) <= DOUBLE_ESCAPE_WINDOW_MS;
        if self.escape_armed.get() && within_window {
            self.escape_armed.set(false);
            // Escape is an asynchronous emergency stop; a failing callback
            // must never tear down the message loop.
            let _ = panic::catch_unwind(AssertUnwindSafe(|| (self.on_stop)()));
            self.shared.post_close();
            return;
        }
        // A single press, or one after the window expired, only arms the stop.
        self.escape_armed.set(true);
        self.first_escape_time_ms.set(event_time_ms);
        self.shared.show_transient_text(
            ESCAPE_AGAIN_TEXT,
            Duration::from_millis(u64::from(DOUBLE_ESCAPE_WINDOW_MS)),
        );
        self.repaint();
    }
}

fn destroy_if_window(hwnd: HWND) {
    unsafe {
        if !hwnd.is_invalid() && IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn last_error_code() -> u32 {
    unsafe { GetLastError() }.0
}

fn run_overlay_thread(
    shared: Arc<Shared>,
    on_stop: StopCallback,
    started: mpsc::Sender<Result<(), String>>,
) {
    shared.transient.lock().text.clear();
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .unwrap_or_default();
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        hInstance: instance,
        lpfnWndProc: Some(indicator_window_procedure),
        lpszClassName: WINDOW_CLASS_WIDE,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        let registration_error = last_error_code();
        if registration_error != ERROR_CLASS_ALREADY_EXISTS.0 {
            let _ = started.send(Err(format!(
                "RegisterClassExW failed: {registration_error}"
            )));
            return;
        }
    }

    let overlay = Box::new(Overlay {
        shared: shared.clone(),
        on_stop,
        instance,
        banner: Cell::new(HWND::default()),
        cursor: Cell::new(HWND::default()),
        edges: RefCell::new(Vec::new()),
        escape_armed: Cell::new(false),
        first_escape_time_ms: Cell::new(0),
    });
    let keyboard_hook = match create_overlay_windows(&overlay) {
        Ok(hook) => hook,
        Err(error) => {
            teardown(&overlay, None);
            let _ = started.send(Err(error));
            return;
        }
    };

    let banner = overlay.banner.get();
    shared.banner.store(hwnd_value(banner), Ordering::Release);
    shared.ready.store(true, Ordering::Release);
    shared.running.store(true, Ordering::Release);
    ESCAPE_DOWN.store(false, Ordering::Release);
    ACTIVE_BANNER.store(hwnd_value(banner), Ordering::Release);
    let _ = started.send(Ok(()));
    overlay.repaint();

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    teardown(&overlay, Some(keyboard_hook));
}

/// Creates the banner, cursor marker, and edges, then installs the Escape hook
/// and the repaint timer.
fn create_overlay_windows(overlay: &Overlay) -> Result<HHOOK, String> {
    let banner_rect = RECT {
        left: 0,
        top: 0,
        right: BANNER_WIDTH,
        bottom: BANNER_HEIGHT,
    };
    let banner = overlay
        .create_window(w!("Desktop computer use"), banner_rect)
        .ok_or_else(|| format!("CreateWindowExW failed: {}", last_error_code()))?;
    overlay.banner.set(banner);

    let cursor_rect = RECT {
        left: 0,
        top: 0,
        right: CURSOR_SIZE,
        bottom: CURSOR_SIZE,
    };
    let cursor = overlay
        .create_window(w!("Desktop computer use cursor"), cursor_rect)
        .ok_or_else(|| format!("CreateWindowExW for cursor failed: {}", last_error_code()))?;
    overlay.cursor.set(cursor);
    unsafe {
        SetWindowLongPtrW(cursor, GWLP_ID, CURSOR_WINDOW_ID);
    }
    if !overlay.rebuild_edges() {
        return Err("Unable to create a click-through edge on every monitor".to_owned());
    }

    let keyboard_hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            Some(overlay.instance),
            0,
        )
    }
    .map_err(|_| {
        format!(
            "SetWindowsHookExW(WH_KEYBOARD_LL) failed: {}",
            last_error_code()
        )
    })?;
    if unsafe { SetTimer(Some(banner), TIMER_ID, REPAINT_INTERVAL_MS, None) } == 0 {
        let code = last_error_code();
        unsafe {
            let _ = UnhookWindowsHookEx(keyboard_hook);
        }
        return Err(format!("SetTimer failed: {code}"));
    }
    Ok(keyboard_hook)
}

fn teardown(overlay: &Overlay, keyboard_hook: Option<HHOOK>) {
    let banner = overlay.banner.get();
    let _ =
        ACTIVE_BANNER.compare_exchange(hwnd_value(banner), 0, Ordering::AcqRel, Ordering::Acquire);
    if let Some(hook) = keyboard_hook {
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
    }
    overlay.destroy_edges();
    destroy_if_window(overlay.cursor.get());
    destroy_if_window(banner);
    overlay.shared.banner.store(0, Ordering::Release);
    overlay.shared.running.store(false, Ordering::Release);
    overlay.shared.ready.store(false, Ordering::Release);
    unsafe {
        let _ = UnregisterClassW(WINDOW_CLASS_WIDE, Some(overlay.instance));
    }
}

unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    message: WPARAM,
    data: LPARAM,
) -> LRESULT {
    if code == HC_ACTION as i32 && data.0 != 0 {
        // SAFETY: for HC_ACTION the system passes a valid KBDLLHOOKSTRUCT.
        let keyboard = unsafe { &*(data.0 as *const KBDLLHOOKSTRUCT) };
        let injected = (keyboard.flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED)).0 != 0;
        if keyboard.vkCode == u32::from(VK_ESCAPE.0) && !injected {
            match message.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    // Auto-repeat while held is latched and is not a second press.
                    if !ESCAPE_DOWN.swap(true, Ordering::AcqRel) {
                        post_escape_message(keyboard.time);
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => ESCAPE_DOWN.store(false, Ordering::Release),
                _ => {}
            }
            if SWALLOW_USER_ESCAPE.load(Ordering::Acquire) {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, message, data) }
}

/// PostMessage instead of handling the press here, because the hook runs on
/// the indicator thread itself. The time is the hook event time, so queueing
/// delay does not affect the double-press window.
fn post_escape_message(event_time_ms: u32) {
    let banner = ACTIVE_BANNER.load(Ordering::Acquire);
    if banner != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(hwnd_from(banner)),
                ESCAPE_MESSAGE,
                WPARAM(event_time_ms as usize),
                LPARAM(0),
            );
        }
    }
}

fn window_id(hwnd: HWND) -> isize {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_ID) }
}

unsafe extern "system" fn indicator_window_procedure(
    hwnd: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    // SAFETY: GWLP_USERDATA is either unset or the Overlay that created this
    // window, which outlives every window it creates.
    let overlay = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Overlay).as_ref() };
    let is_banner = || window_id(hwnd) == 0;
    match message {
        WM_NCCREATE => {
            // SAFETY: WM_NCCREATE carries the CREATESTRUCTW of this window.
            let create = unsafe { &*(l_param.0 as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            return LRESULT(1);
        }
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        ESCAPE_MESSAGE => {
            if let Some(overlay) = overlay {
                overlay.handle_escape(w_param.0 as u32);
            }
            return LRESULT(0);
        }
        WM_TIMER if w_param.0 == TIMER_ID && is_banner() => {
            if let Some(overlay) = overlay {
                overlay.repaint();
            }
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => {
            // The system broadcasts this message to each overlay window. Let
            // the banner perform the rebuild so it cannot destroy the edge
            // window whose callback is currently running.
            if let Some(overlay) = overlay
                && is_banner()
            {
                overlay.repaint();
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                // Only the banner owns the message loop. Cursor, edge, and
                // monitor rebuild windows can be destroyed independently.
                if is_banner() {
                    PostQuitMessage(0);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, w_param, l_param) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Side {
    const ALL: [Side; 4] = [Side::Top, Side::Bottom, Side::Left, Side::Right];
}

fn edge_rect(monitor: &RECT, side: Side) -> RECT {
    let mut result = *monitor;
    match side {
        Side::Top => result.bottom = result.bottom.min(result.top + EDGE_THICKNESS),
        Side::Bottom => result.top = result.top.max(result.bottom - EDGE_THICKNESS),
        Side::Left => result.right = result.right.min(result.left + EDGE_THICKNESS),
        Side::Right => result.left = result.left.max(result.right - EDGE_THICKNESS),
    }
    result
}

unsafe extern "system" fn collect_monitor_rect(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the Vec passed by monitor_rectangles for this call.
    let rectangles = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        rectangles.push(info.rcMonitor);
    }
    BOOL(1)
}

fn monitor_rectangles() -> Vec<RECT> {
    let mut rectangles: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor_rect),
            LPARAM(std::ptr::from_mut(&mut rectangles) as isize),
        );
    }
    rectangles
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | (green as u32) << 8 | (blue as u32) << 16)
}

/// A zeroed top-down 32-bit DIB section selected into its own memory DC.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(width: i32, height: i32) -> Option<Self> {
        let screen = unsafe { GetDC(None) };
        if screen.is_invalid() {
            return None;
        }
        let dc = unsafe { CreateCompatibleDC(Some(screen)) };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .unwrap_or_default();
        unsafe {
            ReleaseDC(None, screen);
        }
        let mut dib = Self {
            dc,
            bitmap,
            old_bitmap: HGDIOBJ::default(),
            bits: bits.cast(),
            width,
            height,
        };
        if dc.is_invalid() || bitmap.is_invalid() || bits.is_null() {
            return None;
        }
        dib.old_bitmap = unsafe { SelectObject(dc, bitmap.into()) };
        dib.pixels().fill(0);
        Some(dib)
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the DIB section owns width * height 32-bit pixels at `bits`
        // for as long as `bitmap` lives.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.bits,
                self.width as usize * self.height as usize * 4,
            )
        }
    }

    fn put_pixel(&mut self, x: i32, y: i32, color: COLORREF, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || alpha == 0 {
            return;
        }
        let [red, green, blue, _] = color.0.to_le_bytes();
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        let premultiply = |channel: u8| (u32::from(channel) * u32::from(alpha) / 255) as u8;
        // UpdateLayeredWindow requires premultiplied BGRA pixels.
        self.pixels()[offset..offset + 4].copy_from_slice(&[
            premultiply(blue),
            premultiply(green),
            premultiply(red),
            alpha,
        ]);
    }

    fn fill_rect(&mut self, rect: RECT, color: COLORREF, alpha: u8) {
        for y in rect.top.max(0)..rect.bottom.min(self.height) {
            for x in rect.left.max(0)..rect.right.min(self.width) {
                self.put_pixel(x, y, color, alpha);
            }
        }
    }

    /// GDI does not set alpha in a DIB when drawing text and lines. Only the
    /// small indicator surfaces are scanned, so this keeps the layered window
    /// correct without touching the whole virtual desktop.
    fn make_drawn_pixels_opaque(&mut self) {
        for pixel in self.pixels().as_chunks_mut::<4>().0 {
            if pixel[3] == 0 && (pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0) {
                pixel[3] = 255;
            }
        }
    }

    fn present(&self, hwnd: HWND, destination: POINT) {
        let source = POINT { x: 0, y: 0 };
        let size = SIZE {
            cx: self.width,
            cy: self.height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return;
            }
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&destination),
                Some(&size),
                Some(self.dc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen);
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.is_invalid() && !self.old_bitmap.is_invalid() {
                SelectObject(self.dc, self.old_bitmap);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

fn place_topmost(hwnd: HWND, destination: POINT, width: i32, height: i32) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            destination.x,
            destination.y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn paint_edge(hwnd: HWND, monitor: &RECT, side: Side) {
    let geometry = edge_rect(monitor, side);
    let width = geometry.right - geometry.left;
    let height = geometry.bottom - geometry.top;
    if width <= 0 || height <= 0 {
        return;
    }
    let destination = POINT {
        x: geometry.left,
        y: geometry.top,
    };
    place_topmost(hwnd, destination, width, height);

    let Some(mut dib) = Dib::new(width, height) else {
        return;
    };
    for distance in 0..EDGE_THICKNESS {
        let alpha = (220 * (EDGE_THICKNESS - distance) / EDGE_THICKNESS) as u8;
        let stripe = match side {
            Side::Top => RECT {
                left: 0,
                top: distance,
                right: width,
                bottom: distance + 1,
            },
            Side::Bottom => RECT {
                left: 0,
                top: height - distance - 1,
                right: width,
                bottom: height - distance,
            },
            Side::Left => RECT {
                left: distance,
                top: 0,
                right: distance + 1,
                bottom: height,
            },
            Side::Right => RECT {
                left: width - distance - 1,
                top: 0,
                right: width - distance,
                bottom: height,
            },
        };
        dib.fill_rect(stripe, rgb(42, 132, 255), alpha);
    }
    dib.present(hwnd, destination);
}

fn paint_banner(hwnd: HWND, label: &str) {
    let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let virtual_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if virtual_width <= 0 || virtual_height <= 0 {
        return;
    }
    let destination = POINT {
        x: virtual_left + 24,
        y: virtual_top + 20,
    };
    place_topmost(hwnd, destination, BANNER_WIDTH, BANNER_HEIGHT);

    let Some(mut dib) = Dib::new(BANNER_WIDTH, BANNER_HEIGHT) else {
        return;
    };
    dib.fill_rect(
        RECT {
            left: 0,
            top: 0,
            right: BANNER_WIDTH,
            bottom: BANNER_HEIGHT,
        },
        rgb(18, 24, 38),
        232,
    );
    dib.fill_rect(
        RECT {
            left: 0,
            top: 0,
            right: 4,
            bottom: BANNER_HEIGHT,
        },
        rgb(63, 164, 255),
        255,
    );

    let label: Vec<u16> = label.encode_utf16().collect();
    unsafe {
        SetBkMode(dib.dc, TRANSPARENT);
        SetTextColor(dib.dc, rgb(245, 248, 255));
        let font = CreateFontW(
            -18,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Segoe UI"),
        );
        if !font.is_invalid() {
            let old_font = SelectObject(dib.dc, font.into());
            let _ = TextOutW(dib.dc, 18, 24, &label);
            SelectObject(dib.dc, old_font);
            let _ = DeleteObject(font.into());
        }
    }
    dib.make_drawn_pixels_opaque();
    dib.present(hwnd, destination);
}

fn paint_cursor(hwnd: HWND) {
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err() {
        return;
    }
    let destination = POINT {
        x: cursor.x - CURSOR_SIZE / 2,
        y: cursor.y - CURSOR_SIZE / 2,
    };
    place_topmost(hwnd, destination, CURSOR_SIZE, CURSOR_SIZE);

    let Some(mut dib) = Dib::new(CURSOR_SIZE, CURSOR_SIZE) else {
        return;
    };
    let center = CURSOR_SIZE / 2;
    unsafe {
        let ring_pen = CreatePen(PS_SOLID, 3, rgb(54, 205, 255));
        if !ring_pen.is_invalid() {
            let old_pen = SelectObject(dib.dc, ring_pen.into());
            let old_brush = SelectObject(dib.dc, GetStockObject(NULL_BRUSH));
            let _ = Ellipse(dib.dc, center - 14, center - 14, center + 15, center + 15);
            SelectObject(dib.dc, old_brush);
            SelectObject(dib.dc, old_pen);
            let _ = DeleteObject(ring_pen.into());
        }
        let arrow = [
            POINT {
                x: center - 1,
                y: center - 4,
            },
            POINT {
                x: center - 1,
                y: center + 14,
            },
            POINT {
                x: center + 4,
                y: center + 9,
            },
            POINT {
                x: center + 10,
                y: center + 16,
            },
            POINT {
                x: center + 14,
                y: center + 12,
            },
            POINT {
                x: center + 8,
                y: center + 5,
            },
            POINT {
                x: center + 15,
                y: center + 5,
            },
        ];
        let arrow_brush = CreateSolidBrush(rgb(255, 255, 255));
        if !arrow_brush.is_invalid() {
            let old_brush = SelectObject(dib.dc, arrow_brush.into());
            let _ = Polygon(dib.dc, &arrow);
            SelectObject(dib.dc, old_brush);
            let _ = DeleteObject(arrow_brush.into());
        }
    }
    dib.make_drawn_pixels_opaque();
    dib.present(hwnd, destination);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn send_escape(message: u32, flags: u32, time: u32) {
        let escape = KBDLLHOOKSTRUCT {
            vkCode: u32::from(VK_ESCAPE.0),
            flags: windows::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT_FLAGS(flags),
            time,
            ..Default::default()
        };
        unsafe {
            low_level_keyboard_proc(
                HC_ACTION as i32,
                WPARAM(message as usize),
                LPARAM(std::ptr::from_ref(&escape) as isize),
            );
        }
    }

    fn press_escape(time: u32) {
        send_escape(WM_KEYDOWN, 0, time);
        send_escape(WM_KEYUP, 0, time + 30);
    }

    fn settle() {
        thread::sleep(Duration::from_millis(50));
    }

    /// Calls the private hook directly instead of synthesizing hardware input,
    /// so it verifies routing only, not physical keyboard delivery.
    #[test]
    #[ignore = "creates topmost overlays and a global keyboard hook on the interactive desktop"]
    fn only_a_second_physical_escape_within_the_window_stops_the_session_once() {
        let indicator = SessionIndicator::new();
        let stop_callbacks = Arc::new(AtomicUsize::new(0));
        let counter = stop_callbacks.clone();
        indicator
            .start(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))
            .expect("indicator starts");

        send_escape(WM_KEYDOWN, LLKHF_INJECTED.0, 1000);
        send_escape(WM_KEYUP, LLKHF_INJECTED.0, 1010);
        send_escape(WM_KEYDOWN, LLKHF_INJECTED.0, 1020);
        settle();
        assert_eq!(
            stop_callbacks.load(Ordering::SeqCst),
            0,
            "injected Escape presses never count"
        );
        assert!(indicator.running());

        press_escape(2000);
        settle();
        assert_eq!(
            stop_callbacks.load(Ordering::SeqCst),
            0,
            "a single press only arms the stop"
        );
        assert!(indicator.running());
        assert!(
            indicator.banner_text().contains("again"),
            "the armed stop shows the hint"
        );

        send_escape(WM_KEYDOWN, 0, 4000);
        send_escape(WM_KEYDOWN, 0, 4100);
        send_escape(WM_KEYUP, 0, 4200);
        settle();
        assert_eq!(
            stop_callbacks.load(Ordering::SeqCst),
            0,
            "auto-repeat is not a second press"
        );
        assert!(indicator.running());

        press_escape(5500);
        settle();
        assert_eq!(
            stop_callbacks.load(Ordering::SeqCst),
            0,
            "a press after the window re-arms"
        );
        assert!(indicator.running());

        press_escape(6200);
        for _ in 0..100 {
            if !indicator.running() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        indicator.stop();
        assert_eq!(
            stop_callbacks.load(Ordering::SeqCst),
            1,
            "a second press within the window stops once"
        );
        assert!(!indicator.running());
    }
}

use std::cell::RefCell;
use std::fs::File;
use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetStockObject, HBRUSH, HDC,
    InvalidateRect, LineTo, MoveToEx, PAINTSTRUCT, SetBkMode, TRANSPARENT, TextOutW, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::MK_LBUTTON;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    IDC_ARROW, LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, ShowWindow,
    TranslateMessage, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const MINIMUM_DRAG_MOVES: u32 = 2;
const MINIMUM_DRAG_DISTANCE: f64 = 40.0;

struct DragLog {
    file: File,
    origin: Instant,
    held: bool,
    start: (i32, i32),
    pointer: (i32, i32),
    moves: u32,
    completed: u32,
}

impl DragLog {
    fn record(&mut self, event: &str, x: i32, y: i32, left_button: bool) {
        let micros = self.origin.elapsed().as_micros();
        let _ = writeln!(
            self.file,
            r#"{{"event":"{event}","us":{micros},"x":{x},"y":{y},"left":{left_button}}}"#
        );
        let _ = self.file.flush();
        self.pointer = (x, y);
        match event {
            "down" => {
                self.held = true;
                self.start = (x, y);
                self.moves = 0;
            }
            "move" if self.held && left_button => self.moves += 1,
            "up" => {
                let distance = f64::from(x - self.start.0).hypot(f64::from(y - self.start.1));
                if self.held && self.moves >= MINIMUM_DRAG_MOVES && distance > MINIMUM_DRAG_DISTANCE
                {
                    self.completed += 1;
                    let (moves, count) = (self.moves, self.completed);
                    let _ = writeln!(
                        self.file,
                        r#"{{"event":"drag-complete","us":{micros},"moves":{moves},"count":{count}}}"#
                    );
                    let _ = self.file.flush();
                }
                self.held = false;
            }
            _ => {}
        }
    }
}

thread_local! {
    static LOG: RefCell<Option<DragLog>> = const { RefCell::new(None) };
}

fn with_log(action: impl FnOnce(&mut DragLog)) {
    LOG.with_borrow_mut(|log| {
        if let Some(log) = log {
            action(log);
        }
    });
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pointer_position(lparam: LPARAM) -> (i32, i32) {
    let x = i32::from((lparam & 0xffff) as u16 as i16);
    let y = i32::from(((lparam >> 16) & 0xffff) as u16 as i16);
    (x, y)
}

unsafe fn draw_text(device: HDC, x: i32, y: i32, text: &str) {
    let text: Vec<u16> = text.encode_utf16().collect();
    unsafe { TextOutW(device, x, y, text.as_ptr(), text.len() as i32) };
}

unsafe fn paint(window: HWND) {
    let mut paint = unsafe { std::mem::zeroed::<PAINTSTRUCT>() };
    let device = unsafe { BeginPaint(window, &mut paint) };
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        GetClientRect(window, &mut client);
        FillRect(device, &client, GetStockObject(WHITE_BRUSH) as HBRUSH);
        SetBkMode(device, TRANSPARENT as i32);
        draw_text(
            device,
            20,
            20,
            "DCU native drag fixture | drag blue square to green square | Esc closes",
        );
        let blue = CreateSolidBrush(0x00e6_6e2d);
        let green = CreateSolidBrush(0x0069_b423);
        FillRect(
            device,
            &RECT {
                left: 80,
                top: 120,
                right: 200,
                bottom: 240,
            },
            blue,
        );
        FillRect(
            device,
            &RECT {
                left: 480,
                top: 120,
                right: 600,
                bottom: 240,
            },
            green,
        );
        DeleteObject(blue);
        DeleteObject(green);
    }
    with_log(|log| unsafe {
        let status = format!(
            "Completed drags: {} | held: {} | moves: {}",
            log.completed,
            u8::from(log.held),
            log.moves
        );
        draw_text(device, 20, 300, &status);
        if log.held {
            MoveToEx(device, log.start.0, log.start.1, std::ptr::null_mut());
            LineTo(device, log.pointer.0, log.pointer.1);
        }
    });
    unsafe { EndPaint(window, &paint) };
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let (x, y) = pointer_position(lparam);
    match message {
        WM_LBUTTONDOWN => unsafe {
            SetCapture(window);
            with_log(|log| log.record("down", x, y, true));
            InvalidateRect(window, std::ptr::null(), 0);
        },
        WM_MOUSEMOVE => unsafe {
            with_log(|log| log.record("move", x, y, wparam & MK_LBUTTON as usize != 0));
            InvalidateRect(window, std::ptr::null(), 0);
        },
        WM_LBUTTONUP => unsafe {
            with_log(|log| log.record("up", x, y, false));
            ReleaseCapture();
            InvalidateRect(window, std::ptr::null(), 0);
        },
        WM_KEYDOWN => {
            if wparam == usize::from(VK_ESCAPE) {
                unsafe { DestroyWindow(window) };
            }
        }
        WM_PAINT => unsafe { paint(window) },
        WM_DESTROY => {
            with_log(|log| {
                let (pointer_x, pointer_y) = log.pointer;
                let held = log.held;
                log.record("closed", pointer_x, pointer_y, held);
            });
            unsafe { PostQuitMessage(0) };
        }
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    0
}

pub fn run() -> ExitCode {
    let log_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "drag-events.ndjson".into());
    let Ok(file) = File::create(&log_path) else {
        eprintln!("Cannot open event log");
        return ExitCode::from(1);
    };
    LOG.set(Some(DragLog {
        file,
        origin: Instant::now(),
        held: false,
        start: (0, 0),
        pointer: (0, 0),
        moves: 0,
        completed: 0,
    }));

    let class_name = wide("DCUDragFixture");
    let title = wide("DCU native drag fixture");
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(std::ptr::null());
        let mut class = std::mem::zeroed::<WNDCLASSW>();
        class.lpfnWndProc = Some(window_procedure);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        class.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        if RegisterClassW(&class) == 0 {
            return ExitCode::from(2);
        }
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            760,
            480,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            return ExitCode::from(3);
        }
        // Start-Process -WindowStyle Hidden suppresses the console; its startup
        // flag overrides the first ShowWindow call, so show the fixture twice.
        ShowWindow(window, SW_SHOW);
        ShowWindow(window, SW_SHOW);
    }
    with_log(|log| log.record("ready", 0, 0, false));

    let mut message = unsafe { std::mem::zeroed::<MSG>() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    ExitCode::SUCCESS
}

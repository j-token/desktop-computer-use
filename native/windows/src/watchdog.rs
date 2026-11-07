//! The DCU process publishes the input it holds into a named file mapping; a
//! child watchdog process releases exactly that input once the parent exits,
//! so a crashed or killed daemon never leaves a button or key held down.

use std::sync::atomic::{AtomicI32, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, FILE_MAP_WRITE,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, GetCurrentProcessId, OpenProcess, PROCESS_INFORMATION,
    PROCESS_SYNCHRONIZE, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// Version 2 adds the space bit (16) to modifiers for toggled keys.
const STATE_VERSION: u32 = 2;
const PARENT_POLL_INTERVAL_MS: u32 = 250;

/// Shared with the watchdog process through the file mapping (24 bytes);
/// every field is 4-byte aligned, so `repr(C)` adds no padding.
#[repr(C)]
struct SharedInputState {
    version: u32,
    alive: AtomicI32,
    buttons: AtomicI32,
    modifiers: AtomicI32,
    key: AtomicI32,
    key_flags: AtomicI32,
}

#[derive(Clone, Copy, Default)]
struct HeldInput {
    buttons: u32,
    modifiers: u32,
    key: u16,
    key_flags: u32,
}

impl SharedInputState {
    fn read(&self) -> HeldInput {
        HeldInput {
            buttons: self.buttons.load(Ordering::SeqCst) as u32,
            modifiers: self.modifiers.load(Ordering::SeqCst) as u32,
            key: self.key.load(Ordering::SeqCst) as u16,
            key_flags: self.key_flags.load(Ordering::SeqCst) as u32,
        }
    }

    fn write(&self, input: HeldInput) {
        self.buttons.swap(input.buttons as i32, Ordering::SeqCst);
        self.modifiers
            .swap(input.modifiers as i32, Ordering::SeqCst);
        self.key.swap(i32::from(input.key), Ordering::SeqCst);
        self.key_flags
            .swap(input.key_flags as i32, Ordering::SeqCst);
    }
}

/// A mapped view of the shared state; unmaps the view and closes the mapping on drop.
struct MappedState {
    mapping: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the view is only accessed through the atomics of SharedInputState,
// and kernel handles may be used and closed from any thread.
unsafe impl Send for MappedState {}
// SAFETY: as above.
unsafe impl Sync for MappedState {}

impl MappedState {
    fn create(name: &HSTRING) -> Option<Self> {
        let size = std::mem::size_of::<SharedInputState>() as u32;
        let mapping = unsafe {
            CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, size, name)
        }
        .ok()?;
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, size as usize) };
        let mapped = Self { mapping, view };
        if view.Value.is_null() {
            return None;
        }
        Some(mapped)
    }

    fn open(name: &HSTRING) -> Result<Self, i32> {
        let access = FILE_MAP_READ | FILE_MAP_WRITE;
        let mapping = unsafe { OpenFileMappingW(access.0, false, name) }.map_err(|_| 3)?;
        let view = unsafe {
            MapViewOfFile(
                mapping,
                access,
                0,
                0,
                std::mem::size_of::<SharedInputState>(),
            )
        };
        let mapped = Self { mapping, view };
        if view.Value.is_null() || mapped.state().version != STATE_VERSION {
            return Err(4);
        }
        Ok(mapped)
    }

    fn state(&self) -> &SharedInputState {
        // SAFETY: the view is a live mapping of at least size_of::<SharedInputState>()
        // bytes, page aligned, and every field after `version` is only accessed atomically.
        unsafe { &*self.view.Value.cast::<SharedInputState>() }
    }
}

impl Drop for MappedState {
    fn drop(&mut self) {
        unsafe {
            if !self.view.Value.is_null() {
                let _ = UnmapViewOfFile(self.view);
            }
            let _ = CloseHandle(self.mapping);
        }
    }
}

struct OwnedHandle(HANDLE);

// Kernel process handles can be waited on and closed from any thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn send_one(input: INPUT) {
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

pub(crate) fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

pub(crate) fn keyboard_input(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn release_tracked(input: HeldInput) {
    for (bit, flag) in [
        (1, MOUSEEVENTF_LEFTUP),
        (2, MOUSEEVENTF_RIGHTUP),
        (4, MOUSEEVENTF_MIDDLEUP),
    ] {
        if input.buttons & bit != 0 {
            send_one(mouse_input(flag));
        }
    }
    for (bit, key) in [
        (1, VK_SHIFT),
        (2, VK_CONTROL),
        (4, VK_MENU),
        (8, VK_LWIN),
        (16, VK_SPACE),
    ] {
        if input.modifiers & bit == 0 {
            continue;
        }
        let extended = if key == VK_LWIN {
            KEYEVENTF_EXTENDEDKEY
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        send_one(keyboard_input(key, 0, KEYEVENTF_KEYUP | extended));
    }
    if input.key != 0 {
        let flags = KEYBD_EVENT_FLAGS(input.key_flags);
        let release = if flags.contains(KEYEVENTF_UNICODE) {
            keyboard_input(VIRTUAL_KEY(0), input.key, flags | KEYEVENTF_KEYUP)
        } else {
            keyboard_input(VIRTUAL_KEY(input.key), 0, flags | KEYEVENTF_KEYUP)
        };
        send_one(release);
    }
}

/// The `--input-watchdog` child mode. Exit codes: 0 done, 2 bad mapping name,
/// 3 mapping missing, 4 mapping unreadable or of another version.
pub fn run_input_watchdog(parent_pid: u32, mapping_name: &str) -> i32 {
    if mapping_name.is_empty() {
        return 2;
    }
    let mapped = match MappedState::open(&HSTRING::from(mapping_name)) {
        Ok(mapped) => mapped,
        Err(code) => return code,
    };
    let state = mapped.state();
    let Ok(parent) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }) else {
        release_tracked(state.read());
        state.write(HeldInput::default());
        return 0;
    };
    let parent = OwnedHandle(parent);
    loop {
        let wait = unsafe { WaitForSingleObject(parent.0, PARENT_POLL_INTERVAL_MS) };
        let parent_exited = wait == WAIT_OBJECT_0;
        if parent_exited || state.alive.load(Ordering::SeqCst) == 0 {
            let held = state.read();
            if parent_exited {
                release_tracked(held);
            }
            state.write(HeldInput::default());
            break;
        }
        if wait == WAIT_FAILED {
            break;
        }
    }
    0
}

/// Owns the shared state mapping and the watchdog child of this process.
pub(crate) struct InputWatchdog {
    mapped: Option<MappedState>,
    child: Option<OwnedHandle>,
}

impl InputWatchdog {
    pub(crate) fn start() -> Self {
        let process_id = unsafe { GetCurrentProcessId() };
        let mapping_name = format!("Local\\DCU.Input.{process_id}.{}", unsafe {
            GetTickCount64()
        });
        let Some(mapped) = MappedState::create(&HSTRING::from(mapping_name.as_str())) else {
            return Self {
                mapped: None,
                child: None,
            };
        };
        // SAFETY: the view was just created and is not shared with anyone yet.
        unsafe {
            std::ptr::write_bytes(
                mapped.view.Value.cast::<u8>(),
                0,
                std::mem::size_of::<SharedInputState>(),
            );
            let state = mapped.view.Value.cast::<SharedInputState>();
            std::ptr::addr_of_mut!((*state).version).write(STATE_VERSION);
        }
        mapped.state().alive.store(1, Ordering::SeqCst);
        let child = spawn_watchdog(process_id, &mapping_name);
        Self {
            mapped: Some(mapped),
            child,
        }
    }

    pub(crate) fn publish(&self, buttons: u32, modifiers: u32, key: u16, key_flags: u32) {
        if let Some(mapped) = &self.mapped {
            mapped.state().write(HeldInput {
                buttons,
                modifiers,
                key,
                key_flags,
            });
        }
    }
}

impl Drop for InputWatchdog {
    fn drop(&mut self) {
        if let Some(mapped) = &self.mapped {
            let state = mapped.state();
            state.write(HeldInput::default());
            state.alive.swap(0, Ordering::SeqCst);
        }
        self.mapped = None;
        if let Some(child) = self.child.take() {
            unsafe {
                let _ = TerminateProcess(child.0, 0);
            }
        }
    }
}

fn spawn_watchdog(process_id: u32, mapping_name: &str) -> Option<OwnedHandle> {
    let mut executable = [0u16; 260 * 4];
    let length = unsafe { GetModuleFileNameW(None, &mut executable) } as usize;
    if length == 0 || length >= executable.len() {
        return None;
    }
    let executable_path = &executable[..length];
    let arguments = format!("\" --input-watchdog {process_id} \"{mapping_name}\"");
    let mut command_line: Vec<u16> = std::iter::once(u16::from(b'"'))
        .chain(executable_path.iter().copied())
        .chain(arguments.encode_utf16())
        .chain(std::iter::once(0))
        .collect();
    let mut executable_z: Vec<u16> = executable_path
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(executable_z.as_mut_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut process,
        )
    }
    .ok()?;
    unsafe {
        let _ = CloseHandle(process.hThread);
    }
    Some(OwnedHandle(process.hProcess))
}

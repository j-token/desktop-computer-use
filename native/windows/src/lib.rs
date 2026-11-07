#![cfg(windows)]

mod backend;
mod capture;
mod indicator;
mod uia;
mod watchdog;

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use dcu_core::{Backend, Error};
use windows::Win32::Foundation::GetLastError;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

pub use watchdog::run_input_watchdog;

/// A COM object of the process-wide multithreaded apartment. Every thread
/// that touches one has joined the MTA (the daemon's main thread through
/// [`RuntimeApartment`], each request through the backend's per-call COM
/// scope), and MTA interface pointers may be used from any of its threads.
/// Objects that are not themselves thread-safe, such as the D3D11 immediate
/// context, are additionally kept behind a mutex.
pub(crate) struct Mta<T>(pub(crate) T);

// SAFETY: see the type documentation.
unsafe impl<T> Send for Mta<T> {}
// SAFETY: see the type documentation.
unsafe impl<T> Sync for Mta<T> {}

impl<T> Deref for Mta<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Mta<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

pub fn make_backend() -> Result<Arc<dyn Backend>, Error> {
    Ok(backend::make_backend())
}

/// DWM frame bounds and captured frames are always physical pixels, while UIA
/// bounds and SetCursorPos follow the caller's DPI awareness. Without
/// per-monitor awareness a scaled display mixes the two spaces and every
/// click lands off target.
pub fn enable_per_monitor_dpi_awareness() {
    if unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_err()
    {
        eprintln!(
            "Per-monitor DPI awareness unavailable ({}); input may miss its target on scaled displays",
            unsafe { GetLastError() }.0
        );
    }
}

/// WGC/UIA objects outlive individual connection worker threads; this keeps
/// their shared MTA alive until the persistent backend is destroyed.
pub struct RuntimeApartment(());

impl RuntimeApartment {
    pub fn initialize() -> Result<Self, Error> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|_| {
            Error::new(
                "initialization_failed",
                "Cannot initialize persistent Windows Runtime apartment",
            )
        })?;
        Ok(Self(()))
    }
}

impl Drop for RuntimeApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

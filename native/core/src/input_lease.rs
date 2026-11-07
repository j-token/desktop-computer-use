//! Independent of the transport endpoint: alternative runtime directories must
//! not allow two daemons to inject input concurrently. Kernel lifetime handles
//! release the lease after a process crash, without a stale PID-file takeover.
//! The lease names match earlier C++ releases, so an old and a new daemon
//! still exclude each other.

use crate::backend::Error;

#[derive(Default)]
pub(crate) struct InputLease {
    #[cfg(windows)]
    desktop_mutex: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(unix)]
    lock_file: Option<std::fs::File>,
}

impl InputLease {
    pub(crate) fn release(&mut self) {
        #[cfg(windows)]
        {
            self.desktop_mutex = None;
        }
        #[cfg(unix)]
        {
            self.lock_file = None;
        }
    }

    #[cfg(windows)]
    pub(crate) fn acquire(&mut self) -> Result<(), Error> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        if self.desktop_mutex.is_some() {
            return Err(Error::new(
                "session_busy",
                "This daemon already owns desktop input",
            ));
        }
        let owner_failure = || Error::new("session_lock_failed", "Cannot identify desktop owner");
        let user_sid = crate::server::current_user_sid().map_err(|_| owner_failure())?;
        let name = wide(&format!("Local\\DesktopComputerUse.Input.{user_sid}"));
        // The object's existence is the process lease. No thread owns this
        // mutex, so releasing the lease closes the handle instead of ReleaseMutex.
        let (mutex, creation_error) = unsafe {
            let mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            (mutex, GetLastError())
        };
        if mutex.is_null() {
            return Err(Error::new(
                "session_lock_failed",
                "Cannot create desktop input lease",
            ));
        }
        let mutex = unsafe { OwnedHandle::from_raw_handle(mutex) };
        if creation_error == ERROR_ALREADY_EXISTS {
            return Err(Error::new(
                "session_busy",
                "Another daemon owns desktop input",
            ));
        }
        self.desktop_mutex = Some(mutex);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn acquire(&mut self) -> Result<(), Error> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        if self.lock_file.is_some() {
            return Err(Error::new(
                "session_busy",
                "This daemon already owns desktop input",
            ));
        }
        let private_lease_failure = || {
            Error::new(
                "session_lock_failed",
                "Cannot create private desktop input lease",
            )
        };
        let user_id = unsafe { libc::getuid() };
        let lock_path = format!("/tmp/desktop-computer-use-input-{user_id}.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|_| private_lease_failure())?;
        let owned_regular_file = file
            .metadata()
            .is_ok_and(|metadata| metadata.uid() == user_id && metadata.file_type().is_file());
        if !owned_regular_file {
            return Err(private_lease_failure());
        }
        if file.try_lock().is_err() {
            return Err(Error::new(
                "session_busy",
                "Another daemon owns desktop input",
            ));
        }
        self.lock_file = Some(file);
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::backend::{Backend, Error, Json};
use crate::dispatch::Dispatcher;

const MAXIMUM_FRAME_BYTES: usize = 16 * 1024 * 1024;
const FRAME_DEADLINE: Duration = Duration::from_secs(15);
const STREAM_RETRY_INTERVAL: Duration = Duration::from_millis(2);
// A nonblocking pipe write of more than the 64 KiB output buffer writes
// nothing, so each write must stay comfortably inside it or the response
// never leaves.
const WRITE_CHUNK_BYTES: usize = 16 * 1024;
const DISCONNECT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAXIMUM_CLIENTS: usize = 24;

static TERMINATE_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_termination(_signal: libc::c_int) {
    TERMINATE_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_termination_handlers() {
    let handler = request_termination as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

fn termination_requested() -> bool {
    TERMINATE_REQUESTED.load(Ordering::SeqCst)
}

enum ReadOutcome {
    Data(usize),
    Pending,
    Closed,
}

enum WriteOutcome {
    Wrote(usize),
    Pending,
    Failed,
}

/// Serves authenticated NDJSON requests on `endpoint` until a client asks the
/// daemon to shut down or the process receives SIGINT/SIGTERM.
pub fn serve(endpoint: &str, token: &str, backend: Arc<dyn Backend>) -> Result<(), Error> {
    let dispatcher = Arc::new(Dispatcher::new(backend, token.to_owned()));
    let token: Arc<str> = Arc::from(token);
    install_termination_handlers();
    let active_clients = Arc::new(AtomicUsize::new(0));

    let listened = platform::listen(endpoint, &dispatcher, |connection| {
        if active_clients.load(Ordering::SeqCst) >= MAXIMUM_CLIENTS {
            return;
        }
        active_clients.fetch_add(1, Ordering::SeqCst);
        let (dispatcher, token, active_clients) =
            (dispatcher.clone(), token.clone(), active_clients.clone());
        thread::spawn(move || {
            service_connection(connection, &dispatcher, &token);
            active_clients.fetch_sub(1, Ordering::SeqCst);
        });
    });

    dispatcher.shutdown();
    while active_clients.load(Ordering::SeqCst) > 0 {
        thread::sleep(Duration::from_millis(20));
    }
    listened
}

fn service_connection(connection: platform::Connection, dispatcher: &Dispatcher, token: &str) {
    if let Err(error) = respond(&connection, dispatcher, token) {
        eprintln!("transport: {error}");
    }
}

fn respond(
    connection: &platform::Connection,
    dispatcher: &Dispatcher,
    token: &str,
) -> Result<(), Error> {
    let request = read_request(connection, dispatcher)?;
    // A lost input owner cancels its operation instead of allowing late input.
    let cancel_on_disconnect = should_cancel_on_disconnect(&request, token);
    let response = thread::scope(|scope| {
        let (finished, finished_signal) = mpsc::channel::<()>();
        scope.spawn(move || {
            while let Err(RecvTimeoutError::Timeout) =
                finished_signal.recv_timeout(DISCONNECT_POLL_INTERVAL)
            {
                if cancel_on_disconnect && connection.is_disconnected() {
                    dispatcher.shutdown();
                    break;
                }
            }
        });
        let response = dispatcher.handle(&request);
        drop(finished);
        response
    });

    let mut frame = response.to_string();
    if frame.len() + 1 > MAXIMUM_FRAME_BYTES {
        frame = json!({
            "id": request.get("id").cloned().unwrap_or(Json::Null),
            "ok": false,
            "error": {"code": "response_too_large", "message": "Response exceeds 16 MiB"},
        })
        .to_string();
    }
    frame.push('\n');
    write_response(connection, frame.as_bytes());
    Ok(())
}

fn read_request(connection: &platform::Connection, dispatcher: &Dispatcher) -> Result<Json, Error> {
    let mut frame = Vec::new();
    let mut buffer = [0u8; 8192];
    let deadline = Instant::now() + FRAME_DEADLINE;
    let line_end = loop {
        if dispatcher.is_quitting() || Instant::now() > deadline {
            return Err(Error::new("transport_timeout", "Request frame timed out"));
        }
        let bytes_read = match connection.read(&mut buffer) {
            ReadOutcome::Data(bytes_read) => bytes_read,
            ReadOutcome::Pending => {
                thread::sleep(STREAM_RETRY_INTERVAL);
                continue;
            }
            ReadOutcome::Closed => {
                return Err(Error::new("transport_closed", "Client disconnected"));
            }
        };
        let searched = frame.len();
        frame.extend_from_slice(&buffer[..bytes_read]);
        if frame.len() > MAXIMUM_FRAME_BYTES {
            return Err(Error::invalid_argument("Request exceeds 16 MiB"));
        }
        if let Some(offset) = frame[searched..].iter().position(|&byte| byte == b'\n') {
            break searched + offset;
        }
    };
    serde_json::from_slice(&frame[..line_end])
        .map_err(|error| Error::new("invalid_json", error.to_string()))
}

fn should_cancel_on_disconnect(request: &Json, token: &str) -> bool {
    if request.get("token").and_then(Json::as_str) != Some(token) {
        return false;
    }
    let Some(method) = request.get("method").and_then(Json::as_str) else {
        return false;
    };
    !matches!(method, "doctor" | "capabilities" | "session.status")
}

fn write_response(connection: &platform::Connection, response: &[u8]) {
    let deadline = Instant::now() + FRAME_DEADLINE;
    let mut bytes_sent = 0;
    while bytes_sent < response.len() && Instant::now() < deadline {
        let chunk_end = response.len().min(bytes_sent + WRITE_CHUNK_BYTES);
        match connection.write(&response[bytes_sent..chunk_end]) {
            WriteOutcome::Wrote(bytes_written) => bytes_sent += bytes_written,
            WriteOutcome::Pending => thread::sleep(STREAM_RETRY_INTERVAL),
            WriteOutcome::Failed => {
                eprintln!(
                    "transport: response truncated at {bytes_sent} of {} bytes",
                    response.len()
                );
                break;
            }
        }
    }
    if bytes_sent == response.len() {
        connection.wait_for_client_close(deadline);
    }
}

#[cfg(windows)]
pub(crate) fn current_user_sid() -> Result<String, Error> {
    platform::current_user_sid()
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PeekNamedPipe,
    };
    use windows_sys::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex,
    };

    use super::{ReadOutcome, WriteOutcome, termination_requested};
    use crate::backend::Error;
    use crate::dispatch::Dispatcher;
    use crate::input_lease::wide;

    const PIPE_ACCEPT_INTERVAL: Duration = Duration::from_millis(5);
    const PIPE_BUFFER_BYTES: u32 = 65536;
    const PIPE_INSTANCES: u32 = 32;

    pub(super) struct Connection(HANDLE);

    // A pipe handle may be used from any thread; the watcher only peeks while
    // the worker reads and writes.
    unsafe impl Send for Connection {}
    unsafe impl Sync for Connection {}

    impl Connection {
        pub(super) fn read(&self, buffer: &mut [u8]) -> ReadOutcome {
            let mut available = 0u32;
            if unsafe {
                PeekNamedPipe(
                    self.0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return ReadOutcome::Closed;
            }
            if available == 0 {
                return ReadOutcome::Pending;
            }
            let requested = available.min(buffer.len() as u32);
            let mut read = 0u32;
            let succeeded = unsafe {
                ReadFile(
                    self.0,
                    buffer.as_mut_ptr(),
                    requested,
                    &mut read,
                    std::ptr::null_mut(),
                )
            } != 0;
            if !succeeded || read == 0 {
                ReadOutcome::Closed
            } else {
                ReadOutcome::Data(read as usize)
            }
        }

        pub(super) fn write(&self, bytes: &[u8]) -> WriteOutcome {
            let mut written = 0u32;
            let succeeded = unsafe {
                WriteFile(
                    self.0,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            } != 0;
            match (succeeded, written) {
                (false, _) => WriteOutcome::Failed,
                (true, 0) => WriteOutcome::Pending,
                (true, written) => WriteOutcome::Wrote(written as usize),
            }
        }

        pub(super) fn is_disconnected(&self) -> bool {
            let mut available = 0u32;
            unsafe {
                PeekNamedPipe(
                    self.0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                ) == 0
            }
        }

        /// DisconnectNamedPipe discards unread output. Clients close after
        /// parsing the complete response; wait for that before teardown.
        pub(super) fn wait_for_client_close(&self, deadline: Instant) {
            while !self.is_disconnected() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            unsafe {
                DisconnectNamedPipe(self.0);
                CloseHandle(self.0);
            }
        }
    }

    struct PrivateSecurity {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl PrivateSecurity {
        fn new() -> Result<Self, Error> {
            let sid = current_user_sid()?;
            let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let converted = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if converted == 0 {
                return Err(Error::new(
                    "invalid_environment",
                    "Cannot restrict pipe access",
                ));
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            Ok(Self {
                descriptor,
                attributes,
            })
        }
    }

    impl Drop for PrivateSecurity {
        fn drop(&mut self) {
            unsafe { LocalFree(self.descriptor) };
        }
    }

    pub(super) fn current_user_sid() -> Result<String, Error> {
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(Error::new(
                "invalid_environment",
                "Cannot inspect Windows user token",
            ));
        }
        let mut size = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size) };
        // u64 storage keeps TOKEN_USER's pointer field aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } != 0;
        unsafe { CloseHandle(token) };
        if size == 0 || !queried {
            return Err(Error::new(
                "invalid_environment",
                "Cannot inspect Windows user SID",
            ));
        }
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        let mut sid_text: *mut u16 = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) } == 0 {
            return Err(Error::new("invalid_environment", "Cannot format user SID"));
        }
        let sid = unsafe {
            let length = (0..).take_while(|&index| *sid_text.add(index) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, length))
        };
        unsafe { LocalFree(sid_text as *mut c_void) };
        Ok(sid)
    }

    /// `std::hash<std::string>` of MSVC's standard library (64-bit FNV-1a), so
    /// the endpoint singleton also excludes a daemon from an earlier C++
    /// release on the same endpoint.
    fn msvc_string_hash(value: &str) -> u64 {
        value
            .bytes()
            .fold(14_695_981_039_346_656_037u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
            })
    }

    struct Singleton(HANDLE);

    impl Drop for Singleton {
        fn drop(&mut self) {
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }

    pub(super) fn listen(
        endpoint: &str,
        dispatcher: &Dispatcher,
        mut accept: impl FnMut(Connection),
    ) -> Result<(), Error> {
        if !endpoint.starts_with(r"\\.\pipe\") {
            return Err(Error::invalid_argument(
                "Expected local named pipe endpoint",
            ));
        }
        let name = wide(endpoint);
        let security = PrivateSecurity::new()?;
        let mutex_name = wide(&format!("Local\\dcu-{}", msvc_string_hash(endpoint)));
        let (singleton, creation_error) = unsafe {
            let singleton = CreateMutexW(&security.attributes, 1, mutex_name.as_ptr());
            (singleton, GetLastError())
        };
        if singleton.is_null() || creation_error == ERROR_ALREADY_EXISTS {
            if !singleton.is_null() {
                unsafe { CloseHandle(singleton) };
            }
            return Err(Error::new(
                "daemon_running",
                "An endpoint owner already exists",
            ));
        }
        let _singleton = Singleton(singleton);

        while !dispatcher.is_quitting() {
            if termination_requested() {
                dispatcher.request_quit();
                break;
            }
            dispatcher.tick();
            let pipe = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_INSTANCES,
                    PIPE_BUFFER_BYTES,
                    PIPE_BUFFER_BYTES,
                    0,
                    &security.attributes,
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                return Err(Error::new("transport_error", "Cannot create private pipe"));
            }
            let connection = Connection(pipe);
            let mut connected = false;
            while !dispatcher.is_quitting() {
                if termination_requested() {
                    dispatcher.request_quit();
                    break;
                }
                if unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0 {
                    connected = true;
                    break;
                }
                match unsafe { GetLastError() } {
                    ERROR_PIPE_CONNECTED => {
                        connected = true;
                        break;
                    }
                    ERROR_PIPE_LISTENING => {}
                    _ => break,
                }
                dispatcher.tick();
                thread::sleep(PIPE_ACCEPT_INTERVAL);
            }
            if connected {
                accept(connection);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::msvc_string_hash;

        #[test]
        fn msvc_string_hash_matches_fnv_1a_reference_values() {
            assert_eq!(msvc_string_hash(""), 14_695_981_039_346_656_037);
            assert_eq!(msvc_string_hash("a"), 0xaf63_dc4c_8601_ec8c);
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Instant;

    use super::{ReadOutcome, WriteOutcome, termination_requested};
    use crate::backend::Error;
    use crate::dispatch::Dispatcher;

    const LISTEN_POLL_MILLISECONDS: libc::c_int = 100;

    pub(super) struct Connection(UnixStream);

    fn would_block() -> bool {
        let error = std::io::Error::last_os_error().raw_os_error();
        matches!(error, Some(libc::EAGAIN | libc::EINTR)) || error == Some(libc::EWOULDBLOCK)
    }

    impl Connection {
        pub(super) fn read(&self, buffer: &mut [u8]) -> ReadOutcome {
            let received = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            match received {
                ..0 if would_block() => ReadOutcome::Pending,
                ..=0 => ReadOutcome::Closed,
                received => ReadOutcome::Data(received as usize),
            }
        }

        pub(super) fn write(&self, bytes: &[u8]) -> WriteOutcome {
            let flags = libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT;
            let sent = unsafe {
                libc::send(
                    self.0.as_raw_fd(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    flags,
                )
            };
            match sent {
                ..0 if would_block() => WriteOutcome::Pending,
                ..=0 => WriteOutcome::Failed,
                sent => WriteOutcome::Wrote(sent as usize),
            }
        }

        pub(super) fn is_disconnected(&self) -> bool {
            let mut byte = 0u8;
            let flags = libc::MSG_PEEK | libc::MSG_DONTWAIT;
            let peeked =
                unsafe { libc::recv(self.0.as_raw_fd(), (&mut byte as *mut u8).cast(), 1, flags) };
            peeked == 0 || (peeked < 0 && !would_block())
        }

        /// Closing a Unix socket never discards data the peer has not read yet.
        pub(super) fn wait_for_client_close(&self, _deadline: Instant) {}
    }

    fn peer_is_current_user(stream: &UnixStream) -> bool {
        let mut peer = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut size = size_of::<libc::ucred>() as libc::socklen_t;
        let queried = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut size,
            )
        } == 0;
        queried && peer.uid == unsafe { libc::getuid() }
    }

    fn require_private_directory(endpoint: &str) -> Result<(), Error> {
        let directory = Path::new(endpoint).parent().unwrap_or(Path::new(""));
        let private = fs::symlink_metadata(directory).is_ok_and(|metadata| {
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::getuid() }
                && metadata.mode() & 0o077 == 0
        });
        if private {
            Ok(())
        } else {
            Err(Error::new(
                "invalid_environment",
                "Endpoint directory must be owned by this user with mode 0700",
            ))
        }
    }

    pub(super) fn listen(
        endpoint: &str,
        dispatcher: &Dispatcher,
        mut accept: impl FnMut(Connection),
    ) -> Result<(), Error> {
        require_private_directory(endpoint)?;
        let maximum_path_bytes = size_of::<libc::sockaddr_un>() - size_of::<libc::sa_family_t>();
        if endpoint.len() >= maximum_path_bytes {
            return Err(Error::invalid_argument("Unix socket path is too long"));
        }
        let listener = UnixListener::bind(endpoint).map_err(|_| {
            Error::new(
                "daemon_running",
                "Cannot bind socket; endpoint may already be owned",
            )
        })?;
        let _ = fs::set_permissions(endpoint, fs::Permissions::from_mode(0o600));

        while !dispatcher.is_quitting() {
            if termination_requested() {
                dispatcher.request_quit();
                break;
            }
            dispatcher.tick();
            let mut ready = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut ready, 1, LISTEN_POLL_MILLISECONDS) } <= 0 {
                continue;
            }
            let Ok((client, _)) = listener.accept() else {
                continue;
            };
            if peer_is_current_user(&client) {
                accept(Connection(client));
            }
        }
        drop(listener);
        let _ = fs::remove_file(endpoint);
        Ok(())
    }
}

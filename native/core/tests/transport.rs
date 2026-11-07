use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dcu_core::{Backend, Context, Error, Json};
use serde_json::json;

const TOKEN: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

struct DoctorBackend;

impl Backend for DoctorBackend {
    fn execute(&self, method: &str, _params: &Json, _context: &Context) -> Result<Json, Error> {
        Ok(json!({"method": method}))
    }

    fn interrupt(&self) {}

    fn set_toggle(&self, _key: &str, _down: bool, _context: &Context) -> Result<(), Error> {
        Ok(())
    }

    fn release_toggles(&self) {}
}

#[cfg(windows)]
mod endpoint {
    use std::fs::{File, OpenOptions};

    pub type Stream = File;

    pub fn create() -> String {
        format!(r"\\.\pipe\dcu-transport-test-{}", std::process::id())
    }

    pub fn connect(endpoint: &str) -> std::io::Result<Stream> {
        OpenOptions::new().read(true).write(true).open(endpoint)
    }
}

#[cfg(unix)]
mod endpoint {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    pub type Stream = UnixStream;

    pub fn create() -> String {
        let directory =
            std::env::temp_dir().join(format!("dcu-transport-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        directory.join("daemon.sock").to_string_lossy().into_owned()
    }

    pub fn connect(endpoint: &str) -> std::io::Result<Stream> {
        UnixStream::connect(endpoint)
    }
}

fn connect(endpoint: &str) -> endpoint::Stream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match endpoint::connect(endpoint) {
            Ok(stream) => return stream,
            Err(error) if Instant::now() > deadline => {
                panic!("daemon endpoint never opened: {error}")
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn exchange(endpoint: &str, frame: &[u8]) -> String {
    let mut stream = connect(endpoint);
    let _ = stream.write_all(frame);
    let mut response = String::new();
    let _ = BufReader::new(stream).read_line(&mut response);
    response
}

fn request(method: &str) -> Vec<u8> {
    let mut frame = json!({"id": "transport", "token": TOKEN, "method": method})
        .to_string()
        .into_bytes();
    frame.push(b'\n');
    frame
}

#[test]
fn daemon_answers_valid_frames_and_closes_malformed_ones_without_a_response() {
    let endpoint = endpoint::create();
    let server_endpoint = endpoint.clone();
    let server =
        thread::spawn(move || dcu_core::serve(&server_endpoint, TOKEN, Arc::new(DoctorBackend)));

    let doctor: Json = serde_json::from_str(&exchange(&endpoint, &request("doctor"))).unwrap();
    let invalid_json = exchange(&endpoint, b"{not json\n");
    let mut oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
    oversized.push(b'\n');
    let oversized_response = exchange(&endpoint, &oversized);
    let unauthorized = exchange(
        &endpoint,
        b"{\"id\":\"bad\",\"token\":\"wrong\",\"method\":\"doctor\"}\n",
    );
    let shutdown: Json =
        serde_json::from_str(&exchange(&endpoint, &request("daemon.shutdown"))).unwrap();

    assert_eq!(
        doctor,
        json!({"id": "transport", "ok": true, "result": {"method": "doctor", "toggles": []}})
    );
    assert_eq!(invalid_json, "");
    assert_eq!(oversized_response, "");
    assert_eq!(
        unauthorized,
        "{\"error\":{\"code\":\"unauthorized\",\"message\":\"Invalid request credentials\"},\"id\":\"bad\",\"ok\":false}\n"
    );
    assert_eq!(shutdown["result"]["stopped"], true);
    assert_eq!(server.join().unwrap(), Ok(()));
}

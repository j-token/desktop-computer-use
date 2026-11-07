use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn server_socket() -> UnixStream {
    let path = std::env::var("DCU_EIS_TEST_SOCKET")
        .expect("DCU_EIS_TEST_SOCKET must name the test server socket");
    UnixStream::connect(path).expect("connect to the libeis test server")
}

#[test]
#[ignore = "requires the real libeis fixture; run tests/integration/eis/run.sh"]
fn mapped_input_releases_held_button_and_key() {
    let socket = server_socket();
    let context = Context::new();
    let mut client = EisClient::new();

    client.connect(socket.as_fd(), &context).unwrap();
    assert!(client.ready());
    client.absolute(1.5, 2.5, "demo", &context).unwrap();
    client.button(272, true, &context).unwrap();
    client.scroll(0.0, -3.0, &context).unwrap();
    client.key(30, true, &context).unwrap();
    client.release();
    client.close();

    assert!(!client.connected());
}

#[test]
#[ignore = "requires the real libeis fixture; run tests/integration/eis/run.sh"]
fn global_point_uses_second_region_without_mapping_ids() {
    let socket = server_socket();
    let context = Context::new();
    let mut client = EisClient::new();

    client.connect(socket.as_fd(), &context).unwrap();
    client.absolute_global(901.5, 202.5, &context).unwrap();
    client.close();

    assert!(!client.connected());
}

#[test]
#[ignore = "requires libei at runtime; run tests/integration/eis/run.sh"]
fn stalled_handshake_observes_cancellation() {
    let (socket, _quiet_server) = UnixStream::pair().unwrap();
    let context = Context::new();
    let worker_context = context.clone();
    let worker = thread::spawn(move || {
        let mut client = EisClient::new();
        client.connect(socket.as_fd(), &worker_context)
    });

    thread::sleep(Duration::from_millis(100));
    let cancelled_at = Instant::now();
    context.cancel();
    let failure = worker.join().unwrap().unwrap_err();

    assert_eq!(failure.code, "cancelled");
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
}

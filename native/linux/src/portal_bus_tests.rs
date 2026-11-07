use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use zbus::zvariant::OwnedObjectPath;

use super::*;
use crate::bus::{SessionBus, ShellStopSubscription};

struct PrivateBus {
    process: Child,
    address: String,
}

impl PrivateBus {
    fn start() -> Self {
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is required for the portal transport tests");
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(
            !address.trim().is_empty(),
            "private bus did not publish its address"
        );
        Self {
            process,
            address: address.trim().to_owned(),
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[derive(Default)]
struct MockRequests {
    sequence: AtomicU32,
}

impl MockRequests {
    fn next_path(&self) -> OwnedObjectPath {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/test/{sequence}"
        ))
        .unwrap()
    }
}

async fn emit_response(
    connection: &Connection,
    path: &OwnedObjectPath,
    values: VariantDict,
) -> zbus::Result<()> {
    connection
        .emit_signal(
            None::<&str>,
            path,
            "org.freedesktop.portal.Request",
            "Response",
            &(0_u32, values),
        )
        .await
}

#[zbus::interface(name = "org.desktopcomputeruse.Test")]
impl MockRequests {
    async fn immediate(
        &self,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let path = self.next_path();
        // Emit before replying to reproduce the subscription race.
        emit_response(
            connection,
            &path,
            HashMap::from([("verified".to_owned(), OwnedValue::from(true))]),
        )
        .await?;
        Ok(path)
    }

    fn delayed(&self, #[zbus(connection)] connection: &Connection) -> OwnedObjectPath {
        let path = self.next_path();
        let delayed_path = path.clone();
        let connection = connection.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let _ = zbus::block_on(emit_response(
                &connection,
                &delayed_path,
                VariantDict::new(),
            ));
        });
        path
    }
}

struct MockRemoteDesktop;

#[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
impl MockRemoteDesktop {
    async fn create_session(
        &self,
        options: VariantDict,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        if !options.contains_key("session_handle_token") {
            return Err(zbus::fdo::Error::InvalidArgs("Missing token".to_owned()));
        }
        if variant_string(&options, "handle_token") != "dcu_request_test"
            || variant_string(&options, "session_handle_token") != "dcu_session_test"
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Unexpected tokens".to_owned(),
            ));
        }
        let request_path =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/test/session")
                .unwrap();
        let session = Value::from("/org/freedesktop/portal/desktop/session/test/dcu_session_test");
        let values =
            HashMap::from([("session_handle".to_owned(), session.try_to_owned().unwrap())]);
        emit_response(connection, &request_path, values).await?;
        Ok(request_path)
    }
}

#[test]
fn portal_preserves_early_responses_and_recovers_after_cancellation() {
    let bus = PrivateBus::start();
    let service = zbus::block_on(async {
        zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(PORTAL_BUS_NAME)
            .unwrap()
            .serve_at(PORTAL_DESKTOP_PATH, MockRequests::default())
            .unwrap()
            .serve_at(PORTAL_DESKTOP_PATH, MockRemoteDesktop)
            .unwrap()
            .build()
            .await
            .unwrap()
    });
    let portal = PortalBus::from_connection(connect_bus_address(&bus.address));
    let missing_session_token = HashMap::from([("handle_token", Value::from("dcu_request_test"))]);

    let rejected = portal
        .request_dict(
            "org.freedesktop.portal.RemoteDesktop",
            "CreateSession",
            &(missing_session_token,),
            None,
            |_| {},
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert_eq!(rejected.code, "permission_required");
    assert!(rejected.message.contains("Missing token"));
    assert_eq!(
        portal
            .create_session("dcu_request_test", "dcu_session_test", None)
            .unwrap(),
        "/org/freedesktop/portal/desktop/session/test/dcu_session_test"
    );

    let mut verified = false;
    portal
        .request_dict(
            "org.desktopcomputeruse.Test",
            "Immediate",
            &(),
            None,
            |values| {
                verified = values
                    .get("verified")
                    .is_some_and(|value| bool::try_from(value).unwrap_or(false))
            },
            Duration::from_secs(1),
        )
        .unwrap();

    assert!(verified);

    let context = Context::new();
    let cancel_context = context.clone();
    let cancel_worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        cancel_context.cancel();
    });
    let rejected = portal
        .request_dict(
            "org.desktopcomputeruse.Test",
            "Delayed",
            &(),
            Some(&context),
            |_| {},
            Duration::from_secs(1),
        )
        .unwrap_err();
    cancel_worker.join().unwrap();

    assert_eq!(rejected.code, "cancelled");

    thread::sleep(Duration::from_millis(100));
    let mut recovered = false;
    portal
        .request_dict(
            "org.desktopcomputeruse.Test",
            "Immediate",
            &(),
            None,
            |_| recovered = true,
            Duration::from_secs(1),
        )
        .unwrap();

    assert!(recovered);
    drop(service);
}

#[test]
fn shell_readiness_tracks_ownership_and_stop_signal() {
    let bus = PrivateBus::start();
    let shell = SessionBus::for_address(&bus.address);
    let service = connect_bus_address(&bus.address).unwrap();

    assert!(!shell.available());
    zbus::block_on(service.request_name("org.desktopcomputeruse.Shell")).unwrap();
    assert!(shell.available());

    let mut subscription = ShellStopSubscription::new(shell.connection()).unwrap();
    zbus::block_on(service.emit_signal(
        None::<&str>,
        "/org/desktopcomputeruse/Shell",
        "org.desktopcomputeruse.Shell",
        "Stopped",
        &("escape",),
    ))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut stopped = false;
    while Instant::now() < deadline && !stopped {
        stopped = subscription.poll_stopped();
        thread::sleep(Duration::from_millis(5));
    }

    assert!(stopped);
    zbus::block_on(service.release_name("org.desktopcomputeruse.Shell")).unwrap();
    assert!(!shell.available());
}

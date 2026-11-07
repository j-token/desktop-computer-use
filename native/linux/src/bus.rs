use std::time::Duration;

use dcu_core::{Error, Json};
use futures_util::{FutureExt, StreamExt};
use serde::Serialize;
use zbus::message::Message;
use zbus::zvariant::DynamicType;
use zbus::{Connection, MatchRule, MessageStream};

use crate::portal_bus::{self, connect_session_bus, expect_reply_type, reply_body};

const SHELL_BUS_NAME: &str = "org.desktopcomputeruse.Shell";
const SHELL_PATH: &str = "/org/desktopcomputeruse/Shell";
const SHELL_INTERFACE: &str = "org.desktopcomputeruse.Shell";

/// Watches the extension's `Stopped` signal on the stop-monitor thread. It
/// never calls input providers: cancellation lets their owning worker release
/// held input without racing libei or an in-progress native operation.
pub struct ShellStopSubscription {
    stream: MessageStream,
}

impl ShellStopSubscription {
    pub fn new(connection: Option<&Connection>) -> Result<Self, Error> {
        let unavailable = || Error::new("setup_required", "GNOME session bus is unavailable");
        let connection = connection.ok_or_else(unavailable)?;
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SHELL_BUS_NAME)
            .and_then(|builder| builder.interface(SHELL_INTERFACE))
            .and_then(|builder| builder.member("Stopped"))
            .and_then(|builder| builder.path(SHELL_PATH))
            .map(|builder| builder.build())
            .map_err(|_| unavailable())?;
        let stream = zbus::block_on(MessageStream::for_match_rule(rule, connection, Some(16)))
            .map_err(|_| unavailable())?;
        Ok(Self { stream })
    }

    /// A closed connection counts as stopped, like a received signal.
    pub fn poll_stopped(&mut self) -> bool {
        match self.stream.next().now_or_never() {
            None => false,
            Some(Some(Ok(_))) | Some(Some(Err(_))) | Some(None) => true,
        }
    }
}

pub struct SessionBus {
    connection: Option<Connection>,
    error: String,
}

impl SessionBus {
    pub fn new() -> Self {
        let address_present =
            std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|address| !address.is_empty());
        if !address_present {
            return Self {
                connection: None,
                error: String::new(),
            };
        }
        Self::from_connection(connect_session_bus())
    }

    #[cfg(test)]
    pub fn for_address(address: &str) -> Self {
        Self::from_connection(portal_bus::connect_bus_address(address))
    }

    fn from_connection(connection: Result<Connection, String>) -> Self {
        match connection {
            Ok(connection) => Self {
                connection: Some(connection),
                error: String::new(),
            },
            Err(error) => Self {
                connection: None,
                error,
            },
        }
    }

    /// Asks the bus for the owner on every call: no main loop runs here, so a
    /// cached owner could stay stale after GNOME Shell starts or exits.
    pub fn available(&self) -> bool {
        let Some(connection) = &self.connection else {
            return false;
        };
        portal_bus::call(
            connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            &(SHELL_BUS_NAME,),
            Duration::from_millis(500),
            None,
        )
        .and_then(|reply| expect_reply_type(reply, "NameHasOwner", "(b)"))
        .and_then(|reply| reply_body::<(bool,)>(&reply))
        .is_ok_and(|(has_owner,)| has_owner)
    }

    pub fn connection(&self) -> Option<&Connection> {
        self.connection.as_ref()
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    fn call<B>(
        &self,
        method: &str,
        body: &B,
        reply_type: &str,
        timeout_ms: u64,
    ) -> Result<Message, Error>
    where
        B: Serialize + DynamicType + Sync,
    {
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| Error::new("setup_required", "GNOME session bus is unavailable"))?;
        portal_bus::call(
            connection,
            SHELL_BUS_NAME,
            SHELL_PATH,
            SHELL_INTERFACE,
            method,
            body,
            Duration::from_millis(timeout_ms),
            None,
        )
        .and_then(|reply| expect_reply_type(reply, method, reply_type))
        .map_err(|message| {
            let message = if message.is_empty() {
                "GNOME extension rejected the request".to_owned()
            } else {
                message
            };
            Error::new("input_unavailable", message)
        })
    }

    fn call_json<B>(&self, method: &str, body: &B, timeout_ms: u64) -> Result<Json, Error>
    where
        B: Serialize + DynamicType + Sync,
    {
        let reply = self.call(method, body, "(s)", timeout_ms)?;
        let (payload,) = reply_body::<(String,)>(&reply)
            .map_err(|message| Error::new("protocol_error", message))?;
        serde_json::from_str(&payload).map_err(|error| {
            Error::new(
                "protocol_error",
                format!("GNOME extension returned invalid JSON: {error}"),
            )
        })
    }

    fn call_boolean<B>(&self, method: &str, body: &B, timeout_ms: u64) -> Result<bool, Error>
    where
        B: Serialize + DynamicType + Sync,
    {
        let reply = self.call(method, body, "(b)", timeout_ms)?;
        let (result,) = reply_body::<(bool,)>(&reply)
            .map_err(|message| Error::new("protocol_error", message))?;
        Ok(result)
    }

    pub fn call_string(
        &self,
        method: &str,
        argument: &str,
        timeout_ms: u64,
    ) -> Result<Json, Error> {
        self.call_json(method, &(argument,), timeout_ms)
    }

    pub fn call_bool(&self, method: &str, timeout_ms: u64) -> Result<bool, Error> {
        self.call_boolean(method, &(), timeout_ms)
    }

    pub fn list_windows(&self) -> Result<Json, Error> {
        self.call_json("ListWindows", &(), 5000)
    }

    pub fn activate(&self, window_id: &str) -> Result<(), Error> {
        if !self.call_boolean("Activate", &(window_id,), 5000)? {
            return Err(Error::new(
                "input_unavailable",
                "GNOME could not activate the target window",
            ));
        }
        Ok(())
    }

    pub fn pointer(&self, x: f64, y: f64) -> Result<bool, Error> {
        self.call_boolean("Pointer", &(x, y), 2000)
    }

    /// Asks the extension to unregister its Escape stop accelerator for the
    /// given time so an injected Escape reaches the focused application and is
    /// not counted as the user's emergency stop. The reply arrives only after
    /// the binding has been removed.
    pub fn suspend_stop_key(&self, duration_ms: u32) -> Result<bool, Error> {
        self.call_boolean("SuspendStopKey", &(duration_ms,), 2000)
    }

    pub fn get_overlay_regions(&self) -> Result<Json, Error> {
        let regions = self.call_json("GetOverlayRegions", &(), 2000)?;
        if !regions.is_array() {
            return Err(Error::new(
                "protocol_error",
                "GNOME extension overlay regions must be an array",
            ));
        }
        Ok(regions)
    }
}

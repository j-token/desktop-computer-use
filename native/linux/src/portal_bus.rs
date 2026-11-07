use std::collections::HashMap;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::thread;
use std::time::{Duration, Instant};

use dcu_core::{Context, Error};
use futures_util::{FutureExt, StreamExt};
use serde::Serialize;
use zbus::message::{Flags, Message};
use zbus::zvariant::{DynamicType, ObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

pub const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

/// An `a{sv}` dictionary as portals send and receive it.
pub type VariantDict = HashMap<String, OwnedValue>;

// GLib's wording, which callers have always received as error messages.
const TIMED_OUT_MESSAGE: &str = "Timeout was reached";
const CANCELLED_MESSAGE: &str = "Operation was cancelled";
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub fn connect_session_bus() -> Result<Connection, String> {
    zbus::block_on(async {
        let builder = zbus::connection::Builder::session()
            .map_err(|error| connection_error_message(&error))?;
        builder
            .build()
            .await
            .map_err(|error| connection_error_message(&error))
    })
}

pub fn connect_bus_address(address: &str) -> Result<Connection, String> {
    zbus::block_on(async {
        let builder = zbus::connection::Builder::address(address)
            .map_err(|error| connection_error_message(&error))?;
        builder
            .build()
            .await
            .map_err(|error| connection_error_message(&error))
    })
}

fn connection_error_message(error: &zbus::Error) -> String {
    match error {
        zbus::Error::Connection(io_error, _) | zbus::Error::InputOutput(io_error) => {
            format!("Could not connect: {}", io_error_text(io_error))
        }
        zbus::Error::Address(_) if std::env::var_os("DISPLAY").is_none() => {
            "Cannot autolaunch D-Bus without X11 $DISPLAY".to_owned()
        }
        other => other.to_string(),
    }
}

fn io_error_text(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => crate::backend::strerror(code),
        None => error.to_string(),
    }
}

/// Describes a failed D-Bus call the way GDBus reports it.
pub fn call_error_message(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, Some(description), _) => {
            format!("GDBus.Error:{name}: {description}")
        }
        zbus::Error::MethodError(name, None, _) => {
            format!("Error return with empty body: GDBus.Error:{name}: ")
        }
        zbus::Error::InputOutput(io_error) => match io_error.kind() {
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::NotConnected => "The connection is closed".to_owned(),
            _ => io_error_text(io_error),
        },
        other => other.to_string(),
    }
}

/// Sends a method call and waits for its reply, failing after `timeout` or
/// once `cancellation` is cancelled. The error is GDBus's message text.
#[allow(clippy::too_many_arguments)]
pub fn call<B>(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
    timeout: Duration,
    cancellation: Option<&Context>,
) -> Result<Message, String>
where
    B: Serialize + DynamicType + Sync,
{
    type Outcome = Result<Message, String>;
    zbus::block_on(async {
        let reply: Pin<Box<dyn Future<Output = Outcome> + Send + '_>> = Box::pin(async move {
            connection
                .call_method(Some(destination), path, Some(interface), method, body)
                .await
                .map_err(|error| call_error_message(&error))
        });
        let timer: Pin<Box<dyn Future<Output = Outcome> + Send>> = Box::pin(async move {
            async_io::Timer::after(timeout).await;
            Err(TIMED_OUT_MESSAGE.to_owned())
        });
        let cancelled: Pin<Box<dyn Future<Output = Outcome> + Send + '_>> = Box::pin(async move {
            let Some(context) = cancellation else {
                return std::future::pending().await;
            };
            while !context.is_cancelled() {
                async_io::Timer::after(CANCELLATION_POLL_INTERVAL).await;
            }
            Err(CANCELLED_MESSAGE.to_owned())
        });
        futures_util::future::select_all([reply, timer, cancelled])
            .await
            .0
    })
}

/// Rejects a reply whose body differs from the reply type, as GDBus does for
/// the type passed to `g_dbus_connection_call_sync`.
pub fn expect_reply_type(reply: Message, method: &str, expected: &str) -> Result<Message, String> {
    let actual = format!("({})", reply.body().signature().to_string_no_parens());
    if actual != expected {
        return Err(format!(
            "Method “{method}” returned type “{actual}”, but expected “{expected}”"
        ));
    }
    Ok(reply)
}

pub fn reply_body<T>(reply: &Message) -> Result<T, String>
where
    T: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
{
    reply
        .body()
        .deserialize::<T>()
        .map_err(|error| error.to_string())
}

/// Sends a method call without waiting for, or asking for, a reply.
pub fn send_without_reply<B>(
    connection: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) where
    B: Serialize + DynamicType + Sync,
{
    let message = Message::method_call(path, method)
        .and_then(|builder| builder.destination(destination))
        .and_then(|builder| builder.interface(interface))
        .and_then(|builder| builder.with_flags(Flags::NoReplyExpected))
        .and_then(|builder| builder.build(body));
    if let Ok(message) = message {
        let _ = zbus::block_on(connection.send(&message));
    }
}

/// A call that failed because its context was cancelled reports the
/// cancellation, not the transport error it caused.
pub fn failure_unless_cancelled(
    cancellation: Option<&Context>,
    code: &str,
    message: String,
) -> Error {
    if let Some(context) = cancellation
        && let Err(cancelled) = context.check()
    {
        return cancelled;
    }
    Error::new(code, message)
}

pub fn empty_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

/// Returns a string or object-path entry of a portal dictionary.
pub fn variant_string(dictionary: &VariantDict, key: &str) -> String {
    match dictionary.get(key).map(|value| &**value) {
        Some(Value::Str(text)) => text.to_string(),
        Some(Value::ObjectPath(path)) => path.to_string(),
        _ => String::new(),
    }
}

/// Collects portal `Request::Response` signals on its own match rule. It must
/// exist before the method call is sent, since a portal can answer before the
/// call returns.
struct ResponseSubscription {
    stream: MessageStream,
    responses: HashMap<String, (u32, Option<VariantDict>)>,
}

impl ResponseSubscription {
    fn new(connection: &Connection) -> Result<Self, String> {
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(PORTAL_BUS_NAME)
            .and_then(|builder| builder.interface("org.freedesktop.portal.Request"))
            .and_then(|builder| builder.member("Response"))
            .map(|builder| builder.build())
            .map_err(|error| error.to_string())?;
        let stream = zbus::block_on(MessageStream::for_match_rule(rule, connection, Some(64)))
            .map_err(|error| call_error_message(&error))?;
        Ok(Self {
            stream,
            responses: HashMap::new(),
        })
    }

    fn drain(&mut self) {
        while let Some(Some(Ok(message))) = self.stream.next().now_or_never() {
            let header = message.header();
            let Some(path) = header.path() else { continue };
            let Ok((code, values)) = message.body().deserialize::<(u32, VariantDict)>() else {
                continue;
            };
            self.responses
                .insert(path.to_string(), (code, Some(values)));
        }
    }

    fn wait(
        &mut self,
        path: &str,
        cancellation: Option<&Context>,
        timeout: Duration,
    ) -> Result<(u32, Option<VariantDict>), Error> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(context) = cancellation {
                context.check()?;
            }
            self.drain();
            if let Some(response) = self.responses.remove(path) {
                return Ok(response);
            }
            thread::sleep(CANCELLATION_POLL_INTERVAL);
        }
        Err(Error::new("timeout", "Portal permission request timed out"))
    }
}

/// Transport only: the backend owns device selection, stream mapping, and the
/// relationship between a RemoteDesktop session and its ScreenCast streams.
pub struct PortalBus {
    connection: Option<Connection>,
    error: String,
}

impl PortalBus {
    pub fn new() -> Self {
        Self::from_connection(connect_session_bus())
    }

    pub fn from_connection(connection: Result<Connection, String>) -> Self {
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

    pub fn available(&self) -> bool {
        self.connection.is_some()
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn connection(&self) -> Option<&Connection> {
        self.connection.as_ref()
    }

    fn require_connection(&self) -> Result<&Connection, Error> {
        self.connection
            .as_ref()
            .ok_or_else(|| Error::new("setup_required", "D-Bus session bus is unavailable"))
    }

    /// Cancellation must not block behind another portal roundtrip.
    fn close_object(&self, path: &str, interface: &str) {
        let Some(connection) = &self.connection else {
            return;
        };
        if path.is_empty() {
            return;
        }
        send_without_reply(connection, PORTAL_BUS_NAME, path, interface, "Close", &());
    }

    fn receive_descriptor(
        &self,
        interface: &str,
        method: &str,
        session_handle: &str,
        cancellation: Option<&Context>,
    ) -> Result<OwnedFd, Error> {
        let connection = self.require_connection()?;
        if let Some(context) = cancellation {
            context.check()?;
        }
        let session = ObjectPath::try_from(session_handle)
            .map_err(|error| Error::new("portal_error", error.to_string()))?;
        let reply = call(
            connection,
            PORTAL_BUS_NAME,
            PORTAL_DESKTOP_PATH,
            interface,
            method,
            &(session, empty_options()),
            Duration::from_millis(5000),
            cancellation,
        )
        .and_then(|reply| expect_reply_type(reply, method, "(h)"))
        .map_err(|message| failure_unless_cancelled(cancellation, "portal_error", message))?;
        let (descriptor,) = reply_body::<(zbus::zvariant::OwnedFd,)>(&reply)
            .map_err(|_| Error::new("protocol_error", "Portal returned no file descriptor"))?;
        Ok(descriptor.into())
    }

    pub fn create_session(
        &self,
        request_token: &str,
        session_token: &str,
        cancellation: Option<&Context>,
    ) -> Result<String, Error> {
        // The request and session have separate object paths. The portal
        // requires a session token before it can begin user authorization.
        let options = HashMap::from([
            ("handle_token", Value::from(request_token)),
            ("session_handle_token", Value::from(session_token)),
        ]);
        let mut session_handle = String::new();
        self.request_dict(
            "org.freedesktop.portal.RemoteDesktop",
            "CreateSession",
            &(options,),
            cancellation,
            |values| session_handle = variant_string(values, "session_handle"),
            Duration::from_millis(30000),
        )?;
        if session_handle.is_empty() {
            return Err(Error::new(
                "protocol_error",
                "RemoteDesktop returned no session handle",
            ));
        }
        Ok(session_handle)
    }

    pub fn request_dict<B>(
        &self,
        interface: &str,
        method: &str,
        parameters: &B,
        cancellation: Option<&Context>,
        consume: impl FnOnce(&VariantDict),
        timeout: Duration,
    ) -> Result<(), Error>
    where
        B: Serialize + DynamicType + Sync,
    {
        let connection = self.require_connection()?;
        if let Some(context) = cancellation {
            context.check()?;
        }
        let request_started = Instant::now();
        let mut subscription = ResponseSubscription::new(connection)
            .map_err(|message| Error::new("permission_required", message))?;
        let reply = call(
            connection,
            PORTAL_BUS_NAME,
            PORTAL_DESKTOP_PATH,
            interface,
            method,
            parameters,
            timeout,
            cancellation,
        )
        .and_then(|reply| expect_reply_type(reply, method, "(o)"))
        .map_err(|message| {
            failure_unless_cancelled(cancellation, "permission_required", message)
        })?;
        let (request_path,) = reply_body::<(zbus::zvariant::OwnedObjectPath,)>(&reply)
            .map_err(|_| Error::new("protocol_error", "Portal returned no request path"))?;
        let request_path = request_path.to_string();

        let remaining = timeout.saturating_sub(request_started.elapsed());
        let outcome = subscription
            .wait(&request_path, cancellation, remaining)
            .and_then(|(code, values)| {
                if code != 0 {
                    return Err(Error::new(
                        "permission_required",
                        "Portal permission was denied",
                    ));
                }
                if let Some(values) = &values {
                    consume(values);
                }
                Ok(())
            });
        if outcome.is_err() {
            self.close_object(&request_path, "org.freedesktop.portal.Request");
        }
        outcome
    }

    pub fn interface_version(
        &self,
        interface: &str,
        cancellation: Option<&Context>,
    ) -> Result<u32, Error> {
        let connection = self.require_connection()?;
        if let Some(context) = cancellation {
            context.check()?;
        }
        let reply = call(
            connection,
            PORTAL_BUS_NAME,
            PORTAL_DESKTOP_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(interface, "version"),
            Duration::from_millis(5000),
            cancellation,
        )
        .and_then(|reply| expect_reply_type(reply, "Get", "(v)"))
        .and_then(|reply| reply_body::<(OwnedValue,)>(&reply))
        .map_err(|message| failure_unless_cancelled(cancellation, "portal_error", message))?;
        match &*reply.0 {
            Value::U32(version) => Ok(*version),
            _ => Err(Error::new(
                "protocol_error",
                "Portal version must be an unsigned integer",
            )),
        }
    }

    #[cfg(feature = "pipewire")]
    pub fn open_pipewire_remote(
        &self,
        session_handle: &str,
        cancellation: Option<&Context>,
    ) -> Result<OwnedFd, Error> {
        self.receive_descriptor(
            "org.freedesktop.portal.ScreenCast",
            "OpenPipeWireRemote",
            session_handle,
            cancellation,
        )
    }

    pub fn connect_to_eis(
        &self,
        session_handle: &str,
        cancellation: Option<&Context>,
    ) -> Result<OwnedFd, Error> {
        self.receive_descriptor(
            "org.freedesktop.portal.RemoteDesktop",
            "ConnectToEIS",
            session_handle,
            cancellation,
        )
    }

    pub fn close_session(&self, session_handle: &str) {
        self.close_object(session_handle, "org.freedesktop.portal.Session");
    }
}

#[cfg(test)]
#[path = "portal_bus_tests.rs"]
mod tests;

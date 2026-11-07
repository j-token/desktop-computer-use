use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Json = serde_json::Value;

/// A failure reported to the client as `{"code", "message"}`. The code is part
/// of the protocol contract; the message is for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new("invalid_argument", message)
    }

    pub fn cancelled() -> Self {
        Self::new("cancelled", "Session stopped")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Cancellation flag of one operation or of the whole desktop session. Clones
/// share the flag, so a backend may keep one (for example on its stop-key hook
/// thread) and cancel it after the call that handed it over has returned.
#[derive(Debug, Clone, Default)]
pub struct Context {
    cancelled: Arc<AtomicBool>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }

    pub(crate) fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

/// The daemon serializes `execute` unless `supports_concurrent_execute` is
/// true; it never runs `session.start` or `session.stop` alongside another
/// call, and it runs at most one physical input (or toggle call) at a time.
/// `interrupt` must be nonblocking and must cancel every running call.
pub trait Backend: Send + Sync {
    fn supports_concurrent_execute(&self) -> bool {
        false
    }

    fn execute(&self, method: &str, params: &Json, context: &Context) -> Result<Json, Error>;

    fn interrupt(&self);

    /// Toggle keys (shift, ctrl, alt, win, space) stay held across requests.
    /// Both toggle calls are serialized like `execute`. Every session end
    /// (interrupt, `session.stop`, drop) must also release toggled keys.
    fn set_toggle(&self, key: &str, down: bool, context: &Context) -> Result<(), Error>;

    fn release_toggles(&self);
}

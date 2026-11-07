//! A synchronous AT-SPI client that issues the same D-Bus calls as libatspi
//! without its caches, which only take effect under an AT-SPI main loop.
//! Failures resolve to libatspi's fallback values: an empty name, role
//! "invalid", no states, -1 counts, and no interface.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use zbus::Connection;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value};

use crate::atspi_roles::ROLE_NAMES;
use crate::portal_bus::{self, expect_reply_type, reply_body};

const REGISTRY_BUS_NAME: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const NULL_PATH: &str = "/org/a11y/atspi/null";

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ACTION: &str = "org.a11y.atspi.Action";
const COMPONENT: &str = "org.a11y.atspi.Component";
const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const SELECTION: &str = "org.a11y.atspi.Selection";
const TEXT: &str = "org.a11y.atspi.Text";
const VALUE: &str = "org.a11y.atspi.Value";

const COORD_TYPE_SCREEN: u32 = 0;
pub const SCROLL_ANYWHERE: u32 = 6;

pub const STATE_CHECKED: u32 = 4;
pub const STATE_EXPANDABLE: u32 = 9;
pub const STATE_EXPANDED: u32 = 10;
pub const STATE_HORIZONTAL: u32 = 14;
pub const STATE_SELECTABLE: u32 = 22;
pub const STATE_SELECTED: u32 = 23;
pub const STATE_SHOWING: u32 = 25;
pub const STATE_VERTICAL: u32 = 29;
pub const STATE_INDETERMINATE: u32 = 32;
pub const STATE_CHECKABLE: u32 = 41;

use crate::atspi_roles::ROLE_EXTENDED;
pub use crate::atspi_roles::{
    ROLE_CHECK_BOX, ROLE_CHECK_MENU_ITEM, ROLE_COMBO_BOX, ROLE_LIST_ITEM, ROLE_MENU_ITEM,
    ROLE_PAGE_TAB, ROLE_RADIO_BUTTON, ROLE_RADIO_MENU_ITEM, ROLE_SCROLL_BAR, ROLE_TABLE_CELL,
    ROLE_TOGGLE_BUTTON, ROLE_TREE_ITEM,
};

const METHOD_CALL_TIMEOUT: Duration = Duration::from_millis(800);
/// libatspi waits up to this long after it first saw an application, since a
/// starting toolkit answers slowly.
const APPLICATION_STARTUP_TIME: Duration = Duration::from_millis(15000);
/// libdbus's default timeout, which libatspi uses for bus daemon calls.
const BUS_DAEMON_CALL_TIMEOUT: Duration = Duration::from_secs(25);

static APPLICATIONS_FIRST_SEEN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn note_application(bus_name: &str) {
    let mut applications = APPLICATIONS_FIRST_SEEN.lock();
    applications
        .get_or_insert_with(HashMap::new)
        .entry(bus_name.to_owned())
        .or_insert_with(Instant::now);
}

fn call_timeout(bus_name: &str) -> Duration {
    let applications = APPLICATIONS_FIRST_SEEN.lock();
    let since_first_seen = applications
        .as_ref()
        .and_then(|applications| applications.get(bus_name))
        .map_or(APPLICATION_STARTUP_TIME, Instant::elapsed);
    APPLICATION_STARTUP_TIME
        .saturating_sub(since_first_seen)
        .max(METHOD_CALL_TIMEOUT)
}

/// Connects to the accessibility bus the way libatspi finds it: the
/// AT_SPI_BUS_ADDRESS variable, then the X11 root window property on an X11
/// session, then org.a11y.Bus on the session bus.
pub fn connect_accessibility_bus() -> Option<Connection> {
    let address = env_address()
        .or_else(x11_address)
        .or_else(session_bus_address)?;
    portal_bus::connect_bus_address(&address).ok()
}

fn env_address() -> Option<String> {
    std::env::var("AT_SPI_BUS_ADDRESS")
        .ok()
        .filter(|address| !address.is_empty())
}

fn x11_address() -> Option<String> {
    let on_x11 =
        std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    if !on_x11 {
        return None;
    }
    crate::x11::root_string_property("AT_SPI_BUS").filter(|address| !address.is_empty())
}

fn session_bus_address() -> Option<String> {
    let session = portal_bus::connect_session_bus().ok()?;
    let reply = portal_bus::call(
        &session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus",
        "GetAddress",
        &(),
        Duration::from_secs(25),
        None,
    )
    .and_then(|reply| expect_reply_type(reply, "GetAddress", "(s)"))
    .ok()?;
    reply_body::<(String,)>(&reply)
        .ok()
        .map(|(address,)| address)
}

#[derive(Clone)]
pub struct Accessible {
    connection: Connection,
    bus_name: String,
    path: String,
    interfaces: OnceCell<Vec<String>>,
}

impl Accessible {
    pub fn desktop(connection: &Connection) -> Self {
        Self::new(
            connection.clone(),
            REGISTRY_BUS_NAME.to_owned(),
            ROOT_PATH.to_owned(),
        )
    }

    fn new(connection: Connection, bus_name: String, path: String) -> Self {
        note_application(&bus_name);
        Self {
            connection,
            bus_name,
            path,
            interfaces: OnceCell::new(),
        }
    }

    fn referenced(&self, (bus_name, path): (String, OwnedObjectPath)) -> Option<Self> {
        if path.as_str() == NULL_PATH {
            return None;
        }
        Some(Self::new(
            self.connection.clone(),
            bus_name,
            path.to_string(),
        ))
    }

    pub fn same_object(&self, other: &Self) -> bool {
        self.bus_name == other.bus_name && self.path == other.path
    }

    fn call<B, R>(&self, interface: &str, method: &str, body: &B, reply_type: &str) -> Option<R>
    where
        B: Serialize + DynamicType + Sync,
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let reply = portal_bus::call(
            &self.connection,
            &self.bus_name,
            &self.path,
            interface,
            method,
            body,
            call_timeout(&self.bus_name),
            None,
        )
        .and_then(|reply| expect_reply_type(reply, method, reply_type))
        .ok()?;
        reply_body::<R>(&reply).ok()
    }

    fn property(&self, interface: &str, name: &str) -> Option<OwnedValue> {
        self.call::<_, (OwnedValue,)>(
            "org.freedesktop.DBus.Properties",
            "Get",
            &(interface, name),
            "(v)",
        )
        .map(|(value,)| value)
    }

    fn string_property(&self, interface: &str, name: &str) -> Option<String> {
        match &*self.property(interface, name)? {
            Value::Str(text) => Some(text.to_string()),
            _ => None,
        }
    }

    fn i32_property(&self, interface: &str, name: &str) -> Option<i32> {
        match &*self.property(interface, name)? {
            Value::I32(number) => Some(*number),
            _ => None,
        }
    }

    fn f64_property(&self, interface: &str, name: &str) -> f64 {
        match self.property(interface, name).as_deref() {
            Some(Value::F64(number)) => *number,
            _ => 0.0,
        }
    }

    pub fn implements(&self, interface: &str) -> bool {
        let interfaces = self.interfaces.get_or_init(|| {
            self.call::<_, (Vec<String>,)>(ACCESSIBLE, "GetInterfaces", &(), "(as)")
                .map(|(interfaces,)| interfaces)
                .unwrap_or_default()
        });
        interfaces.iter().any(|candidate| candidate == interface)
    }

    pub fn name(&self) -> String {
        self.string_property(ACCESSIBLE, "Name").unwrap_or_default()
    }

    pub fn accessible_id(&self) -> String {
        self.string_property(ACCESSIBLE, "AccessibleId")
            .unwrap_or_default()
    }

    pub fn role(&self) -> u32 {
        self.call::<_, (u32,)>(ACCESSIBLE, "GetRole", &(), "(u)")
            .map(|(role,)| role)
            .unwrap_or(0)
    }

    /// libatspi names a known role after its enum nick with dashes turned
    /// into spaces, and asks the provider only for other roles.
    pub fn role_name(&self) -> String {
        let role = self.role();
        if role != ROLE_EXTENDED
            && let Some(name) = ROLE_NAMES.get(role as usize)
        {
            return (*name).to_owned();
        }
        self.call::<_, (String,)>(ACCESSIBLE, "GetRoleName", &(), "(s)")
            .map(|(name,)| name)
            .unwrap_or_default()
    }

    pub fn has_state(&self, state: u32) -> bool {
        let Some((words,)) = self.call::<_, (Vec<u32>,)>(ACCESSIBLE, "GetState", &(), "(au)")
        else {
            return false;
        };
        let word = (state / 32) as usize;
        words
            .get(word)
            .is_some_and(|bits| bits & (1 << (state % 32)) != 0)
    }

    pub fn child_count(&self) -> i32 {
        self.i32_property(ACCESSIBLE, "ChildCount").unwrap_or(-1)
    }

    /// zvariant parses a reply holding one struct, such as `((so))`, into the
    /// struct's own fields, so single-struct replies are matched as `(so)`.
    pub fn child_at(&self, index: i32) -> Option<Self> {
        let reference = self.call(ACCESSIBLE, "GetChildAtIndex", &(index,), "(so)")?;
        self.referenced(reference)
    }

    pub fn parent(&self) -> Option<Self> {
        let value = self.property(ACCESSIBLE, "Parent")?;
        let reference = <(String, OwnedObjectPath)>::try_from(value).ok()?;
        self.referenced(reference)
    }

    pub fn index_in_parent(&self) -> i32 {
        self.call::<_, (i32,)>(ACCESSIBLE, "GetIndexInParent", &(), "(i)")
            .map(|(index,)| index)
            .unwrap_or(-1)
    }

    pub fn process_id(&self) -> u32 {
        portal_bus::call(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            &(self.bus_name.as_str(),),
            BUS_DAEMON_CALL_TIMEOUT,
            None,
        )
        .ok()
        .and_then(|reply| reply_body::<(u32,)>(&reply).ok())
        .map_or(u32::MAX, |(pid,)| pid)
    }

    /// Screen extents as x, y, width, height; (-1, -1, -1, -1) when the
    /// provider fails, as libatspi reports it.
    pub fn extents(&self) -> Option<[i32; 4]> {
        if !self.implements(COMPONENT) {
            return None;
        }
        let (x, y, width, height) = self
            .call::<_, (i32, i32, i32, i32)>(
                COMPONENT,
                "GetExtents",
                &(COORD_TYPE_SCREEN,),
                "(iiii)",
            )
            .unwrap_or((-1, -1, -1, -1));
        Some([x, y, width, height])
    }

    pub fn has_component(&self) -> bool {
        self.implements(COMPONENT)
    }

    pub fn scroll_to(&self, kind: u32) -> bool {
        self.call::<_, (bool,)>(COMPONENT, "ScrollTo", &(kind,), "(b)")
            .is_some_and(|(scrolled,)| scrolled)
    }

    pub fn has_text(&self) -> bool {
        self.implements(TEXT)
    }

    pub fn character_count(&self) -> i32 {
        self.i32_property(TEXT, "CharacterCount").unwrap_or(0)
    }

    pub fn text(&self, start: i32, end: i32) -> String {
        self.call::<_, (String,)>(TEXT, "GetText", &(start, end), "(s)")
            .map(|(text,)| text)
            .unwrap_or_default()
    }

    pub fn has_value(&self) -> bool {
        self.implements(VALUE)
    }

    pub fn current_value(&self) -> f64 {
        self.f64_property(VALUE, "CurrentValue")
    }

    pub fn minimum_value(&self) -> f64 {
        self.f64_property(VALUE, "MinimumValue")
    }

    pub fn maximum_value(&self) -> f64 {
        self.f64_property(VALUE, "MaximumValue")
    }

    pub fn set_current_value(&self, value: f64) -> bool {
        self.call::<_, ()>(
            "org.freedesktop.DBus.Properties",
            "Set",
            &(VALUE, "CurrentValue", Value::F64(value)),
            "()",
        )
        .is_some()
    }

    pub fn has_editable_text(&self) -> bool {
        self.implements(EDITABLE_TEXT)
    }

    pub fn set_text_contents(&self, contents: &str) -> bool {
        self.call::<_, (bool,)>(EDITABLE_TEXT, "SetTextContents", &(contents,), "(b)")
            .is_some_and(|(set,)| set)
    }

    pub fn has_action(&self) -> bool {
        self.implements(ACTION)
    }

    pub fn action_count(&self) -> i32 {
        self.i32_property(ACTION, "NActions").unwrap_or(0)
    }

    pub fn action_name(&self, index: i32) -> Option<String> {
        self.call::<_, (String,)>(ACTION, "GetName", &(index,), "(s)")
            .map(|(name,)| name)
    }

    pub fn do_action(&self, index: i32) -> bool {
        self.call::<_, (bool,)>(ACTION, "DoAction", &(index,), "(b)")
            .is_some_and(|(done,)| done)
    }

    pub fn has_selection(&self) -> bool {
        self.implements(SELECTION)
    }

    pub fn select_child(&self, index: i32) -> bool {
        self.call::<_, (bool,)>(SELECTION, "SelectChild", &(index,), "(b)")
            .is_some_and(|(selected,)| selected)
    }
}

#[cfg(test)]
mod tests {
    use zbus::Message;
    use zbus::zvariant::ObjectPath;

    use super::*;

    #[test]
    fn reply_holding_one_struct_matches_and_decodes_as_its_fields() {
        let reference = (REGISTRY_BUS_NAME, ObjectPath::try_from(ROOT_PATH).unwrap());
        let reply = Message::method_call("/", "GetChildAtIndex")
            .unwrap()
            .build(&(reference,))
            .unwrap();

        let reply = expect_reply_type(reply, "GetChildAtIndex", "(so)").unwrap();
        let (bus_name, path) = reply_body::<(String, OwnedObjectPath)>(&reply).unwrap();

        assert_eq!(
            (bus_name.as_str(), path.as_str()),
            (REGISTRY_BUS_NAME, ROOT_PATH)
        );
    }
}

//! The X11 requests the backend makes, with the semantics of the Xlib calls
//! it replaced (XGetWindowProperty, XFetchName, XGetClassHint, XGetImage with
//! XGetPixel, XKeysymToKeycode, and XTest).

use x11rb::connection::Connection as _;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, AtomEnum, ConfigureWindowAux, ConnectionExt as _, ImageFormat, ImageOrder, InputFocus,
    MapState, StackMode, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::x11_keysyms::keysym_from_name;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;
/// Xlib's BUFSIZ-sized reads, in 32-bit units.
const XLIB_BUFSIZ_LONGS: u32 = 8192;

pub const XK_CONTROL_L: u32 = 0xffe3;
pub const XK_SHIFT_L: u32 = 0xffe1;
pub const XK_ALT_L: u32 = 0xffe9;
pub const XK_SUPER_L: u32 = 0xffeb;
pub const XK_SPACE: u32 = 0x20;

pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct WindowProperties {
    pub pid: i32,
    pub title: String,
    pub app: String,
    pub transient_for: Option<Window>,
    pub modal: bool,
    pub geometry: Option<Geometry>,
}

pub struct Capture {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

pub struct Crtc {
    pub id: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub rotation: u16,
}

pub struct X11Display {
    connection: RustConnection,
    screen_number: usize,
    root: Window,
}

/// Reads one STRING property of the default root window over a short-lived
/// connection, as libatspi does for AT_SPI_BUS.
pub fn root_string_property(name: &str) -> Option<String> {
    let display = X11Display::open()?;
    let atom = display.intern_atom(name)?;
    let reply = display
        .connection
        .get_property(
            false,
            display.root,
            atom,
            AtomEnum::STRING,
            0,
            XLIB_BUFSIZ_LONGS,
        )
        .ok()?
        .reply()
        .ok()?;
    if reply.type_ != u32::from(AtomEnum::STRING) || reply.format != 8 {
        return None;
    }
    Some(c_string(&reply.value))
}

/// Text up to the first NUL, as Xlib callers read a returned C string.
fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl X11Display {
    pub fn open() -> Option<Self> {
        let (connection, screen_number) = x11rb::connect(None).ok()?;
        let root = connection.setup().roots.get(screen_number)?.root;
        Some(Self {
            connection,
            screen_number,
            root,
        })
    }

    fn screen(&self) -> &xproto::Screen {
        &self.connection.setup().roots[self.screen_number]
    }

    pub fn intern_atom(&self, name: &str) -> Option<u32> {
        let reply = self
            .connection
            .intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()?;
        (reply.atom != x11rb::NONE).then_some(reply.atom)
    }

    fn property(
        &self,
        window: Window,
        atom: u32,
        kind: impl Into<u32>,
        length: u32,
    ) -> Option<xproto::GetPropertyReply> {
        self.connection
            .get_property(false, window, atom, kind, 0, length)
            .ok()?
            .reply()
            .ok()
    }

    /// Any 8-bit property as raw text, embedded NULs included.
    fn property_string(&self, window: Window, name: &str) -> String {
        let Some(atom) = self.intern_atom(name) else {
            return String::new();
        };
        let Some(reply) = self.property(window, atom, AtomEnum::ANY, 4096) else {
            return String::new();
        };
        if reply.format != 8 {
            return String::new();
        }
        String::from_utf8_lossy(&reply.value).into_owned()
    }

    /// The first 32-bit item of any property, or 0.
    pub fn property_cardinal(&self, window: Window, name: &str) -> u32 {
        let Some(atom) = self.intern_atom(name) else {
            return 0;
        };
        let Some(reply) = self.property(window, atom, AtomEnum::ANY, 1) else {
            return 0;
        };
        reply
            .value32()
            .and_then(|mut values| values.next())
            .unwrap_or(0)
    }

    fn has_state(&self, window: Window, state_name: &str) -> bool {
        let (Some(state_atom), Some(wanted)) = (
            self.intern_atom("_NET_WM_STATE"),
            self.intern_atom(state_name),
        ) else {
            return false;
        };
        let Some(reply) = self.property(window, state_atom, AtomEnum::ATOM, 64) else {
            return false;
        };
        reply
            .value32()
            .is_some_and(|mut atoms| atoms.any(|atom| atom == wanted))
    }

    fn fetch_name(&self, window: Window) -> String {
        let Some(reply) = self.property(
            window,
            AtomEnum::WM_NAME.into(),
            AtomEnum::STRING,
            XLIB_BUFSIZ_LONGS,
        ) else {
            return String::new();
        };
        if reply.type_ != u32::from(AtomEnum::STRING) || reply.format != 8 {
            return String::new();
        }
        c_string(&reply.value)
    }

    /// WM_CLASS as (res_name, res_class).
    fn class_hint(&self, window: Window) -> Option<(String, String)> {
        let reply = self.property(
            window,
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING,
            XLIB_BUFSIZ_LONGS,
        )?;
        if reply.type_ != u32::from(AtomEnum::STRING) || reply.format != 8 {
            return None;
        }
        let name_length = reply
            .value
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(reply.value.len());
        let class = reply
            .value
            .get(name_length + 1..)
            .map(c_string)
            .unwrap_or_default();
        Some((c_string(&reply.value), class))
    }

    fn transient_for(&self, window: Window) -> Option<Window> {
        let reply = self.property(
            window,
            AtomEnum::WM_TRANSIENT_FOR.into(),
            AtomEnum::WINDOW,
            1,
        )?;
        if reply.type_ != u32::from(AtomEnum::WINDOW) {
            return None;
        }
        reply.value32()?.next()
    }

    pub fn is_viewable(&self, window: Window) -> bool {
        let Some(attributes) = self
            .connection
            .get_window_attributes(window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return false;
        };
        let Some(geometry) = self
            .connection
            .get_geometry(window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return false;
        };
        attributes.map_state == MapState::VIEWABLE && geometry.width > 0 && geometry.height > 0
    }

    fn geometry(&self, window: Window) -> Option<Geometry> {
        self.connection
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?;
        let geometry = self.connection.get_geometry(window).ok()?.reply().ok()?;
        let translated = self
            .connection
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok();
        let (x, y) = translated.map_or((0, 0), |reply| {
            (i32::from(reply.dst_x), i32::from(reply.dst_y))
        });
        Some(Geometry {
            x,
            y,
            width: i32::from(geometry.width),
            height: i32::from(geometry.height),
        })
    }

    pub fn window_properties(&self, window: Window) -> WindowProperties {
        let pid = self.property_cardinal(window, "_NET_WM_PID") as i32;
        let mut title = self.property_string(window, "_NET_WM_NAME");
        if title.is_empty() {
            title = self.fetch_name(window);
        }
        let app = self
            .class_hint(window)
            .map(|(name, class)| if class.is_empty() { name } else { class })
            .unwrap_or_default();
        let transient_for = self
            .transient_for(window)
            .filter(|owner| *owner != 0 && *owner != self.root && *owner != window);
        let modal = transient_for.is_some() && self.has_state(window, "_NET_WM_STATE_MODAL");
        WindowProperties {
            pid,
            title,
            app,
            transient_for,
            modal,
            geometry: self.geometry(window),
        }
    }

    /// _NET_CLIENT_LIST, or the root's children when the window manager
    /// publishes no client list.
    pub fn client_windows(&self) -> Vec<Window> {
        let listed = self
            .intern_atom("_NET_CLIENT_LIST")
            .and_then(|atom| self.property(self.root, atom, AtomEnum::WINDOW, 4096))
            .and_then(|reply| reply.value32().map(Iterator::collect::<Vec<_>>))
            .unwrap_or_default();
        if !listed.is_empty() {
            return listed;
        }
        self.connection
            .query_tree(self.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.children)
            .unwrap_or_default()
    }

    pub fn active_window(&self) -> Window {
        self.property_cardinal(self.root, "_NET_ACTIVE_WINDOW")
    }

    pub fn root_size(&self) -> (u16, u16) {
        let screen = self.screen();
        (screen.width_in_pixels, screen.height_in_pixels)
    }

    pub fn crtcs(&self) -> Vec<Crtc> {
        let Some(resources) = self
            .connection
            .randr_get_screen_resources_current(self.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return Vec::new();
        };
        resources
            .crtcs
            .iter()
            .filter_map(|&crtc| {
                let info: randr::GetCrtcInfoReply = self
                    .connection
                    .randr_get_crtc_info(crtc, resources.config_timestamp)
                    .ok()?
                    .reply()
                    .ok()?;
                Some(Crtc {
                    id: crtc,
                    x: info.x,
                    y: info.y,
                    width: info.width,
                    height: info.height,
                    rotation: info.rotation.bits(),
                })
            })
            .collect()
    }

    /// The first keycode, scanning symbol columns in order, whose mapping
    /// holds `keysym`; 0 when none does.
    pub fn keycode_for(&self, keysym: u32) -> u8 {
        let setup = self.connection.setup();
        let (minimum, maximum) = (setup.min_keycode, setup.max_keycode);
        let Some(mapping) = self
            .connection
            .get_keyboard_mapping(minimum, maximum - minimum + 1)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return 0;
        };
        let per_keycode = usize::from(mapping.keysyms_per_keycode);
        for column in 0..per_keycode {
            for (offset, keycode) in (minimum..=maximum).enumerate() {
                if mapping.keysyms.get(offset * per_keycode + column) == Some(&keysym) {
                    return keycode;
                }
            }
        }
        0
    }

    fn fake_input(&self, kind: u8, detail: u8, x: i16, y: i16) {
        let _ = self.connection.xtest_fake_input(
            kind,
            detail,
            x11rb::CURRENT_TIME,
            x11rb::NONE,
            x,
            y,
            0,
        );
    }

    pub fn fake_motion(&self, x: i32, y: i32) {
        self.fake_input(MOTION_NOTIFY, 0, x as i16, y as i16);
    }

    pub fn fake_button(&self, button: u8, pressed: bool) {
        self.fake_input(
            if pressed {
                BUTTON_PRESS
            } else {
                BUTTON_RELEASE
            },
            button,
            0,
            0,
        );
    }

    pub fn fake_key(&self, keycode: u8, pressed: bool) {
        self.fake_input(if pressed { KEY_PRESS } else { KEY_RELEASE }, keycode, 0, 0);
    }

    pub fn flush(&self) {
        let _ = self.connection.flush();
    }

    pub fn raise_and_focus(&self, window: Window) {
        let _ = self.connection.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        let _ = self
            .connection
            .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME);
        self.flush();
    }

    /// Captures a root-window rectangle as RGBA, scaling each channel mask of
    /// the root visual to 0..=255 as XGetPixel-based conversion did.
    pub fn capture(&self, x: i32, y: i32, width: i32, height: i32) -> Option<Capture> {
        let image = self
            .connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                x as i16,
                y as i16,
                width as u16,
                height as u16,
                !0,
            )
            .ok()?
            .reply()
            .ok()?;
        let setup = self.connection.setup();
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == image.depth)?;
        let bits_per_pixel = usize::from(format.bits_per_pixel);
        let scanline_pad = usize::from(format.scanline_pad).max(8);
        let bytes_per_line =
            (width as usize * bits_per_pixel).div_ceil(scanline_pad) * scanline_pad / 8;
        let visual = self
            .screen()
            .allowed_depths
            .iter()
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| visual.visual_id == image.visual);
        let (red_mask, green_mask, blue_mask) = visual.map_or((0, 0, 0), |visual| {
            (visual.red_mask, visual.green_mask, visual.blue_mask)
        });
        let depth_mask = if image.depth >= 32 {
            u32::MAX
        } else {
            (1u32 << image.depth) - 1
        };
        let most_significant_first = setup.image_byte_order == ImageOrder::MSB_FIRST;
        let bytes_per_pixel = bits_per_pixel / 8;

        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        for row in 0..height as usize {
            for column in 0..width as usize {
                let start = row * bytes_per_line + column * bytes_per_pixel;
                let Some(bytes) = image
                    .data
                    .get(start..start + bytes_per_pixel)
                    .filter(|_| bytes_per_pixel > 0)
                else {
                    continue;
                };
                let raw = bytes.iter().enumerate().fold(0u32, |pixel, (index, byte)| {
                    let shift = if most_significant_first {
                        (bytes_per_pixel - 1 - index) * 8
                    } else {
                        index * 8
                    };
                    pixel | (u32::from(*byte) << shift)
                });
                let pixel = raw & depth_mask;
                let target = (row * width as usize + column) * 4;
                rgba[target] = mask_component(pixel, red_mask);
                rgba[target + 1] = mask_component(pixel, green_mask);
                rgba[target + 2] = mask_component(pixel, blue_mask);
                rgba[target + 3] = 255;
            }
        }
        Some(Capture {
            width,
            height,
            rgba,
        })
    }
}

fn mask_component(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let range = u64::from(mask >> shift);
    (u64::from((pixel & mask) >> shift) * 255 / range) as u8
}

/// X11 keysym of a canonical key name, as XStringToKeysym resolved it after
/// the named-key aliases.
pub fn keysym(key: &str) -> u32 {
    match key {
        "ctrl" => XK_CONTROL_L,
        "shift" => XK_SHIFT_L,
        "alt" => XK_ALT_L,
        "super" => XK_SUPER_L,
        "enter" => 0xff0d,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "space" => XK_SPACE,
        "left" => 0xff51,
        "right" => 0xff53,
        "up" => 0xff52,
        "down" => 0xff54,
        "home" => 0xff50,
        "end" => 0xff57,
        "insert" => 0xff63,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "pause" => 0xff13,
        "printscreen" => 0xff61,
        "capslock" => 0xffe5,
        _ => keysym_from_name(key),
    }
}

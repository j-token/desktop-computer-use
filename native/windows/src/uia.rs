//! UI Automation is intentionally separate from screenshot capture. A
//! screenshot-only observation never instantiates UIAutomation or walks the
//! accessibility tree.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dcu_core::{Context, Error, Json};
use serde_json::json;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VT_ARRAY, VT_BOOL, VT_BSTR, VT_I4, VT_R8, VT_UI4,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{BSTR, IUnknown, Interface, Ref, implement};

use crate::Mta;

pub(crate) struct ElementRecord {
    pub(crate) description: Json,
    pub(crate) element: Mta<IUIAutomationElement>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ScrollPosition {
    horizontal_percent: f64,
    vertical_percent: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StateChange {
    pub(crate) pattern: &'static str,
    pub(crate) changed: bool,
    pub(crate) verified: bool,
}

pub(crate) struct AccessibilitySnapshot {
    pub(crate) value: Json,
    pub(crate) elements: Vec<ElementRecord>,
}

const MAXIMUM_ANCESTORS: usize = 32;
const SETTLE_TIMEOUT: Duration = Duration::from_millis(600);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Runs a call that crosses into the inspected application's UI Automation
/// provider. A fault there arrives as a structured exception, which Rust
/// cannot unwind and which would otherwise take the whole daemon down. The
/// closure must own nothing that needs dropping: a fault skips its cleanup,
/// and releasing a pointer the faulted call may have written would be a
/// second, real crash, so whatever it held is abandoned deliberately.
/// Returns the structured exception code on a fault.
fn guarded<R>(call: impl FnMut() -> R) -> Result<R, u32> {
    microseh::try_seh(call).map_err(|exception| exception.raw_code())
}

fn description_field<'a>(description: &'a Json, key: &str) -> &'a str {
    description
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
}

/// One line per caught fault, in the daemon log next to the transport
/// messages, so a provider that misbehaves is never silent.
fn report_pattern_fault(call: &str, description: &Json, code: u32) {
    let control_type = description_field(description, "controlType");
    let name = description_field(description, "name");
    let mut line = format!(
        "uia: {call} raised structured exception 0x{code:X} on {}",
        if control_type.is_empty() {
            "<unknown control type>"
        } else {
            control_type
        }
    );
    if !name.is_empty() {
        line.push_str(&format!(" \"{name}\""));
    }
    line.push_str("; treating the pattern as unusable\n");
    eprint!("{line}");
}

/// A string with a lone surrogate becomes empty, as WC_ERR_INVALID_CHARS
/// conversion did in earlier releases, so the tree text stays the same.
fn utf8(value: &[u16]) -> String {
    String::from_utf16(value).unwrap_or_default()
}

fn ascii_lowercase_wide(value: &[u16]) -> Vec<u16> {
    value
        .iter()
        .map(|&unit| {
            if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        })
        .collect()
}

fn looks_sensitive(name: &[u16], value: &[u16], is_password: bool) -> bool {
    if is_password {
        return true;
    }
    let haystack = format!(
        "{} {}",
        String::from_utf16_lossy(name),
        String::from_utf16_lossy(value)
    )
    .to_ascii_lowercase();
    ["password", "passcode", "secret", "token"]
        .iter()
        .any(|token| haystack.contains(token))
}

const CONTROL_TYPE_NAMES: [(UIA_CONTROLTYPE_ID, &str); 41] = [
    (UIA_ButtonControlTypeId, "Button"),
    (UIA_CheckBoxControlTypeId, "CheckBox"),
    (UIA_ComboBoxControlTypeId, "ComboBox"),
    (UIA_EditControlTypeId, "Edit"),
    (UIA_HyperlinkControlTypeId, "Hyperlink"),
    (UIA_ImageControlTypeId, "Image"),
    (UIA_ListControlTypeId, "List"),
    (UIA_ListItemControlTypeId, "ListItem"),
    (UIA_MenuControlTypeId, "Menu"),
    (UIA_MenuItemControlTypeId, "MenuItem"),
    (UIA_PaneControlTypeId, "Pane"),
    (UIA_RadioButtonControlTypeId, "RadioButton"),
    (UIA_ScrollBarControlTypeId, "ScrollBar"),
    (UIA_SliderControlTypeId, "Slider"),
    (UIA_TabControlTypeId, "Tab"),
    (UIA_TabItemControlTypeId, "TabItem"),
    (UIA_TextControlTypeId, "Text"),
    (UIA_TreeControlTypeId, "Tree"),
    (UIA_TreeItemControlTypeId, "TreeItem"),
    (UIA_WindowControlTypeId, "Window"),
    (UIA_CalendarControlTypeId, "Calendar"),
    (UIA_MenuBarControlTypeId, "MenuBar"),
    (UIA_ProgressBarControlTypeId, "ProgressBar"),
    (UIA_SpinnerControlTypeId, "Spinner"),
    (UIA_StatusBarControlTypeId, "StatusBar"),
    (UIA_ToolBarControlTypeId, "ToolBar"),
    (UIA_ToolTipControlTypeId, "ToolTip"),
    (UIA_CustomControlTypeId, "Custom"),
    (UIA_GroupControlTypeId, "Group"),
    (UIA_ThumbControlTypeId, "Thumb"),
    (UIA_DataGridControlTypeId, "DataGrid"),
    (UIA_DataItemControlTypeId, "DataItem"),
    (UIA_DocumentControlTypeId, "Document"),
    (UIA_SplitButtonControlTypeId, "SplitButton"),
    (UIA_HeaderControlTypeId, "Header"),
    (UIA_HeaderItemControlTypeId, "HeaderItem"),
    (UIA_TableControlTypeId, "Table"),
    (UIA_TitleBarControlTypeId, "TitleBar"),
    (UIA_SeparatorControlTypeId, "Separator"),
    (UIA_SemanticZoomControlTypeId, "SemanticZoom"),
    (UIA_AppBarControlTypeId, "AppBar"),
];

fn control_type_name(control_type: i32) -> String {
    CONTROL_TYPE_NAMES
        .iter()
        .find(|(id, _)| id.0 == control_type)
        .map_or_else(
            || format!("ControlType_{control_type}"),
            |(_, name)| (*name).to_owned(),
        )
}

fn cached_property(element: &IUIAutomationElement, property: UIA_PROPERTY_ID) -> Option<VARIANT> {
    unsafe { element.GetCachedPropertyValue(property) }.ok()
}

fn cached_string(element: &IUIAutomationElement, property: UIA_PROPERTY_ID) -> Vec<u16> {
    let Some(value) = cached_property(element, property) else {
        return Vec::new();
    };
    if value.vt() != VT_BSTR {
        return Vec::new();
    }
    // SAFETY: the variant holds a BSTR, checked above.
    unsafe { value.Anonymous.Anonymous.Anonymous.bstrVal.to_vec() }
}

fn cached_bool(element: &IUIAutomationElement, property: UIA_PROPERTY_ID) -> bool {
    let Some(value) = cached_property(element, property) else {
        return false;
    };
    // SAFETY: boolVal is read only for a VT_BOOL variant.
    value.vt() == VT_BOOL && unsafe { value.Anonymous.Anonymous.Anonymous.boolVal }.0 == -1
}

fn cached_int(element: &IUIAutomationElement, property: UIA_PROPERTY_ID) -> i32 {
    let Some(value) = cached_property(element, property) else {
        return 0;
    };
    // SAFETY: each field is read only for its own variant type.
    unsafe {
        match value.vt() {
            VT_I4 => value.Anonymous.Anonymous.Anonymous.lVal,
            VT_UI4 => value.Anonymous.Anonymous.Anonymous.ulVal as i32,
            _ => 0,
        }
    }
}

/// The one-dimensional SAFEARRAY of `element_type` in `value`, copied out.
fn cached_array<T: Copy>(value: &VARIANT, element_type: VARENUM) -> Option<Vec<T>> {
    if value.vt() != VARENUM(VT_ARRAY.0 | element_type.0) {
        return None;
    }
    // SAFETY: an array variant holds a SAFEARRAY pointer, which is checked
    // for null; its data stays valid between AccessData and UnaccessData.
    unsafe {
        let array: *const SAFEARRAY = value.Anonymous.Anonymous.Anonymous.parray;
        if array.is_null() {
            return None;
        }
        let lower_bound = SafeArrayGetLBound(array, 1).ok()?;
        let upper_bound = SafeArrayGetUBound(array, 1).ok()?;
        if upper_bound < lower_bound {
            return Some(Vec::new());
        }
        let count = (upper_bound - lower_bound + 1) as usize;
        let mut data = std::ptr::null_mut();
        SafeArrayAccessData(array, &mut data).ok()?;
        let values = std::slice::from_raw_parts(data.cast::<T>(), count).to_vec();
        let _ = SafeArrayUnaccessData(array);
        Some(values)
    }
}

fn cached_bounds(element: &IUIAutomationElement, window_rect: &RECT) -> RECT {
    let values = cached_property(element, UIA_BoundingRectanglePropertyId)
        .and_then(|value| cached_array::<f64>(&value, VT_R8))
        .filter(|values| values.len() >= 4);
    let Some(values) = values else {
        return RECT::default();
    };
    let (left, top, width, height) = (values[0], values[1], values[2], values[3]);
    RECT {
        left: left.round() as i32 - window_rect.left,
        top: top.round() as i32 - window_rect.top,
        right: (left + width).round() as i32 - window_rect.left,
        bottom: (top + height).round() as i32 - window_rect.top,
    }
}

fn cached_runtime_id(element: &IUIAutomationElement) -> Vec<i32> {
    cached_property(element, UIA_RuntimeIdPropertyId)
        .and_then(|value| cached_array::<i32>(&value, VT_I4))
        .unwrap_or_default()
}

/// A cached pattern the element does not support still succeeds and yields a
/// null interface, so the pointer decides support, not the return code.
fn has_cached_pattern(element: &IUIAutomationElement, pattern: UIA_PATTERN_ID) -> bool {
    unsafe { element.GetCachedPattern(pattern) }.is_ok()
}

fn rect_text(rect: &RECT) -> String {
    format!(
        "{},{} {}x{}",
        rect.left,
        rect.top,
        (rect.right - rect.left).max(0),
        (rect.bottom - rect.top).max(0)
    )
}

pub(crate) fn uia_failure(error: &windows::core::Error) -> Error {
    Error::new(
        "provider_error",
        format!("UI Automation failed: {}", error.message()),
    )
}

fn create_automation() -> Option<IUIAutomation> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok()
}

pub(crate) fn build_snapshot(
    hwnd: HWND,
    window_rect: &RECT,
    context: &Context,
) -> Result<AccessibilitySnapshot, Error> {
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(Error::new(
            "window_not_found",
            "The target window no longer exists",
        ));
    }
    context.check()?;
    let automation = create_automation()
        .ok_or_else(|| Error::new("uia_unavailable", "UI Automation is unavailable"))?;
    let all = find_all_cached(&automation, hwnd).map_err(|error| uia_failure(&error))?;
    let length = unsafe { all.Length() }.map_err(|error| uia_failure(&error))?;

    let mut tree = String::new();
    let mut descriptions = Vec::new();
    let mut elements = Vec::new();
    for position in 0..length {
        context.check()?;
        let Ok(element) = (unsafe { all.GetElement(position) }) else {
            continue;
        };
        let name = cached_string(&element, UIA_NamePropertyId);
        let automation_id = cached_string(&element, UIA_AutomationIdPropertyId);
        let class_name = cached_string(&element, UIA_ClassNamePropertyId);
        let raw_value = cached_string(&element, UIA_ValueValuePropertyId);
        let password = cached_bool(&element, UIA_IsPasswordPropertyId);
        let sensitive = looks_sensitive(&name, &raw_value, password);
        let bounds = cached_bounds(&element, window_rect);
        let type_name = control_type_name(cached_int(&element, UIA_ControlTypePropertyId));
        let index = elements.len();

        let mut description = json!({
            "index": index,
            "name": utf8(&name),
            "controlType": type_name,
            "automationId": utf8(&automation_id),
            "className": utf8(&class_name),
            "enabled": cached_bool(&element, UIA_IsEnabledPropertyId),
            "offscreen": cached_bool(&element, UIA_IsOffscreenPropertyId),
            "focused": cached_bool(&element, UIA_HasKeyboardFocusPropertyId),
            "bounds": {
                "x": bounds.left,
                "y": bounds.top,
                "width": (bounds.right - bounds.left).max(0),
                "height": (bounds.bottom - bounds.top).max(0),
            },
        });
        if !raw_value.is_empty() || sensitive {
            description["value"] = json!(if sensitive {
                "[redacted]".to_owned()
            } else {
                utf8(&raw_value)
            });
        }
        let patterns: Vec<&str> = [
            (UIA_InvokePatternId, "invoke"),
            (UIA_ValuePatternId, "value"),
            (UIA_SelectionItemPatternId, "select"),
            (UIA_TogglePatternId, "toggle"),
            (UIA_ExpandCollapsePatternId, "expand"),
            (UIA_RangeValuePatternId, "range"),
            (UIA_ScrollPatternId, "scroll"),
            (UIA_ScrollItemPatternId, "scrollItem"),
        ]
        .into_iter()
        .filter(|(pattern, _)| has_cached_pattern(&element, *pattern))
        .map(|(_, label)| label)
        .collect();
        description["patterns"] = json!(patterns);
        let runtime_id = cached_runtime_id(&element);
        if !runtime_id.is_empty() {
            description["runtimeId"] = json!(runtime_id);
        }

        tree.push_str(&format!("[{index}] {type_name}"));
        if !name.is_empty() {
            tree.push_str(&format!(" \"{}\"", utf8(&name)));
        }
        tree.push_str(&format!(" rect={}", rect_text(&bounds)));
        if sensitive {
            tree.push_str(" value=[redacted]");
        } else if !raw_value.is_empty() {
            tree.push_str(&format!(" value=\"{}\"", utf8(&raw_value)));
        }
        tree.push('\n');

        descriptions.push(description.clone());
        elements.push(ElementRecord {
            description,
            element: Mta(element),
        });
    }
    let value = json!({"tree": tree, "elements": descriptions, "elementCount": elements.len()});
    Ok(AccessibilitySnapshot { value, elements })
}

fn find_all_cached(
    automation: &IUIAutomation,
    hwnd: HWND,
) -> windows::core::Result<IUIAutomationElementArray> {
    unsafe {
        let root = automation.ElementFromHandle(hwnd)?;
        let cache = automation.CreateCacheRequest()?;
        for property in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_AutomationIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_IsOffscreenPropertyId,
            UIA_HasKeyboardFocusPropertyId,
            UIA_IsPasswordPropertyId,
            UIA_ValueValuePropertyId,
            UIA_RuntimeIdPropertyId,
        ] {
            cache.AddProperty(property)?;
        }
        for pattern in [
            UIA_InvokePatternId,
            UIA_ValuePatternId,
            UIA_SelectionItemPatternId,
            UIA_TogglePatternId,
            UIA_ExpandCollapsePatternId,
            UIA_RangeValuePatternId,
            UIA_ScrollPatternId,
            UIA_ScrollItemPatternId,
        ] {
            cache.AddPattern(pattern)?;
        }
        let condition = automation.CreateTrueCondition()?;
        root.FindAllBuildCache(TreeScope_Subtree, &condition, &cache)
    }
}

/// The pattern the element supports now, or `Ok(None)`; `Err` carries the
/// structured exception code. A pattern the element does not support still
/// succeeds and yields a null interface, so the pointer decides support, not
/// the return code.
fn guarded_pattern<P: Interface>(
    element: &IUIAutomationElement,
    pattern: UIA_PATTERN_ID,
) -> Result<Option<P>, u32> {
    guarded(|| {
        unsafe { element.GetCurrentPattern(pattern) }
            .ok()
            .and_then(|unknown: IUnknown| unknown.cast::<P>().ok())
    })
}

#[derive(PartialEq, Eq)]
enum PatternOutcome {
    Performed,
    Unavailable,
    Faulted,
}

/// `call` makes one guarded pattern call; its `Err` means the provider refused.
fn call_pattern<P: Interface>(
    record: &ElementRecord,
    pattern_id: UIA_PATTERN_ID,
    lookup_name: &str,
    call_name: &str,
    mut call: impl FnMut(&P) -> windows::core::Result<()>,
) -> PatternOutcome {
    let pattern = match guarded_pattern::<P>(&record.element, pattern_id) {
        Ok(pattern) => pattern,
        Err(fault) => {
            report_pattern_fault(lookup_name, &record.description, fault);
            return PatternOutcome::Faulted;
        }
    };
    let Some(pattern) = pattern else {
        return PatternOutcome::Unavailable;
    };
    match guarded(|| call(&pattern)) {
        Ok(Ok(())) => PatternOutcome::Performed,
        Ok(Err(_)) => PatternOutcome::Unavailable,
        Err(fault) => {
            report_pattern_fault(call_name, &record.description, fault);
            PatternOutcome::Faulted
        }
    }
}

/// A leaf node can neither expand nor collapse, so it reports E_NOTIMPL.
/// Returns whether it expanded.
fn expand_or_collapse(pattern: &IUIAutomationExpandCollapsePattern) -> windows::core::Result<bool> {
    let state = unsafe { pattern.CurrentExpandCollapseState() }?;
    if state == ExpandCollapseState_LeafNode {
        return Err(windows::Win32::Foundation::E_NOTIMPL.into());
    }
    let expand = state != ExpandCollapseState_Expanded;
    unsafe {
        if expand {
            pattern.Expand()
        } else {
            pattern.Collapse()
        }?;
    }
    Ok(expand)
}

/// Static text and plain groups expose the pattern with an empty default
/// action, and DoDefaultAction on them does nothing while reporting success.
fn default_action(pattern: &IUIAutomationLegacyIAccessiblePattern) -> windows::core::Result<()> {
    let has_action =
        unsafe { pattern.CurrentDefaultAction() }.is_ok_and(|action| !action.is_empty());
    if !has_action {
        return Err(windows::Win32::Foundation::E_NOTIMPL.into());
    }
    unsafe { pattern.DoDefaultAction() }
}

/// A caught fault leaves the provider, and possibly this apartment, in an
/// undefined state, so the element is abandoned on the first one instead of
/// being probed with the remaining patterns.
fn perform_element_action(
    record: &ElementRecord,
    context: &Context,
    include_expand_and_default: bool,
) -> Result<Option<&'static str>, Error> {
    context.check()?;
    match call_pattern::<IUIAutomationInvokePattern>(
        record,
        UIA_InvokePatternId,
        "GetCurrentPattern(Invoke)",
        "InvokePattern.Invoke",
        |pattern| unsafe { pattern.Invoke() },
    ) {
        PatternOutcome::Performed => return Ok(Some("invoke")),
        PatternOutcome::Faulted => return Ok(None),
        PatternOutcome::Unavailable => {}
    }

    context.check()?;
    match call_pattern::<IUIAutomationSelectionItemPattern>(
        record,
        UIA_SelectionItemPatternId,
        "GetCurrentPattern(SelectionItem)",
        "SelectionItemPattern.Select",
        |pattern| unsafe { pattern.Select() },
    ) {
        PatternOutcome::Performed => return Ok(Some("select")),
        PatternOutcome::Faulted => return Ok(None),
        PatternOutcome::Unavailable => {}
    }

    context.check()?;
    match call_pattern::<IUIAutomationTogglePattern>(
        record,
        UIA_TogglePatternId,
        "GetCurrentPattern(Toggle)",
        "TogglePattern.Toggle",
        |pattern| unsafe { pattern.Toggle() },
    ) {
        PatternOutcome::Performed => return Ok(Some("toggle")),
        PatternOutcome::Faulted => return Ok(None),
        PatternOutcome::Unavailable if !include_expand_and_default => return Ok(None),
        PatternOutcome::Unavailable => {}
    }

    context.check()?;
    let mut expanded = false;
    match call_pattern::<IUIAutomationExpandCollapsePattern>(
        record,
        UIA_ExpandCollapsePatternId,
        "GetCurrentPattern(ExpandCollapse)",
        "ExpandCollapsePattern.Expand/Collapse",
        |pattern| expand_or_collapse(pattern).map(|expand| expanded = expand),
    ) {
        PatternOutcome::Performed => return Ok(Some(if expanded { "expand" } else { "collapse" })),
        PatternOutcome::Faulted => return Ok(None),
        PatternOutcome::Unavailable => {}
    }

    context.check()?;
    let outcome = call_pattern::<IUIAutomationLegacyIAccessiblePattern>(
        record,
        UIA_LegacyIAccessiblePatternId,
        "GetCurrentPattern(LegacyIAccessible)",
        "LegacyIAccessiblePattern.DoDefaultAction",
        default_action,
    );
    Ok((outcome == PatternOutcome::Performed).then_some("defaultAction"))
}

/// Invoke, select, or toggle without moving the pointer or focus; `None`
/// when the element exposes none of them.
pub(crate) fn invoke(
    record: &ElementRecord,
    context: &Context,
) -> Result<Option<&'static str>, Error> {
    perform_element_action(record, context, false)
}

/// Like [`invoke`], then expand, collapse, or the default action.
pub(crate) fn perform_semantic_action(
    record: &ElementRecord,
    context: &Context,
) -> Result<Option<&'static str>, Error> {
    perform_element_action(record, context, true)
}

/// wcstod in the C locale: leading whitespace, then a decimal or hexadecimal
/// number that must use up the rest of the text.
fn parse_number(text: &str) -> Option<f64> {
    let trimmed = text.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let (negative, unsigned) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let magnitude = match unsigned.get(..2) {
        Some("0x" | "0X") => parse_hexadecimal(&unsigned[2..])?,
        _ if unsigned.starts_with(|first: char| first.is_ascii_digit() || first == '.') => {
            unsigned.parse().ok()?
        }
        _ => return None,
    };
    let value = if negative { -magnitude } else { magnitude };
    value.is_finite().then_some(value)
}

fn parse_hexadecimal(text: &str) -> Option<f64> {
    let (mantissa, exponent) = match text.find(['p', 'P']) {
        Some(position) => (&text[..position], Some(&text[position + 1..])),
        None => (text, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let mut value = 0.0;
    for digit in whole.chars() {
        value = value * 16.0 + f64::from(digit.to_digit(16)?);
    }
    let mut scale = 1.0 / 16.0;
    for digit in fraction.chars() {
        value += f64::from(digit.to_digit(16)?) * scale;
        scale /= 16.0;
    }
    match exponent {
        Some(exponent)
            if exponent
                .trim_start_matches(['+', '-'])
                .bytes()
                .all(|byte| byte.is_ascii_digit()) =>
        {
            Some(value * 2f64.powi(exponent.parse().ok()?))
        }
        Some(_) => None,
        None => Some(value),
    }
}

/// ValuePattern first, then RangeValuePattern for a numeric value.
pub(crate) fn set_value(
    record: &ElementRecord,
    value: &str,
    context: &Context,
) -> Result<bool, Error> {
    context.check()?;
    // SysAllocString stops at the first NUL, so the provider never saw anything after it.
    let text = BSTR::from(value.split('\0').next().unwrap_or_default());
    let outcome = call_pattern::<IUIAutomationValuePattern>(
        record,
        UIA_ValuePatternId,
        "GetCurrentPattern(Value)",
        "ValuePattern.SetValue",
        |pattern| unsafe { pattern.SetValue(&text) },
    );
    match outcome {
        PatternOutcome::Performed => return Ok(true),
        PatternOutcome::Faulted => return Ok(false),
        PatternOutcome::Unavailable => {}
    }

    // Sliders and spinners often pair a read-only ValuePattern with a writable RangeValuePattern.
    let Some(number) = parse_number(value) else {
        return Ok(false);
    };
    context.check()?;
    let outcome = call_pattern::<IUIAutomationRangeValuePattern>(
        record,
        UIA_RangeValuePatternId,
        "GetCurrentPattern(RangeValue)",
        "RangeValuePattern.SetValue",
        |pattern| unsafe { pattern.SetValue(number) },
    );
    Ok(outcome == PatternOutcome::Performed)
}

/// Providers can apply a pattern call after it returns, so the state is read
/// again until it settles or the timeout passes.
fn poll_until(mut settled: impl FnMut() -> bool, context: &Context) -> Result<bool, Error> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    while !settled() {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        context.check()?;
        std::thread::sleep(SETTLE_POLL_INTERVAL);
    }
    Ok(true)
}

/// `None` when the element does not support the pattern.
fn current_pattern<P: Interface>(
    record: &ElementRecord,
    pattern_id: UIA_PATTERN_ID,
    lookup_name: &str,
) -> Result<Option<P>, Error> {
    guarded_pattern::<P>(&record.element, pattern_id).map_err(|fault| {
        report_pattern_fault(lookup_name, &record.description, fault);
        Error::new(
            "pattern_unavailable",
            format!("{lookup_name} faulted in the target application"),
        )
    })
}

fn call_or_fail(
    record: &ElementRecord,
    call_name: &str,
    call: impl FnMut() -> windows::core::Result<()>,
) -> Result<(), Error> {
    match guarded(call) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Error::new(
            "pattern_unavailable",
            format!("{call_name} was refused by the target application"),
        )),
        Err(fault) => {
            report_pattern_fault(call_name, &record.description, fault);
            Err(Error::new(
                "pattern_unavailable",
                format!("{call_name} faulted in the target application"),
            ))
        }
    }
}

fn read_guarded<T>(
    record: &ElementRecord,
    call_name: &str,
    call: impl FnMut() -> windows::core::Result<T>,
) -> Option<T> {
    match guarded(call) {
        Ok(value) => value.ok(),
        Err(fault) => {
            report_pattern_fault(call_name, &record.description, fault);
            None
        }
    }
}

fn read_toggle_state(
    record: &ElementRecord,
    pattern: &IUIAutomationTogglePattern,
) -> Option<ToggleState> {
    read_guarded(record, "TogglePattern.CurrentToggleState", || unsafe {
        pattern.CurrentToggleState()
    })
}

fn read_expand_collapse_state(
    record: &ElementRecord,
    pattern: &IUIAutomationExpandCollapsePattern,
) -> Option<ExpandCollapseState> {
    read_guarded(
        record,
        "ExpandCollapsePattern.CurrentExpandCollapseState",
        || unsafe { pattern.CurrentExpandCollapseState() },
    )
}

fn read_is_selected(
    record: &ElementRecord,
    pattern: &IUIAutomationSelectionItemPattern,
) -> Option<bool> {
    read_guarded(
        record,
        "SelectionItemPattern.CurrentIsSelected",
        || unsafe { pattern.CurrentIsSelected() },
    )
    .map(|selected| selected.0 == 1)
}

fn set_toggle_state(
    record: &ElementRecord,
    checked: bool,
    context: &Context,
) -> Result<Option<StateChange>, Error> {
    let Some(pattern) = current_pattern::<IUIAutomationTogglePattern>(
        record,
        UIA_TogglePatternId,
        "GetCurrentPattern(Toggle)",
    )?
    else {
        return Ok(None);
    };
    let wanted = Some(if checked {
        ToggleState_On
    } else {
        ToggleState_Off
    });
    let mut current = read_toggle_state(record, &pattern);
    if current == wanted {
        return Ok(Some(StateChange {
            pattern: "toggle",
            changed: false,
            verified: true,
        }));
    }

    // A three-state check box cycles off, on, indeterminate, so reaching off
    // from on takes a second toggle.
    const MAXIMUM_TOGGLES: usize = 2;
    for _ in 0..MAXIMUM_TOGGLES {
        context.check()?;
        call_or_fail(record, "TogglePattern.Toggle", || unsafe {
            pattern.Toggle()
        })?;
        let before = current;
        poll_until(
            || {
                current = read_toggle_state(record, &pattern);
                current.is_some() && current != before
            },
            context,
        )?;
        if current == wanted {
            return Ok(Some(StateChange {
                pattern: "toggle",
                changed: true,
                verified: true,
            }));
        }
        if current.is_none() || current == before {
            break;
        }
    }
    Ok(Some(StateChange {
        pattern: "toggle",
        changed: true,
        verified: false,
    }))
}

fn set_expand_collapse_state(
    record: &ElementRecord,
    expanded: bool,
    context: &Context,
) -> Result<Option<StateChange>, Error> {
    let Some(pattern) = current_pattern::<IUIAutomationExpandCollapsePattern>(
        record,
        UIA_ExpandCollapsePatternId,
        "GetCurrentPattern(ExpandCollapse)",
    )?
    else {
        return Ok(None);
    };
    let wanted = if expanded {
        ExpandCollapseState_Expanded
    } else {
        ExpandCollapseState_Collapsed
    };
    let current = read_expand_collapse_state(record, &pattern);
    if current == Some(ExpandCollapseState_LeafNode) {
        return Err(Error::new(
            "pattern_unavailable",
            "The element is a leaf node, which can neither expand nor collapse",
        ));
    }
    if current == Some(wanted) {
        return Ok(Some(StateChange {
            pattern: "expandCollapse",
            changed: false,
            verified: true,
        }));
    }

    let call_name = if expanded {
        "ExpandCollapsePattern.Expand"
    } else {
        "ExpandCollapsePattern.Collapse"
    };
    call_or_fail(record, call_name, || unsafe {
        if expanded {
            pattern.Expand()
        } else {
            pattern.Collapse()
        }
    })?;
    let verified = poll_until(
        || read_expand_collapse_state(record, &pattern) == Some(wanted),
        context,
    )?;
    Ok(Some(StateChange {
        pattern: "expandCollapse",
        changed: true,
        verified,
    }))
}

fn select_item(record: &ElementRecord, context: &Context) -> Result<Option<StateChange>, Error> {
    let Some(pattern) = current_pattern::<IUIAutomationSelectionItemPattern>(
        record,
        UIA_SelectionItemPatternId,
        "GetCurrentPattern(SelectionItem)",
    )?
    else {
        return Ok(None);
    };
    if read_is_selected(record, &pattern) == Some(true) {
        return Ok(Some(StateChange {
            pattern: "select",
            changed: false,
            verified: true,
        }));
    }

    call_or_fail(record, "SelectionItemPattern.Select", || unsafe {
        pattern.Select()
    })?;
    let verified = poll_until(|| read_is_selected(record, &pattern) == Some(true), context)?;
    Ok(Some(StateChange {
        pattern: "select",
        changed: true,
        verified,
    }))
}

/// Puts the element in `state` (checked, unchecked, expanded, collapsed, or
/// selected) and acts only when it is not already there. `None` when the
/// element lacks the pattern for that state.
pub(crate) fn set_state(
    record: &ElementRecord,
    state: &str,
    context: &Context,
) -> Result<Option<StateChange>, Error> {
    context.check()?;
    match state {
        "checked" | "unchecked" => set_toggle_state(record, state == "checked", context),
        "expanded" | "collapsed" => set_expand_collapse_state(record, state == "expanded", context),
        "selected" => select_item(record, context),
        _ => Err(Error::invalid_argument(format!("Unknown state: {state}"))),
    }
}

/// ScrollItemPattern.ScrollIntoView; false when the element lacks the pattern.
pub(crate) fn scroll_into_view(record: &ElementRecord, context: &Context) -> Result<bool, Error> {
    context.check()?;
    let Some(pattern) = current_pattern::<IUIAutomationScrollItemPattern>(
        record,
        UIA_ScrollItemPatternId,
        "GetCurrentPattern(ScrollItem)",
    )?
    else {
        return Ok(false);
    };
    call_or_fail(record, "ScrollItemPattern.ScrollIntoView", || unsafe {
        pattern.ScrollIntoView()
    })?;
    Ok(true)
}

fn control_view_walker(automation: &IUIAutomation) -> Option<IUIAutomationTreeWalker> {
    unsafe { automation.ControlViewWalker() }.ok()
}

fn parent_of(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
) -> Option<IUIAutomationElement> {
    guarded(|| unsafe { walker.GetParentElement(element) })
        .ok()?
        .ok()
}

fn scroll_pattern_of(element: &IUIAutomationElement) -> Option<IUIAutomationScrollPattern> {
    guarded_pattern(element, UIA_ScrollPatternId).ok().flatten()
}

fn screen_bounds(element: &IUIAutomationElement) -> Option<RECT> {
    guarded(|| unsafe { element.CurrentBoundingRectangle() })
        .ok()?
        .ok()
}

fn has_area(rect: &RECT) -> bool {
    rect.right > rect.left && rect.bottom > rect.top
}

/// IntersectRect: the overlap, or `None` when it is empty.
pub(crate) fn intersect(first: &RECT, second: &RECT) -> Option<RECT> {
    let overlap = RECT {
        left: first.left.max(second.left),
        top: first.top.max(second.top),
        right: first.right.min(second.right),
        bottom: first.bottom.min(second.bottom),
    };
    has_area(&overlap).then_some(overlap)
}

/// Window-local bounds the provider reports now, clipped to every scrollable
/// ancestor, or `None` once the element is gone. An observation's bounds go
/// stale when the page scrolls or reflows while the window keeps its
/// geometry, and browsers report unclipped bounds for content scrolled under
/// their toolbar, so the result can be empty.
pub(crate) fn visible_bounds(element: &IUIAutomationElement, window_rect: &RECT) -> Option<RECT> {
    let mut visible = screen_bounds(element)?;
    if let Some(walker) = create_automation().as_ref().and_then(control_view_walker) {
        let mut node = parent_of(&walker, element);
        for _ in 0..MAXIMUM_ANCESTORS {
            let Some(current) = node else { break };
            if scroll_pattern_of(&current).is_some()
                && let Some(viewport) = screen_bounds(&current)
            {
                let Some(clipped) = intersect(&visible, &viewport) else {
                    return Some(RECT::default());
                };
                visible = clipped;
            }
            node = parent_of(&walker, &current);
        }
    }
    Some(RECT {
        left: visible.left - window_rect.left,
        top: visible.top - window_rect.top,
        right: visible.right - window_rect.left,
        bottom: visible.bottom - window_rect.top,
    })
}

fn nearest_scroll_container(
    walker: &IUIAutomationTreeWalker,
    mut node: Option<IUIAutomationElement>,
) -> Option<IUIAutomationScrollPattern> {
    for _ in 0..MAXIMUM_ANCESTORS {
        let current = node?;
        if let Some(pattern) = scroll_pattern_of(&current) {
            return Some(pattern);
        }
        node = parent_of(walker, &current);
    }
    None
}

/// The innermost scrollable container under a screen point. A browser
/// hit-tests to a container that never moves while its content scrolls, so a
/// wheel scroll is observed through this position instead.
pub(crate) fn scroll_container_at(screen_point: POINT) -> Option<IUIAutomationScrollPattern> {
    let automation = create_automation()?;
    let walker = control_view_walker(&automation)?;
    let node = guarded(|| unsafe { automation.ElementFromPoint(screen_point) })
        .ok()?
        .ok()?;
    nearest_scroll_container(&walker, Some(node))
}

/// The element itself or its nearest scrollable ancestor.
pub(crate) fn scroll_container_of(
    element: &IUIAutomationElement,
) -> Option<IUIAutomationScrollPattern> {
    let walker = control_view_walker(&create_automation()?)?;
    nearest_scroll_container(&walker, Some(element.clone()))
}

fn scrollable_axes(pattern: &IUIAutomationScrollPattern) -> Option<(bool, bool)> {
    guarded(|| unsafe {
        let horizontal = pattern.CurrentHorizontallyScrollable()?;
        let vertical = pattern.CurrentVerticallyScrollable()?;
        Ok::<_, windows::core::Error>((horizontal.as_bool(), vertical.as_bool()))
    })
    .ok()?
    .ok()
}

fn scroll_once(
    pattern: &IUIAutomationScrollPattern,
    horizontal: ScrollAmount,
    vertical: ScrollAmount,
) -> bool {
    matches!(
        guarded(|| unsafe { pattern.Scroll(horizontal, vertical) }),
        Ok(Ok(()))
    )
}

pub(crate) fn scroll_position(
    container: Option<&IUIAutomationScrollPattern>,
) -> Option<ScrollPosition> {
    let (horizontal_percent, vertical_percent) = scroll_percents(container?)?;
    Some(ScrollPosition {
        horizontal_percent,
        vertical_percent,
    })
}

fn scroll_percents(pattern: &IUIAutomationScrollPattern) -> Option<(f64, f64)> {
    guarded(|| unsafe {
        let horizontal = pattern.CurrentHorizontalScrollPercent()?;
        let vertical = pattern.CurrentVerticalScrollPercent()?;
        Ok::<_, windows::core::Error>((horizontal, vertical))
    })
    .ok()?
    .ok()
}

/// Scrolls by small increments through ScrollPattern, without the pointer.
/// The increments applied (fewer at the end of the content), or `None` when
/// the container cannot scroll on that axis.
pub(crate) fn scroll(
    container: Option<&IUIAutomationScrollPattern>,
    horizontal: bool,
    forward: bool,
    increments: i32,
    context: &Context,
) -> Result<Option<i32>, Error> {
    let Some(container) = container else {
        return Ok(None);
    };
    let Some((horizontally_scrollable, vertically_scrollable)) = scrollable_axes(container) else {
        return Ok(None);
    };
    if !(if horizontal {
        horizontally_scrollable
    } else {
        vertically_scrollable
    }) {
        return Ok(None);
    }
    let step = if forward {
        ScrollAmount_SmallIncrement
    } else {
        ScrollAmount_SmallDecrement
    };
    let (horizontal_step, vertical_step) = if horizontal {
        (step, ScrollAmount_NoAmount)
    } else {
        (ScrollAmount_NoAmount, step)
    };
    // The provider refuses an increment past the end, which ends the scroll early.
    let mut applied = 0;
    while applied < increments {
        context.check()?;
        if !scroll_once(container, horizontal_step, vertical_step) {
            break;
        }
        applied += 1;
    }
    Ok(Some(applied))
}

/// Providers report bounds unclipped by the viewport, so a target that fits
/// must lie inside it; a larger one only has to overlap it.
fn shown_in(target: &RECT, viewport: &RECT) -> bool {
    if !has_area(target) {
        return false;
    }
    let fits = target.right - target.left <= viewport.right - viewport.left
        && target.bottom - target.top <= viewport.bottom - viewport.top;
    if fits {
        return target.left >= viewport.left
            && target.right <= viewport.right
            && target.top >= viewport.top
            && target.bottom <= viewport.bottom;
    }
    intersect(target, viewport).is_some()
}

struct AxisPosition {
    percent: f64,
    view_size_percent: f64,
}

fn axis_position(pattern: &IUIAutomationScrollPattern, vertical: bool) -> Option<AxisPosition> {
    let (horizontal_percent, vertical_percent) = scroll_percents(pattern)?;
    let (horizontal_view, vertical_view) = guarded(|| unsafe {
        let horizontal = pattern.CurrentHorizontalViewSize()?;
        let vertical = pattern.CurrentVerticalViewSize()?;
        Ok::<_, windows::core::Error>((horizontal, vertical))
    })
    .ok()?
    .ok()?;
    Some(if vertical {
        AxisPosition {
            percent: vertical_percent,
            view_size_percent: vertical_view,
        }
    } else {
        AxisPosition {
            percent: horizontal_percent,
            view_size_percent: horizontal_view,
        }
    })
}

/// The provider can move the content after the call returns. False when the
/// position never left `before`.
fn wait_for_axis_move(
    pattern: &IUIAutomationScrollPattern,
    vertical: bool,
    before: f64,
    context: &Context,
) -> Result<bool, Error> {
    poll_until(
        || axis_position(pattern, vertical).is_none_or(|now| now.percent != before),
        context,
    )
}

fn set_axis_percent(pattern: &IUIAutomationScrollPattern, vertical: bool, percent: f64) -> bool {
    let (horizontal, vertical_percent) = if vertical {
        (UIA_ScrollPatternNoScroll, percent)
    } else {
        (percent, UIA_ScrollPatternNoScroll)
    };
    matches!(
        guarded(|| unsafe { pattern.SetScrollPercent(horizontal, vertical_percent) }),
        Ok(Ok(()))
    )
}

/// The content is viewSize percent of the whole, so the viewport extent maps
/// to the scroll percent through the hidden remainder.
fn center_target(
    pattern: &IUIAutomationScrollPattern,
    target: &RECT,
    viewport: &RECT,
    context: &Context,
) -> Result<bool, Error> {
    let vertical = target.top < viewport.top || target.bottom > viewport.bottom;
    let Some(position) = axis_position(pattern, vertical) else {
        return Ok(false);
    };
    if position.view_size_percent <= 0.0 || position.view_size_percent >= 100.0 {
        return Ok(false);
    }
    let (extent, offset) = if vertical {
        (
            f64::from(viewport.bottom - viewport.top),
            f64::from(target.top + target.bottom - viewport.top - viewport.bottom) / 2.0,
        )
    } else {
        (
            f64::from(viewport.right - viewport.left),
            f64::from(target.left + target.right - viewport.left - viewport.right) / 2.0,
        )
    };
    let hidden_extent = extent * (100.0 - position.view_size_percent) / position.view_size_percent;
    let percent = (position.percent + offset / hidden_extent * 100.0).clamp(0.0, 100.0);
    if percent == position.percent || !set_axis_percent(pattern, vertical, percent) {
        return Ok(false);
    }
    wait_for_axis_move(pattern, vertical, position.percent, context)
}

/// An unrealized item of a virtualized list has no bounds to steer by, so the
/// container is paged from its start until the item shows.
fn page_until_shown(
    pattern: &IUIAutomationScrollPattern,
    shown: impl Fn() -> bool,
    context: &Context,
) -> Result<bool, Error> {
    let Some((horizontally_scrollable, vertically_scrollable)) = scrollable_axes(pattern) else {
        return Ok(false);
    };
    if !horizontally_scrollable && !vertically_scrollable {
        return Ok(false);
    }
    let vertical = vertically_scrollable;

    let Some(start) = axis_position(pattern, vertical) else {
        return Ok(false);
    };
    if start.percent > 0.0 {
        if !set_axis_percent(pattern, vertical, 0.0) {
            return Ok(false);
        }
        wait_for_axis_move(pattern, vertical, start.percent, context)?;
    }
    let (horizontal_step, vertical_step) = if vertical {
        (ScrollAmount_NoAmount, ScrollAmount_LargeIncrement)
    } else {
        (ScrollAmount_LargeIncrement, ScrollAmount_NoAmount)
    };
    const MAXIMUM_PAGES: usize = 200;
    for _ in 0..MAXIMUM_PAGES {
        context.check()?;
        if shown() {
            return Ok(true);
        }
        let Some(before) = axis_position(pattern, vertical) else {
            return Ok(false);
        };
        if before.percent >= 100.0 {
            return Ok(false);
        }
        if !scroll_once(pattern, horizontal_step, vertical_step) {
            return Ok(false);
        }
        if !wait_for_axis_move(pattern, vertical, before.percent, context)? {
            return Ok(shown());
        }
    }
    Ok(shown())
}

/// Scrolls the nearest scrollable ancestor through ScrollPattern until the
/// element shows inside it: centered when it has bounds, paged from the start
/// when it has none yet. Whether it shows, or `None` without a scrollable ancestor.
pub(crate) fn scroll_ancestor_into_view(
    record: &ElementRecord,
    context: &Context,
) -> Result<Option<bool>, Error> {
    context.check()?;
    let Some((ancestor, pattern)) = nearest_scroll_ancestor(&record.element) else {
        return Ok(None);
    };
    let shown = || {
        let target = screen_bounds(&record.element);
        let viewport = screen_bounds(&ancestor);
        matches!((target, viewport), (Some(target), Some(viewport)) if shown_in(&target, &viewport))
    };

    const MAXIMUM_CENTERING_SCROLLS: usize = 10;
    for _ in 0..MAXIMUM_CENTERING_SCROLLS {
        context.check()?;
        if shown() {
            return Ok(Some(true));
        }
        let (Some(target), Some(viewport)) =
            (screen_bounds(&record.element), screen_bounds(&ancestor))
        else {
            break;
        };
        if !has_area(&target) || !center_target(&pattern, &target, &viewport, context)? {
            break;
        }
    }
    page_until_shown(&pattern, shown, context).map(Some)
}

fn nearest_scroll_ancestor(
    element: &IUIAutomationElement,
) -> Option<(IUIAutomationElement, IUIAutomationScrollPattern)> {
    let walker = control_view_walker(&create_automation()?)?;
    let mut node = parent_of(&walker, element);
    for _ in 0..MAXIMUM_ANCESTORS {
        let current = node?;
        if let Some(pattern) = scroll_pattern_of(&current) {
            return Some((current, pattern));
        }
        node = parent_of(&walker, &current);
    }
    None
}

fn current_name(element: &IUIAutomationElement) -> Vec<u16> {
    match guarded(|| unsafe { element.CurrentName() }) {
        Ok(Ok(name)) => name.to_vec(),
        _ => Vec::new(),
    }
}

fn pattern_available_condition(
    automation: &IUIAutomation,
    property: UIA_PROPERTY_ID,
) -> Option<IUIAutomationCondition> {
    unsafe { automation.CreatePropertyCondition(property, &VARIANT::from(true)) }.ok()
}

/// Only items the provider has realized are found; a virtualized list
/// realizes the rest as it scrolls.
fn selectable_items(
    automation: &IUIAutomation,
    container: &IUIAutomationElement,
) -> Vec<IUIAutomationElement> {
    let Some(condition) =
        pattern_available_condition(automation, UIA_IsSelectionItemPatternAvailablePropertyId)
    else {
        return Vec::new();
    };
    let Ok(found) = (unsafe { container.FindAll(TreeScope_Descendants, &condition) }) else {
        return Vec::new();
    };
    let Ok(length) = (unsafe { found.Length() }) else {
        return Vec::new();
    };
    (0..length)
        .filter_map(|position| unsafe { found.GetElement(position) }.ok())
        .collect()
}

fn selectable_items_named(
    automation: &IUIAutomation,
    container: &IUIAutomationElement,
    option: &[u16],
) -> Vec<IUIAutomationElement> {
    let wanted = ascii_lowercase_wide(option);
    selectable_items(automation, container)
        .into_iter()
        .filter(|item| ascii_lowercase_wide(&current_name(item)) == wanted)
        .collect()
}

/// A Win32 combo box seen through the MSAA proxy has no ExpandCollapsePattern
/// and exposes its items only while dropped down; its drop-down button's
/// Invoke opens and closes the list instead.
fn drop_down_button(
    automation: &IUIAutomation,
    combo_box: &IUIAutomationElement,
) -> Option<IUIAutomationInvokePattern> {
    let invokable =
        pattern_available_condition(automation, UIA_IsInvokePatternAvailablePropertyId)?;
    let found = unsafe {
        let is_button = automation
            .CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_ButtonControlTypeId.0),
            )
            .ok()?;
        let condition = automation.CreateAndCondition(&is_button, &invokable).ok()?;
        combo_box.FindFirst(TreeScope_Children, &condition).ok()?
    };
    guarded_pattern(&found, UIA_InvokePatternId).ok().flatten()
}

fn invoke_succeeded(pattern: &IUIAutomationInvokePattern) -> bool {
    matches!(guarded(|| unsafe { pattern.Invoke() }), Ok(Ok(())))
}

/// A combo box scrolls through a list child, not through itself.
fn scroll_pattern_within(
    automation: &IUIAutomation,
    container: &IUIAutomationElement,
) -> Option<IUIAutomationScrollPattern> {
    if let Some(own) = scroll_pattern_of(container) {
        return Some(own);
    }
    let condition =
        pattern_available_condition(automation, UIA_IsScrollPatternAvailablePropertyId)?;
    let list = unsafe { container.FindFirst(TreeScope_Descendants, &condition) }.ok()?;
    scroll_pattern_of(&list)
}

/// Selects the one SelectionItem descendant named `option` (ASCII
/// case-insensitive): a collapsed container is expanded first and collapsed
/// again afterwards, and a list is paged until the item is realized.
pub(crate) fn select_option(
    record: &ElementRecord,
    option: &str,
    context: &Context,
) -> Result<StateChange, Error> {
    context.check()?;
    let automation = create_automation()
        .ok_or_else(|| Error::new("uia_unavailable", "UI Automation is unavailable"))?;
    let option_wide: Vec<u16> = option.encode_utf16().collect();

    let matches = RefCell::new(Vec::new());
    let search = || {
        *matches.borrow_mut() = selectable_items_named(&automation, &record.element, &option_wide);
        !matches.borrow().is_empty()
    };
    let list_shown = || !selectable_items(&automation, &record.element).is_empty();

    let expander = current_pattern::<IUIAutomationExpandCollapsePattern>(
        record,
        UIA_ExpandCollapsePatternId,
        "GetCurrentPattern(ExpandCollapse)",
    )?;
    let is_combo_box = description_field(&record.description, "controlType") == "ComboBox";
    let drop_down = if expander.is_none() && is_combo_box {
        drop_down_button(&automation, &record.element)
    } else {
        None
    };
    let mut expanded_here = false;
    if let Some(expander) = &expander
        && read_expand_collapse_state(record, expander) == Some(ExpandCollapseState_Collapsed)
    {
        expanded_here = matches!(guarded(|| unsafe { expander.Expand() }), Ok(Ok(())));
        if expanded_here {
            poll_until(
                || {
                    read_expand_collapse_state(record, expander)
                        == Some(ExpandCollapseState_Expanded)
                },
                context,
            )?;
        }
    } else if let Some(drop_down) = &drop_down
        && !search()
    {
        expanded_here = invoke_succeeded(drop_down);
        if expanded_here {
            poll_until(search, context)?;
        }
    }
    let restore = || {
        if !expanded_here {
            return;
        }
        if let Some(expander) = &expander {
            let _ = guarded(|| unsafe { expander.Collapse() });
        } else if let Some(drop_down) = &drop_down
            // The list may already have closed on selection, and a second Invoke would reopen it.
            && list_shown()
        {
            invoke_succeeded(drop_down);
        }
    };

    let selected = (|| {
        if !search()
            && let Some(list) = scroll_pattern_within(&automation, &record.element)
        {
            page_until_shown(&list, search, context)?;
        }
        let matches = matches.take();
        if matches.len() > 1 {
            return Err(Error::new(
                "ambiguous_target",
                format!(
                    "{} selectable items are named \"{option}\"; select one with set-state instead",
                    matches.len()
                ),
            ));
        }
        let Some(first) = matches.into_iter().next() else {
            return Err(Error::new(
                "option_not_found",
                format!("No selectable item named \"{option}\" under the element"),
            ));
        };
        let item = ElementRecord {
            description: json!({"name": option}),
            element: Mta(first),
        };
        select_item(&item, context)?.ok_or_else(|| {
            Error::new(
                "pattern_unavailable",
                "The matching item has no SelectionItemPattern",
            )
        })
    })();
    restore();
    selected
}

fn monotonic_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Nanoseconds after `monotonic_epoch` plus one, or 0 before any change.
type ChangeClock = Arc<AtomicU64>;

#[implement(
    IUIAutomationEventHandler,
    IUIAutomationStructureChangedEventHandler,
    IUIAutomationPropertyChangedEventHandler
)]
struct ChangeHandler {
    last_change: ChangeClock,
}

impl ChangeHandler {
    fn record(&self) {
        let elapsed = monotonic_epoch().elapsed().as_nanos() as u64;
        self.last_change.store(elapsed + 1, Ordering::Release);
    }
}

impl IUIAutomationEventHandler_Impl for ChangeHandler_Impl {
    fn HandleAutomationEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        self.record();
        Ok(())
    }
}

impl IUIAutomationStructureChangedEventHandler_Impl for ChangeHandler_Impl {
    fn HandleStructureChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: StructureChangeType,
        _: *const SAFEARRAY,
    ) -> windows::core::Result<()> {
        self.record();
        Ok(())
    }
}

impl IUIAutomationPropertyChangedEventHandler_Impl for ChangeHandler_Impl {
    fn HandlePropertyChangedEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        _: UIA_PROPERTY_ID,
        _: &VARIANT,
    ) -> windows::core::Result<()> {
        self.record();
        Ok(())
    }
}

/// Records when a window's provider reports that it applied a change: layout
/// invalidation, structure changes, or a scroll position change. Browsers
/// publish moved locations up to about half a second after the input that
/// moved them, so these events are what tells a pending update from no
/// change at all.
pub(crate) struct UiaChangeWatcher {
    registration: Option<Mta<IUIAutomation>>,
    last_change: ChangeClock,
}

impl UiaChangeWatcher {
    pub(crate) fn new(hwnd: HWND) -> Self {
        let last_change = ChangeClock::default();
        let registration = create_automation()
            .filter(|automation| register_change_handlers(automation, hwnd, &last_change));
        Self {
            registration: registration.map(Mta),
            last_change,
        }
    }

    pub(crate) fn watching(&self) -> bool {
        self.registration.is_some()
    }

    pub(crate) fn last_change(&self) -> Option<Instant> {
        self.registration.as_ref()?;
        match self.last_change.load(Ordering::Acquire) {
            0 => None,
            ticks => Some(monotonic_epoch() + Duration::from_nanos(ticks - 1)),
        }
    }
}

fn register_change_handlers(
    automation: &IUIAutomation,
    hwnd: HWND,
    last_change: &ChangeClock,
) -> bool {
    unsafe {
        let Ok(root) = automation.ElementFromHandle(hwnd) else {
            return false;
        };
        // Only the arrival time matters, so no element data is fetched across
        // processes for each event; a burst would otherwise delay its delivery.
        let Ok(nothing) = automation.CreateCacheRequest() else {
            return false;
        };
        if nothing
            .SetAutomationElementMode(AutomationElementMode_None)
            .is_err()
        {
            return false;
        }
        let handler: IUIAutomationEventHandler = ChangeHandler {
            last_change: last_change.clone(),
        }
        .into();
        let registered = (|| {
            automation.AddAutomationEventHandler(
                UIA_LayoutInvalidatedEventId,
                &root,
                TreeScope_Subtree,
                &nothing,
                &handler,
            )?;
            let structure: IUIAutomationStructureChangedEventHandler = handler.cast()?;
            automation.AddStructureChangedEventHandler(
                &root,
                TreeScope_Subtree,
                &nothing,
                &structure,
            )?;
            let property: IUIAutomationPropertyChangedEventHandler = handler.cast()?;
            automation.AddPropertyChangedEventHandlerNativeArray(
                &root,
                TreeScope_Subtree,
                &nothing,
                &property,
                &[
                    UIA_ScrollHorizontalScrollPercentPropertyId,
                    UIA_ScrollVerticalScrollPercentPropertyId,
                ],
            )
        })();
        if registered.is_err() {
            let _ = automation.RemoveAllEventHandlers();
            return false;
        }
        true
    }
}

impl Drop for UiaChangeWatcher {
    fn drop(&mut self) {
        if let Some(automation) = &self.registration {
            unsafe {
                let _ = automation.RemoveAllEventHandlers();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "system" fn read_unmapped_address() -> i32 {
        unsafe { std::ptr::read_volatile(8 as *const i32) }
    }

    #[test]
    fn faulting_foreign_call_reports_its_exception_code_instead_of_crashing() {
        let faulting_call: unsafe extern "system" fn() -> i32 = read_unmapped_address;

        let outcome = guarded(|| unsafe { faulting_call() });

        assert_eq!(outcome, Err(0xC000_0005));
    }

    #[test]
    fn guarded_call_without_fault_returns_its_value() {
        assert_eq!(guarded(|| 7), Ok(7));
    }

    #[test]
    fn numbers_parse_like_wcstod_in_the_c_locale() {
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("  -1.5e2"), Some(-150.0));
        assert_eq!(parse_number("+.5"), Some(0.5));
        assert_eq!(parse_number("0x1A"), Some(26.0));
        assert_eq!(parse_number("0x1.8p1"), Some(3.0));
        assert_eq!(parse_number("12 "), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("nan"), None);
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("1e400"), None);
    }

    #[test]
    fn names_and_values_mentioning_secrets_are_sensitive() {
        let wide = |text: &str| text.encode_utf16().collect::<Vec<u16>>();

        assert!(looks_sensitive(&wide("API Token"), &wide(""), false));
        assert!(looks_sensitive(&wide("Field"), &wide("my PassCode"), false));
        assert!(looks_sensitive(&wide("Field"), &wide(""), true));
        assert!(!looks_sensitive(&wide("Search"), &wide("weather"), false));
    }

    #[test]
    fn text_with_a_lone_surrogate_converts_to_empty_like_the_cpp_daemon() {
        assert_eq!(utf8(&[u16::from(b'a'), 0xD800]), "");
        assert_eq!(utf8(&[u16::from(b'a'), u16::from(b'b')]), "ab");
    }

    #[test]
    fn a_target_that_fits_must_lie_inside_the_viewport_to_show() {
        let viewport = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };

        assert!(shown_in(
            &RECT {
                left: 10,
                top: 10,
                right: 50,
                bottom: 50
            },
            &viewport
        ));
        assert!(!shown_in(
            &RECT {
                left: 80,
                top: 10,
                right: 120,
                bottom: 50
            },
            &viewport
        ));
        assert!(shown_in(
            &RECT {
                left: -50,
                top: 10,
                right: 150,
                bottom: 50
            },
            &viewport
        ));
        assert!(!shown_in(
            &RECT {
                left: 10,
                top: 10,
                right: 10,
                bottom: 50
            },
            &viewport
        ));
    }
}

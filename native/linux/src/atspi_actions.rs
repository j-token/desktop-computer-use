//! Accessibility snapshots and semantic actions over AT-SPI.

use std::ffi::CStr;

use dcu_core::{Context, Error, Json};
use serde_json::json;
use zbus::Connection;

use crate::atspi::*;
use crate::backend::{WindowInfo, wait_checked};

pub const MAX_TREE_NODES: usize = 1200;
pub const MAX_TREE_DEPTH: usize = 64;
const MAX_TEXT: i32 = 500;
const SETTLE_MILLISECONDS: i32 = 600;
const SETTLE_POLL_MILLISECONDS: i32 = 25;
const MAX_SCROLL_ANCESTORS: usize = 32;
const MAX_SCROLL_STEPS: usize = 200;

pub struct StateChange {
    pub action: String,
    pub changed: bool,
    pub verified: bool,
}

impl StateChange {
    fn already_reached() -> Self {
        Self {
            action: String::new(),
            changed: false,
            verified: true,
        }
    }
}

pub fn rect(node: &Accessible) -> Option<[i32; 4]> {
    node.extents()
        .filter(|bounds| bounds[2] > 0 && bounds[3] > 0)
}

/// GTK reports a row it has not drawn yet at INT_MIN, which is no position at
/// all, so such a rectangle counts as missing.
pub fn drawn_rect(node: &Accessible) -> Option<[i32; 4]> {
    const MAX_COORDINATE: i32 = 1 << 20;
    let plausible = |coordinate: i32| (-MAX_COORDINATE..=MAX_COORDINATE).contains(&coordinate);
    rect(node).filter(|bounds| plausible(bounds[0]) && plausible(bounds[1]))
}

/// The top-level accessible of `target`: a child of its application (found
/// by process id or name) with the window's geometry or title.
pub fn find_window(connection: &Connection, target: &WindowInfo) -> Option<Accessible> {
    let desktop = Accessible::desktop(connection);
    for app_index in 0..desktop.child_count() {
        let Some(app) = desktop.child_at(app_index) else {
            continue;
        };
        if target.pid > 0
            && app.process_id() != target.pid as u32
            && !app.name().eq_ignore_ascii_case(&target.app)
        {
            continue;
        }
        for child_index in 0..app.child_count() {
            let Some(child) = app.child_at(child_index) else {
                continue;
            };
            let bounds = rect(&child);
            let title = child.name();
            if bounds == Some([target.x, target.y, target.width, target.height]) {
                return Some(child);
            }
            if !title.is_empty() && title == target.title && bounds.is_some() {
                return Some(child);
            }
        }
    }
    None
}

fn require_window(connection: &Connection, window: &WindowInfo) -> Result<Accessible, Error> {
    find_window(connection, window).ok_or_else(|| {
        Error::new(
            "accessibility_unavailable",
            "The target window is not exposed on the AT-SPI bus",
        )
    })
}

fn is_sensitive(node: &Accessible) -> bool {
    let description = format!(
        "{} {} {}",
        node.role_name(),
        node.name(),
        node.accessible_id()
    )
    .to_ascii_lowercase();
    [
        "password",
        "passcode",
        "secret",
        "one-time code",
        "verification code",
    ]
    .iter()
    .any(|token| description.contains(token))
}

/// `std::ostream << double`: printf's %g.
fn format_number(value: f64) -> String {
    let mut buffer = [0u8; 64];
    // SAFETY: snprintf writes at most buffer.len() bytes, NUL included.
    unsafe {
        libc::snprintf(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%g".as_ptr(),
            value,
        )
    };
    CStr::from_bytes_until_nul(&buffer)
        .map(|text| text.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn value_text(node: &Accessible) -> String {
    if is_sensitive(node) {
        return "[redacted]".to_owned();
    }
    if node.has_text() {
        let count = node.character_count();
        if count > 0 {
            let mut text = node.text(0, count.min(MAX_TEXT));
            if count > MAX_TEXT {
                text.push_str("...");
            }
            return text;
        }
    }
    if node.has_value() {
        return format_number(node.current_value());
    }
    String::new()
}

fn action_names(node: &Accessible) -> Vec<String> {
    if !node.has_action() {
        return Vec::new();
    }
    (0..node.action_count())
        .filter_map(|index| node.action_name(index))
        .filter(|name| !name.is_empty())
        .collect()
}

pub fn snapshot(
    connection: &Connection,
    window: &WindowInfo,
    context: &Context,
) -> Result<Json, Error> {
    context.check()?;
    let root = require_window(connection, window)?;
    let mut snapshot = Snapshot {
        window,
        context,
        elements: Vec::new(),
        tree: String::new(),
        count: 0,
    };
    snapshot.visit(&root, 0, "0")?;
    Ok(json!({
        "tree": snapshot.tree,
        "elements": snapshot.elements,
        "truncated": snapshot.count >= MAX_TREE_NODES,
        "maxNodes": MAX_TREE_NODES,
        "maxDepth": MAX_TREE_DEPTH,
    }))
}

struct Snapshot<'a> {
    window: &'a WindowInfo,
    context: &'a Context,
    elements: Vec<Json>,
    tree: String,
    count: usize,
}

impl Snapshot<'_> {
    fn visit(&mut self, node: &Accessible, depth: usize, path: &str) -> Result<(), Error> {
        if self.count >= MAX_TREE_NODES || depth > MAX_TREE_DEPTH {
            return Ok(());
        }
        self.context.check()?;
        let index = self.count;
        self.count += 1;
        let name = node.name();
        let role = node.role_name();
        let value = value_text(node);
        let frame = drawn_rect(node).map(|[x, y, width, height]| {
            json!({"x": x - self.window.x, "y": y - self.window.y, "width": width, "height": height})
        });
        self.elements.push(json!({
            "index": index,
            "runtimeId": path,
            "automationId": node.accessible_id(),
            "name": name,
            "controlType": role,
            "localizedControlType": role,
            "className": "",
            "value": value,
            "isSelected": node.has_state(STATE_SELECTED),
            "actions": action_names(node),
            "frame": frame,
        }));
        self.tree
            .push_str(&format!("{}{index}: {role}", " ".repeat(depth * 2)));
        if !name.is_empty() {
            self.tree.push_str(&format!(" {name}"));
        }
        if !value.is_empty() && value != name {
            self.tree.push_str(&format!(" — {value}"));
        }
        self.tree.push('\n');
        let children = node.child_count();
        let mut child = 0;
        while child < children && self.count < MAX_TREE_NODES {
            if let Some(child_node) = node.child_at(child) {
                self.visit(&child_node, depth + 1, &format!("{path}/{child}"))?;
            }
            child += 1;
        }
        Ok(())
    }
}

/// The element at `requested`, numbered in the same pre-order as the
/// snapshot's indexes.
pub fn element_for_index(
    connection: &Connection,
    window: &WindowInfo,
    requested: usize,
    context: &Context,
) -> Result<Accessible, Error> {
    fn visit(
        node: &Accessible,
        depth: usize,
        requested: usize,
        count: &mut usize,
        context: &Context,
    ) -> Result<Option<Accessible>, Error> {
        if *count >= MAX_TREE_NODES || depth > MAX_TREE_DEPTH {
            return Ok(None);
        }
        context.check()?;
        let index = *count;
        *count += 1;
        if index == requested {
            return Ok(Some(node.clone()));
        }
        for child in 0..node.child_count() {
            let Some(child_node) = node.child_at(child) else {
                continue;
            };
            if let Some(found) = visit(&child_node, depth + 1, requested, count, context)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    let root = require_window(connection, window)?;
    visit(&root, 0, requested, &mut 0, context)?.ok_or_else(|| {
        Error::new(
            "element_not_found",
            "elementIndex is not present in the current accessibility tree",
        )
    })
}

/// Runs the first action whose lowercased name is in `preferred` (or, with
/// `fall_back_to_first`, the toolkit's default first action) and returns its
/// name; None when there is no such action or the toolkit refuses it.
fn do_preferred_action(
    element: &Accessible,
    preferred: &[&str],
    fall_back_to_first: bool,
) -> Option<String> {
    if !element.has_action() {
        return None;
    }
    let count = element.action_count();
    let names: Vec<String> = (0..count)
        .map(|index| {
            element
                .action_name(index)
                .unwrap_or_default()
                .to_ascii_lowercase()
        })
        .collect();
    let default_action = (fall_back_to_first && count > 0).then_some(0);
    let chosen = preferred
        .iter()
        .find_map(|wanted| names.iter().position(|name| name == wanted))
        .or(default_action)?;
    if !element.do_action(chosen as i32) {
        return None;
    }
    let name = &names[chosen];
    Some(if name.is_empty() {
        format!("action{chosen}")
    } else {
        name.clone()
    })
}

/// Toolkits name the same intent differently (GTK "click", Qt "Press",
/// Firefox "jump" for links); the first action is the toolkit's default.
pub fn do_primary_action(element: &Accessible) -> Option<String> {
    do_preferred_action(
        element,
        &[
            "click",
            "press",
            "activate",
            "toggle",
            "jump",
            "open",
            "expand or contract",
        ],
        true,
    )
}

fn wait_until(settled: impl Fn() -> bool, context: &Context) -> Result<bool, Error> {
    let mut waited = 0;
    while !settled() {
        if waited >= SETTLE_MILLISECONDS {
            return Ok(false);
        }
        wait_checked(context, SETTLE_POLL_MILLISECONDS)?;
        waited += SETTLE_POLL_MILLISECONDS;
    }
    Ok(true)
}

fn is_checkable(node: &Accessible) -> bool {
    node.has_state(STATE_CHECKABLE)
        || matches!(
            node.role(),
            ROLE_CHECK_BOX
                | ROLE_TOGGLE_BUTTON
                | ROLE_RADIO_BUTTON
                | ROLE_CHECK_MENU_ITEM
                | ROLE_RADIO_MENU_ITEM
        )
}

/// Changes one state flag through actions. A tri-state check box can need two
/// toggles, but a second toggle only follows a toggle that visibly changed
/// something, so a slow provider is not toggled back.
fn set_flag(
    element: &Accessible,
    flag: u32,
    wanted: bool,
    actions: &[&str],
    context: &Context,
) -> Result<Option<StateChange>, Error> {
    let reached = || element.has_state(flag) == wanted;
    if reached() {
        return Ok(Some(StateChange::already_reached()));
    }
    let signature = || {
        (
            element.has_state(flag),
            element.has_state(STATE_INDETERMINATE),
        )
    };
    let initial = signature();
    let mut performed = None;
    let mut toggles = 0;
    while toggles < 2 && !reached() {
        let before = signature();
        let Some(action) = do_preferred_action(element, actions, false) else {
            break;
        };
        performed = Some(action);
        if !wait_until(|| signature() != before, context)? {
            break;
        }
        toggles += 1;
    }
    Ok(performed.map(|action| StateChange {
        action,
        changed: signature() != initial,
        verified: reached(),
    }))
}

fn select_in_parent(parent: Option<&Accessible>, child: &Accessible) -> bool {
    let Some(parent) = parent.filter(|parent| parent.has_selection()) else {
        return false;
    };
    let index = child.index_in_parent();
    index >= 0 && parent.select_child(index)
}

fn select(element: &Accessible) -> Option<String> {
    if select_in_parent(element.parent().as_ref(), element) {
        return Some("selection".to_owned());
    }
    do_preferred_action(element, &["select", "click", "press", "activate"], false)
}

fn set_selected(element: &Accessible, context: &Context) -> Result<Option<StateChange>, Error> {
    let selected = || element.has_state(STATE_SELECTED);
    if selected() {
        return Ok(Some(StateChange::already_reached()));
    }
    let Some(action) = select(element) else {
        return Ok(None);
    };
    let reached = wait_until(selected, context)?;
    Ok(Some(StateChange {
        action,
        changed: reached,
        verified: reached,
    }))
}

/// None when the element exposes no state and action for `state`.
pub fn set_state(
    element: &Accessible,
    state: &str,
    context: &Context,
) -> Result<Option<StateChange>, Error> {
    if state == "checked" || state == "unchecked" {
        if !is_checkable(element) {
            return Ok(None);
        }
        return set_flag(
            element,
            STATE_CHECKED,
            state == "checked",
            &["toggle", "click", "press", "activate"],
            context,
        );
    }
    if state == "expanded" || state == "collapsed" {
        if !element.has_state(STATE_EXPANDABLE) {
            return Ok(None);
        }
        let expand = state == "expanded";
        let first = if expand { "expand" } else { "collapse" };
        let actions = [
            first,
            "expand or contract",
            "toggle",
            "activate",
            "press",
            "click",
        ];
        return set_flag(element, STATE_EXPANDED, expand, &actions, context);
    }
    if !element.has_state(STATE_SELECTABLE) {
        return Ok(None);
    }
    set_selected(element, context)
}

fn rects_intersect(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

struct ScrollBar {
    bar: Accessible,
    vertical: bool,
}

struct ScrollArea {
    viewport: Accessible,
    bars: Vec<ScrollBar>,
}

fn is_vertical_bar(bar: &Accessible) -> bool {
    if bar.has_state(STATE_VERTICAL) {
        return true;
    }
    if bar.has_state(STATE_HORIZONTAL) {
        return false;
    }
    rect(bar).is_some_and(|bounds| bounds[3] > bounds[2])
}

/// The nearest ancestor with scroll bars as direct children, which is how GTK
/// and Qt scrolled areas expose their scrolling.
fn scroll_ancestor(element: &Accessible) -> Option<ScrollArea> {
    let mut ancestor = element.parent();
    for _ in 0..MAX_SCROLL_ANCESTORS {
        let node = ancestor?;
        let bars: Vec<ScrollBar> = (0..node.child_count())
            .filter_map(|index| node.child_at(index))
            .filter(|child| child.role() == ROLE_SCROLL_BAR && child.has_value())
            .map(|bar| ScrollBar {
                vertical: is_vertical_bar(&bar),
                bar,
            })
            .collect();
        if !bars.is_empty() {
            return Some(ScrollArea {
                viewport: node,
                bars,
            });
        }
        ancestor = node.parent();
    }
    None
}

/// Moves the scroll bars toward the target until it is shown. With known
/// bounds each bar centers the target, assuming one value unit per pixel as in
/// GTK adjustments; without bounds the vertical bar pages from the top. It
/// stops when no bar moves any more.
fn scroll_with_bars(
    element: &Accessible,
    area: &ScrollArea,
    shown: &impl Fn() -> bool,
    context: &Context,
) -> Result<bool, Error> {
    let mut paging = false;
    for _ in 0..MAX_SCROLL_STEPS {
        if shown() {
            return Ok(true);
        }
        let Some(view) = rect(&area.viewport) else {
            return Ok(false);
        };
        let target = drawn_rect(element);
        let starts_paging = target.is_none() && !paging;
        let mut moved = false;
        for ScrollBar { bar, vertical } in &area.bars {
            if target.is_none() && !vertical {
                continue;
            }
            let current = bar.current_value();
            let minimum = bar.minimum_value();
            let maximum = bar.maximum_value();
            // A provider that reports no range clamps the value itself.
            let upper = if maximum > minimum { maximum } else { f64::MAX };
            let next = match target {
                Some(target) => {
                    let axis = usize::from(*vertical);
                    let target_center = f64::from(target[axis]) + f64::from(target[axis + 2]) / 2.0;
                    let view_center = f64::from(view[axis]) + f64::from(view[axis + 2]) / 2.0;
                    current + (target_center - view_center)
                }
                None if starts_paging => minimum,
                None => current + f64::from(view[3]),
            };
            let next = next.max(minimum).min(upper);
            if (next - current).abs() < 1.0 {
                continue;
            }
            bar.set_current_value(next);
            moved = (bar.current_value() - current).abs() >= 1.0 || moved;
        }
        if target.is_none() {
            paging = true;
        }
        if !moved && !starts_paging {
            return Ok(shown());
        }
        wait_checked(context, SETTLE_POLL_MILLISECONDS * 2)?;
    }
    Ok(shown())
}

/// Brings the element into view and returns the action used (empty when it
/// was already shown) and whether it is shown now.
pub fn scroll_to(
    element: &Accessible,
    window_bounds: [i32; 4],
    context: &Context,
) -> Result<(String, bool), Error> {
    let area = scroll_ancestor(element);
    // GTK keeps a scrolled-out row SHOWING with its real extents, so the row
    // must also meet the scroll area, not only the window.
    let shown = || {
        let Some(bounds) = drawn_rect(element) else {
            return false;
        };
        if !element.has_state(STATE_SHOWING) || !rects_intersect(bounds, window_bounds) {
            return false;
        }
        area.as_ref()
            .and_then(|area| rect(&area.viewport))
            .is_none_or(|view| rects_intersect(bounds, view))
    };

    let mut action = String::new();
    let mut visible = shown();
    if !visible && element.has_component() && element.scroll_to(SCROLL_ANYWHERE) {
        action = "scrollTo".to_owned();
        visible = wait_until(shown, context)?;
    }
    // Component.scroll_to is optional for a toolkit and can report success
    // without moving anything, so the enclosing scroll bars are the fallback.
    if !visible {
        if area.is_none() && action.is_empty() {
            return Err(Error::new(
                "pattern_unavailable",
                "The target element supports no AT-SPI scroll_to and has no ancestor with scroll bars",
            ));
        }
        if let Some(area) = &area {
            action = "scrollBar".to_owned();
            visible =
                scroll_with_bars(element, area, &shown, context)? || wait_until(shown, context)?;
        }
    }
    Ok((action, visible))
}

fn is_option(node: &Accessible) -> bool {
    node.has_state(STATE_SELECTABLE)
        || matches!(
            node.role(),
            ROLE_LIST_ITEM
                | ROLE_MENU_ITEM
                | ROLE_TREE_ITEM
                | ROLE_PAGE_TAB
                | ROLE_TABLE_CELL
                | ROLE_RADIO_MENU_ITEM
                | ROLE_CHECK_MENU_ITEM
        )
}

/// Descendants of `container` that look like options named `option`,
/// compared case-insensitively.
fn options_named(
    container: &Accessible,
    option: &str,
    context: &Context,
) -> Result<Vec<Accessible>, Error> {
    let mut search = OptionSearch {
        container,
        wanted: option.to_ascii_lowercase(),
        context,
        visited: 0,
        matches: Vec::new(),
    };
    search.visit(container, 0)?;
    Ok(search.matches)
}

struct OptionSearch<'a> {
    container: &'a Accessible,
    wanted: String,
    context: &'a Context,
    visited: usize,
    matches: Vec<Accessible>,
}

impl OptionSearch<'_> {
    fn visit(&mut self, node: &Accessible, depth: usize) -> Result<(), Error> {
        let visited = self.visited;
        self.visited += 1;
        if visited >= MAX_TREE_NODES || depth > MAX_TREE_DEPTH {
            return Ok(());
        }
        self.context.check()?;
        if !node.same_object(self.container)
            && node.name().to_ascii_lowercase() == self.wanted
            && is_option(node)
        {
            self.matches.push(node.clone());
        }
        for index in 0..node.child_count() {
            let Some(child) = node.child_at(index) else {
                continue;
            };
            self.visit(&child, depth + 1)?;
        }
        Ok(())
    }
}

/// A GTK combo box implements Selection over its popup's items, so its
/// selection is tried before the item's own parent.
fn select_option_item(container: &Accessible, item: &Accessible) -> Option<String> {
    if container.role() == ROLE_COMBO_BOX && select_in_parent(Some(container), item) {
        return Some("selection".to_owned());
    }
    select(item)
}

pub fn select_named(
    container: &Accessible,
    option: &str,
    context: &Context,
) -> Result<StateChange, Error> {
    let matches = options_named(container, option, context)?;
    if matches.is_empty() {
        return Err(Error::new(
            "option_not_found",
            format!("No selectable item named \"{option}\" is inside the element"),
        ));
    }
    if matches.len() > 1 {
        return Err(Error::new(
            "ambiguous_target",
            format!(
                "{} items are named \"{option}\"; use set-state selected on one of them",
                matches.len()
            ),
        ));
    }
    let item = &matches[0];
    // A combo box names itself after its active item instead of marking it selected.
    let chosen = || item.has_state(STATE_SELECTED) || container.name().eq_ignore_ascii_case(option);
    if chosen() {
        return Ok(StateChange::already_reached());
    }
    let action = select_option_item(container, item).ok_or_else(|| {
        Error::new(
            "pattern_unavailable",
            format!(
                "The item named \"{option}\" exposes no AT-SPI Selection parent or select action"
            ),
        )
    })?;
    let reached = wait_until(chosen, context)?;
    Ok(StateChange {
        action,
        changed: reached,
        verified: reached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_an_ostream() {
        assert_eq!(format_number(50.0), "50");
        assert_eq!(format_number(0.25), "0.25");
        assert_eq!(format_number(1234567.0), "1.23457e+06");
    }

    #[test]
    fn touching_rectangles_do_not_intersect() {
        assert!(!rects_intersect([0, 0, 10, 10], [10, 0, 5, 5]));
        assert!(rects_intersect([0, 0, 10, 10], [9, 9, 5, 5]));
    }
}

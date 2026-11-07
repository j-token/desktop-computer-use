use crate::backend::{Error, Json};

const SEMANTIC_ACTIONS: &[&str] = &["set-state", "scroll-to", "select-option"];
const INPUT_ACTIONS: &[&str] = &[
    "click",
    "drag",
    "scroll",
    "type-text",
    "press-key",
    "hotkey",
    "set-value",
    "paste-text",
    "set-state",
    "scroll-to",
    "select-option",
];
const SUPPORTED_METHODS: &[&str] = &[
    "doctor",
    "capabilities",
    "session.start",
    "session.status",
    "session.stop",
    "daemon.shutdown",
    "list-apps",
    "list-windows",
    "get-app-state",
    "get-full-screenshot",
    "click",
    "drag",
    "scroll",
    "type-text",
    "press-key",
    "hotkey",
    "set-value",
    "paste-text",
    "set-state",
    "scroll-to",
    "select-option",
    "toggle.set",
    "toggle.release-all",
    "toggle.status",
];
const CLICK_MODIFIERS: &[&str] = &[
    "shift", "ctrl", "control", "alt", "option", "win", "meta", "super",
];
const MAXIMUM_STRING_BYTES: usize = 1_048_576;

fn has(params: &Json, key: &str) -> bool {
    params.get(key).is_some()
}

fn validate_modifiers(params: &Json) -> Result<(), Error> {
    let Some(value) = params.get("modifiers") else {
        return Ok(());
    };
    let Some(value) = value.as_str() else {
        return Err(Error::invalid_argument(
            "modifiers must be a string such as shift+ctrl",
        ));
    };
    for part in value.split('+') {
        let part = part.to_ascii_lowercase();
        if !CLICK_MODIFIERS.contains(&part.as_str()) {
            return Err(Error::invalid_argument(format!(
                "Unknown modifier: {part} (use shift, ctrl, alt, or win joined by +)"
            )));
        }
    }
    Ok(())
}

fn validate_number(
    params: &Json,
    key: &str,
    minimum: f64,
    maximum: f64,
    requires_integer: bool,
) -> Result<(), Error> {
    let Some(value) = params.get(key) else {
        return Ok(());
    };
    let Some(value) = value.as_f64() else {
        return Err(Error::invalid_argument(format!("{key} must be numeric")));
    };
    let outside_range = !value.is_finite() || value < minimum || value > maximum;
    let invalid_integer = requires_integer && value != value.floor();
    if outside_range || invalid_integer {
        return Err(Error::invalid_argument(format!(
            "{key} is outside its supported range"
        )));
    }
    Ok(())
}

fn validate_choice(params: &Json, key: &str, choices: &[&str]) -> Result<(), Error> {
    let Some(value) = params.get(key) else {
        return Ok(());
    };
    match value.as_str() {
        Some(choice) if choices.contains(&choice) => Ok(()),
        _ => Err(Error::invalid_argument(format!("Invalid {key}"))),
    }
}

/// Agent ids appear in other agents' notices and errors, so they stay short and printable.
fn validate_agent_id(params: &Json) -> Result<(), Error> {
    let Some(value) = params.get("agentId") else {
        return Ok(());
    };
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
    let valid = value
        .as_str()
        .is_some_and(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(allowed));
    if !valid {
        return Err(Error::invalid_argument(
            "agentId must be 1-64 letters, digits, '-', '_', or '.'",
        ));
    }
    Ok(())
}

fn require(condition: bool, message: &str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::invalid_argument(message))
    }
}

pub fn validate_params(method: &str, params: &Json) -> Result<(), Error> {
    if !SUPPORTED_METHODS.contains(&method) {
        return Err(Error::invalid_argument(format!("Unknown method: {method}")));
    }
    require(params.is_object(), "params must be an object")?;
    for key in ["x", "y", "fromX", "fromY", "toX", "toY"] {
        validate_number(params, key, 0.0, 1_000_000.0, false)?;
    }
    for key in ["durationMs", "holdBeforeMs", "holdAfterMs"] {
        validate_number(params, key, 0.0, 10_000.0, true)?;
    }
    validate_number(params, "steps", 1.0, 1000.0, true)?;
    validate_number(params, "quality", 1.0, 100.0, true)?;
    validate_number(params, "maxEdge", 0.0, 16384.0, true)?;
    validate_number(params, "amount", 1.0, 1000.0, true)?;
    for key in ["elementIndex", "fromElementIndex", "toElementIndex"] {
        validate_number(params, key, 0.0, 1_000_000.0, true)?;
    }
    for key in [
        "includeText",
        "includeScreenshot",
        "restoreWindow",
        "activate",
        "headless",
        "multiAgent",
    ] {
        if params.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(Error::invalid_argument(format!("{key} must be boolean")));
        }
    }
    for key in [
        "app",
        "windowId",
        "observationId",
        "sessionId",
        "text",
        "key",
        "value",
        "option",
    ] {
        let invalid = params.get(key).is_some_and(|value| {
            value
                .as_str()
                .is_none_or(|text| text.len() > MAXIMUM_STRING_BYTES)
        });
        if invalid {
            return Err(Error::invalid_argument(format!("Invalid {key}")));
        }
    }
    validate_agent_id(params)?;
    validate_choice(params, "button", &["left", "right", "middle"])?;
    validate_choice(params, "format", &["jpeg", "png"])?;
    validate_choice(params, "observe", &["none", "screenshot", "text", "both"])?;
    validate_choice(params, "direction", &["up", "down", "left", "right"])?;
    validate_choice(params, "coords", &["reduced", "full"])?;
    validate_choice(
        params,
        "state",
        &["checked", "unchecked", "expanded", "collapsed", "selected"],
    )?;
    validate_modifiers(params)?;

    if method == "toggle.set" {
        let known_key = params
            .get("key")
            .and_then(Json::as_str)
            .is_some_and(|key| canonical_toggle_key(key).is_some());
        require(
            known_key,
            "toggle key must be shift, ctrl, alt, win, or space",
        )?;
        require(
            params.get("on").is_some_and(Json::is_boolean),
            "on must be boolean",
        )?;
    }
    let uses_element_index = has(params, "elementIndex")
        || has(params, "fromElementIndex")
        || has(params, "toElementIndex");
    require(
        !uses_element_index || has(params, "observationId"),
        "Element indexes require observationId",
    )?;
    if INPUT_ACTIONS.contains(&method)
        || method == "get-app-state"
        || method == "get-full-screenshot"
    {
        require(
            has(params, "windowId") || has(params, "app"),
            "Specify app or windowId",
        )?;
    }
    if method == "drag" {
        for (x_key, y_key) in [("fromX", "fromY"), ("toX", "toY")] {
            require(
                has(params, x_key) == has(params, y_key),
                "Drag coordinates require both x and y",
            )?;
        }
        let has_start = has(params, "fromX") || has(params, "fromElementIndex");
        let has_end = has(params, "toX") || has(params, "toElementIndex");
        require(has_start && has_end, "Drag requires both endpoints")?;
    }
    match method {
        "get-full-screenshot" => require(
            has(params, "observationId"),
            "get-full-screenshot requires observationId",
        ),
        "click" => require(
            has(params, "elementIndex") || (has(params, "x") && has(params, "y")),
            "Click requires an element or x/y",
        ),
        "type-text" | "paste-text" => require(has(params, "text"), "text is required"),
        "press-key" | "hotkey" => require(has(params, "key"), "key is required"),
        "set-value" => require(
            has(params, "elementIndex") && has(params, "value"),
            "set-value requires elementIndex and value",
        ),
        "scroll" => require(has(params, "direction"), "direction is required"),
        "set-state" => require(
            has(params, "elementIndex") && has(params, "state"),
            "set-state requires elementIndex and state",
        ),
        "scroll-to" => require(
            has(params, "elementIndex"),
            "scroll-to requires elementIndex",
        ),
        "select-option" => {
            let has_option = params
                .get("option")
                .and_then(Json::as_str)
                .is_some_and(|option| !option.is_empty());
            require(
                has(params, "elementIndex") && has_option,
                "select-option requires elementIndex and a non-empty option",
            )
        }
        _ => Ok(()),
    }
}

/// Refuses, before any input, an action that cannot reach the target without
/// the user's pointer or keyboard while `params.headless` is true.
pub fn validate_headless_input(method: &str, params: &Json) -> Result<(), Error> {
    if !bool_param(params, "headless") {
        return Ok(());
    }
    let refuse = |message: String| {
        Err(Error::new(
            "headless_unavailable",
            format!("{message}; pass headless false to let it use the real pointer and keyboard"),
        ))
    };
    match method {
        "type-text" => {
            return Err(Error::new(
                "headless_unavailable",
                "type-text presses keys; pass headless false to let it use the real pointer and keyboard, \
                 or use set-value on the field",
            ));
        }
        "drag" | "press-key" | "hotkey" | "paste-text" => {
            return refuse(format!("{method} needs the real pointer or keyboard"));
        }
        "click" => {}
        _ => return Ok(()),
    }
    if !has(params, "elementIndex") {
        return refuse("A coordinate click moves the pointer; click an elementIndex".into());
    }
    if params
        .get("button")
        .and_then(Json::as_str)
        .unwrap_or("left")
        != "left"
    {
        return refuse("Only a primary click has an accessibility action".into());
    }
    if has(params, "modifiers") {
        return refuse("Click modifiers hold real keys".into());
    }
    Ok(())
}

pub fn is_input_action(method: &str) -> bool {
    INPUT_ACTIONS.contains(&method)
}

/// An input action that only ever calls accessibility patterns, headless or
/// not, so it never takes the physical-input turn.
pub fn is_semantic_action(method: &str) -> bool {
    SEMANTIC_ACTIONS.contains(&method)
}

/// Canonical toggle key name (shift, ctrl, alt, win, space).
pub fn canonical_toggle_key(key: &str) -> Option<&'static str> {
    match key.to_ascii_lowercase().as_str() {
        "shift" => Some("shift"),
        "ctrl" | "control" => Some("ctrl"),
        "alt" => Some("alt"),
        "win" | "super" | "meta" => Some("win"),
        "space" => Some("space"),
        _ => None,
    }
}

pub(crate) fn bool_param(params: &Json, key: &str) -> bool {
    params.get(key).and_then(Json::as_bool).unwrap_or(false)
}

pub(crate) fn string_param(params: &Json, key: &str) -> String {
    params
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

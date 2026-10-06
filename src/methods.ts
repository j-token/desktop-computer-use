export const SESSIONLESS_METHODS: ReadonlySet<string> = new Set([
  "doctor",
  "capabilities",
  "session.start",
  "session.status",
  "session.stop",
  "daemon.shutdown",
  "toggle.status"
]);

// Keys that `toggle on` can hold across actions. Aliases are canonicalized by
// the native daemon (control -> ctrl, super/meta -> win).
export const TOGGLE_KEYS: readonly string[] = ["shift", "ctrl", "alt", "win", "space"];
export const TOGGLE_KEY_ALIASES: readonly string[] = [...TOGGLE_KEYS, "control", "super", "meta"];

// Modifier names accepted by --modifiers on click and drag, joined by "+".
export const MODIFIER_NAMES: readonly string[] = ["shift", "ctrl", "control", "alt", "option", "win", "meta", "super"];

export function isValidModifiers(value: string): boolean {
  return value.split("+").every(part => MODIFIER_NAMES.includes(part.toLowerCase()));
}

export const WINDOW_TARGET_METHODS: ReadonlySet<string> = new Set([
  "get-app-state",
  "get-full-screenshot",
  "click",
  "drag",
  "scroll",
  "type-text",
  "press-key",
  "hotkey",
  "set-value",
  "paste-text"
]);

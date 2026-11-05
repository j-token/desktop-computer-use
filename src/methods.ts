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

// Decides how a native response changes the saved session file. A refused stop
// (for example toggles_active) keeps the file. A multi-agent session is never
// saved because agents sharing one runtime directory would overwrite each
// other's ID, and stopping another agent's session keeps the saved one.
export function sessionFileAction(
  method: string,
  response: { ok: boolean; result?: unknown },
  ids: { requested?: string; saved?: string } = {}
): "write" | "clear" | undefined {
  if (!response.ok) return undefined;
  if (method === "session.start") {
    const result = response.result as { multiAgent?: unknown } | undefined;
    return result?.multiAgent === true ? undefined : "write";
  }
  if (method === "session.stop") {
    const stopsSavedSession = ids.requested === undefined || ids.requested === ids.saved;
    return stopsSavedSession ? "clear" : undefined;
  }
  return undefined;
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
  "paste-text",
  "set-state",
  "scroll-to",
  "select-option"
]);

export const ELEMENT_STATES: readonly string[] = ["checked", "unchecked", "expanded", "collapsed", "selected"];

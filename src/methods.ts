export const SESSIONLESS_METHODS: ReadonlySet<string> = new Set([
  "doctor",
  "capabilities",
  "session.start",
  "session.status",
  "session.stop",
  "daemon.shutdown"
]);

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

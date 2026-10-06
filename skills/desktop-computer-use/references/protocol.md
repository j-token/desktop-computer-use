# Desktop Computer Use protocol

The CLI and native helper use one UTF-8 NDJSON request and response per local connection. The
maximum encoded line is 16 MiB. Linux uses a user-private Unix socket; Windows uses a per-user
named pipe. The token file and current session file live beside the Linux socket or in the
Windows local application data directory and are created with user-only permissions where the OS
supports them.

```json
{"id":"uuid","token":"local-secret","method":"get-app-state","params":{"sessionId":"s","app":"notepad"}}
{"id":"uuid","ok":true,"result":{"observationId":"..."}}
```

Native method names remain kebab-case except `session.start`, `session.status`, `session.stop`,
`daemon.shutdown`, and `toggle.set`, `toggle.release-all`, `toggle.status`. The daemon owns authorization, one active session, operation serialization,
interrupt handling, and the 120-second idle lease. `session.start` only succeeds after the platform
indicator and global stop hotkey (`Esc` pressed twice within 1 second) are ready. While active, the
indicator provides a top banner, a high-contrast cursor ring, and a blue screen-edge border that
fades inward; these visual actors must not take focus or intercept application input. `session.stop`
is an emergency path and may be sent without a session ID; it must release held input before
returning. Escape sent by `press-key` or `hotkey` reaches the application and never counts toward
the stop. On Linux, if the GNOME extension cannot release the stop hotkey first, the action fails
with `stop_key_conflict` and nothing is injected.

Each screenshot observation encodes two files from one frame: the original
(`<observationId>-full.<ext>`) and a reduced image (`<observationId>.<ext>`, 0.5x when the source
exceeds 1280x720, otherwise 1x; `maxEdge` optionally caps only the reduced image). The result's
`screenshot` describes only the reduced image: `path`, `mimeType`, `width`, `height`,
`variant: "reduced"`, `actionTransform: {scaleX, scaleY, offsetX: 0, offsetY: 0}` (pixel =
window-local point x scale + offset), `fullAvailable`, `fullWidth`, `fullHeight`, plus `scale`,
`scaleX`, `scaleY`, `sourceWidth`, `sourceHeight`, and capture diagnostics.
`get-full-screenshot` (`observationId` required) returns the cached original with
`variant: "full"` and its own `actionTransform` without recapturing; an expired observation fails
with `stale_observation`. Both files are deleted when the observation leaves the cache or the
session stops.

`click`, `drag`, and `scroll` `x`/`y` (and `fromX`/`fromY`/`toX`/`toY`) are pixels of the reduced
screenshot; `coords: "full"` selects the original image instead. The daemon converts them with the
`observationId` observation or, without one, the window's most recent screenshot observation, and
fails with `observation_required` when there is none. Coordinate action results include
`coordinateSpace` and the window-local point used (`windowPoint`, or `windowFrom`/`windowTo` for a
drag). An observation ID becomes stale when the window geometry, monitor scale, or target state
changes, and the native backend rejects stale element or coordinate use. MCP reads the screenshot
path into an image content block; the CLI leaves the absolute private path in JSON.

Toggles hold keys across requests. `toggle.set {key, on}` (key `shift`, `ctrl`, `alt`, `win`, or
`space`; `control`, `super`, and `meta` are aliases) presses or releases one key and is
idempotent; it returns `{key, on, changed}`. `toggle.release-all` returns `{released: [keys]}`.
Both need the active session. `toggle.status` needs no session and returns `{active}`. The daemon is
the only source of toggle state: every successful result object carries `toggles` (sorted active
keys, `[]` when none), and while any key is held also `` notice: "Toggle still on: shift, space.
Release with `dcu toggle off --all` before ending." `` (appended to an existing notice). Error
responses after authentication carry the same fields in `error.details` while any key is held.
`session.stop` and `daemon.shutdown` sent over RPC fail with `toggles_active` and the held keys while
any toggle is on. Every other session end (Esc twice, idle expiry, operation timeout, client
disconnect, daemon exit) stops the session and releases all toggles. `type-text` fails with
`toggles_active` while `ctrl`, `alt`, or `win` is toggled on Windows or any toggle is on Linux, and
pressing a toggled key as `press-key` or a hotkey base fails the same way. `click` and `drag` accept
`modifiers` (`shift`, `ctrl`, `alt`, `win` joined by `+`) held for that one action; a toggled
modifier is neither pressed again nor released by it.

`list-windows` and `list-apps` include visible dialogs owned by another window (Windows owned
windows, Linux transient windows). Such a window carries `ownerWindowId`, the nearest listed
owner, and `modal`, which is `true` while it blocks input to that owner (Windows: the owner is
disabled; Linux: a modal-dialog window type or `_NET_WM_STATE_MODAL`). Windows records also carry
`isEnabled`. `app` selects the main (unowned) window before its dialogs; `windowId` accepts a
dialog's ID for observation, screenshots, accessibility, and input. When the target is blocked,
`get-app-state` adds `modal: {windowId, title, app}` for the dialog receiving input (the deepest
one when dialogs are nested) and a `notice`, and input methods fail with `modal_active` before
sending anything, naming the dialog's window ID in the message. On Windows the target's
screenshot and accessibility tree exclude the dialog because they are captured per window.

Actions return `delivered: true` only when the provider accepted the input sequence. Their
verification state is `unverified` unless the follow-up observation asserts the intended state.
Transport errors after request bytes were written are terminal for that mutation: inspect fresh
state before deciding whether to act again.

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
and `daemon.shutdown`. The daemon owns authorization, one active session, operation serialization,
interrupt handling, and the 120-second idle lease. `session.start` only succeeds after the platform
indicator and global `Esc` stop hotkey are ready. While active, the indicator provides a top banner,
a high-contrast cursor ring, and a blue screen-edge border that fades inward; these visual actors
must not take focus or intercept application input. `session.stop` is an emergency path and may be
sent without a session ID; it must release held input before returning.

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

Actions return `delivered: true` only when the provider accepted the input sequence. Their
verification state is `unverified` unless the follow-up observation asserts the intended state.
Transport errors after request bytes were written are terminal for that mutation: inspect fresh
state before deciding whether to act again.

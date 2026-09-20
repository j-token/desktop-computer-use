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

Observation coordinates are window-local logical action coordinates. A screenshot may be scaled or
cropped. Use the latest observation's explicit action transform when it is present:
`actionX = (pixelX - offsetX) / scaleX` and `actionY = (pixelY - offsetY) / scaleY`. If the
observation has no `actionTransform`, use `scaleX`/`scaleY` when present and otherwise `scale`; an
omitted offset is zero. `window.x`/`window.y` describe the window's screen position and must not be
used as screenshot offsets. An observation ID becomes stale when the window geometry, monitor scale,
crop, or target state changes, and the native backend rejects stale element or coordinate use.
Observation results may include `screenshot.path`, `mimeType`, `width`, `height`, `sourceWidth`,
`sourceHeight`, `scale`, `scaleX`, `scaleY`, optional `offsetX`/`offsetY`, and an optional
`actionTransform`, as well as an accessibility element list, timings, and overlay regions. MCP reads
a native screenshot path into an image content block; the CLI leaves the absolute private path in
JSON.

Actions return `delivered: true` only when the provider accepted the input sequence. Their
verification state is `unverified` unless the follow-up observation asserts the intended state.
Transport errors after request bytes were written are terminal for that mutation: inspect fresh
state before deciding whether to act again.

# Desktop Computer Use protocol

The CLI and native helper use one UTF-8 NDJSON request and response per local connection. The
maximum encoded line is 16 MiB. Linux uses a user-private Unix socket; Windows uses a per-user
named pipe. The token file and current session file live beside the Linux socket or in the
Windows local application data directory and are created with user-only permissions where the OS
supports them. The default Windows pipe name ends with a fingerprint of the token, so a packaged
host that virtualizes local application data (for example Codex Desktop) gets its own daemon.

```json
{"id":"uuid","token":"local-secret","method":"get-app-state","params":{"sessionId":"s","app":"notepad"}}
{"id":"uuid","ok":true,"result":{"observationId":"..."}}
```

Native method names remain kebab-case except `session.start`, `session.status`, `session.stop`,
`daemon.shutdown`, and `toggle.set`, `toggle.release-all`, `toggle.status`. The daemon owns authorization, the desktop session and its agent sessions, operation scheduling,
interrupt handling, and the 120-second idle lease (per agent session). `session.start` only succeeds after the platform
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
drag). On Windows an element `click` or `drag` that falls back to pointer input targets the
element's bounds as UI Automation reports them at action time, clipped to its scrollable ancestors
and the window, so a page that scrolled or reflowed since the observation does not shift the target;
the click result then carries `windowPoint`. An element that has left the tree fails with
`stale_observation`, and one with no visible area (for example scrolled under a browser toolbar)
fails with `element_not_actionable`. When a scrollable container is under the pointer, a Windows
`scroll` waits up to one second for its scroll position to change and settle before returning and
reports `layoutSettled`; `false` means it did not move or settle (end of page), so observe again
before acting on element bounds. Without such a container the field is absent and the scroll
returns at once. On Windows every input action watches its window's UI Automation layout,
structure, and scroll events from just before the input, and the first accessibility observation
of that window afterwards (an attached `observe: "text"`/`"both"` or a later `get-app-state` with
text) waits for them: with none within 0.7 seconds of the input it reads the tree at once;
otherwise it waits until they stop for 0.6 seconds, up to 2.5 seconds after the input, and
reports `layoutSettled` (`false` when the page kept changing, so
observe again before acting on its bounds). This catches inputs that animate or scroll the page,
whose new locations Chrome publishes up to about half a second late. Screenshot-only observations
never wait. An observation ID becomes stale when the window geometry, monitor scale, or target state
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

Headless mode never moves the user's pointer, sends keys, or changes focus or the foreground
window. `session.start {headless: true}` sets the default for that agent session; any request may
pass `headless` to override it. A headless `click` needs `elementIndex`, the left button, and no
`modifiers`, and performs the element's accessibility action (Windows: Invoke, SelectionItem, Toggle,
ExpandCollapse, then LegacyIAccessible default action; Linux: the AT-SPI action named click, press,
activate, toggle, jump, open, or expand or contract, else action 0). `set-value` uses ValuePattern,
then RangeValuePattern for a number (Linux: AT-SPI EditableText or Value) without activating the
window. A Windows headless `scroll` uses the ScrollPattern of the element at `elementIndex`
(or the container under `x`/`y`) or its nearest scrollable ancestor, one small increment per three
wheel lines, and reports `layoutSettled`. `get-app-state` never activates the window unless
`activate` is true. Anything that would need real input fails with `headless_unavailable` before
sending anything: `type-text` (use `set-value`), `paste-text`, `press-key`, `hotkey`, `drag`, a
coordinate click, a non-left button, click `modifiers`, an element with no accessibility action,
and Linux `scroll`. Every input result carries `delivery: "semantic"` (with `semanticAction`, for
example `invoke` or `toggle`) or `delivery: "physical"`; a non-headless element click that succeeds
through Invoke is also reported as semantic. `capabilities.headless` lists what each platform
supports.

`set-state {elementIndex, state}` and `scroll-to {elementIndex}` are always semantic, with or without `headless`: they never activate the window, never
take the physical-input turn, and ignore other agents' toggles. `state` is `checked` or `unchecked`
(TogglePattern, toggled at most twice so a three-state check box reaches off),
`expanded` or `collapsed` (ExpandCollapsePattern; a leaf node fails), or `selected`
(SelectionItemPattern). The element is changed only when it is not already in that state, and the
state is read back for up to 600 ms. The result carries `semanticAction` (`toggle`,
`expandCollapse`, or `select`), `state`, `changed`, and `verified`. `scroll-to` calls
ScrollItemPattern.ScrollIntoView and waits up to 800 ms for the element to intersect the window
inside every scrollable ancestor. When the element has no ScrollItemPattern or is still hidden, it
scrolls the nearest ancestor with a ScrollPattern instead: up to 10 times it centers an element
that has bounds through SetScrollPercent, and an element without bounds yet (an unrealized row of a
virtualized list) is found by paging the ancestor from its start with LargeIncrement until it
shows or the position stops moving. The result reports `visible` and `semanticAction`
(`scrollIntoView` or `scrollPattern`). An element with neither pattern nor scrollable ancestor, a
refused call, or a provider fault fails with `pattern_unavailable`.

`select-option {elementIndex, option}` is semantic in the same way. It selects the
one SelectionItemPattern descendant of the element whose name equals `option`, ignoring case. A
collapsed container with ExpandCollapsePattern is expanded first; a Win32 combo box without it is
opened through the Invoke of its drop-down button. Either is closed again afterwards. When no
realized item matches, a scrollable container (the element or its first scrollable descendant) is
paged from its start until one appears. The selection is read back for up to 600 ms, and the result
carries `semanticAction: "select"`, `option`, `changed`, and `verified`. No match fails with
`option_not_found`; several matches in one page fail with `ambiguous_target` (use `set-state
selected` on one of them instead). Opening a Win32 combo box can move keyboard focus to it inside
its window. Observation elements list `scrollItem` among their
`patterns` when ScrollIntoView is available. `set-value` with an empty `value` clears a field.

On Linux the same three methods use AT-SPI and need a build with AT-SPI
(`capabilities.headless.setState`, `scrollTo`, `selectOption`). `checked`/`unchecked` need a
checkable element and `expanded`/`collapsed` an expandable one; both run the element's action named
toggle, click, press, or activate (for expansion: expand or collapse, expand or contract, toggle,
activate, press, click), at most twice. `selected` and `select-option` select through the parent's
(for a combo box, the combo box's) Selection interface, else the action named select, click, press,
or activate. The state is read back for up to 600 ms, and `semanticAction` names the AT-SPI action
or `selection`. `select-option` matches descendants that are selectable or have an item role (list,
menu, tree, or table cell items and page tabs) and does not open a combo box or page a list, so it
finds only items the toolkit already exposes. `scroll-to` calls Component.scroll_to, then moves the
scroll bars of the nearest ancestor that has them: it centers an element with bounds, assuming one
scroll bar unit per pixel as in GTK, and pages the vertical bar from the top for one without bounds.
It reports `semanticAction` `scrollTo` or `scrollBar`, or none when the element was already shown.

One desktop session (indicator, Esc stop, input lease) can be shared by up to 16 agent sessions.
`session.start {multiAgent: true, agentId?}` starts or joins it; joining requires that both the
running session and the new request use `multiAgent`, otherwise the start fails with
`session_busy`. `agentId` is 1-64 characters of letters, digits, `-`, `_`, or `.`; it defaults to
`agent-N`, and a duplicate fails with `agent_id_in_use`. The result carries a distinct
`sessionId`, `agentId`, `multiAgent`, `headless`, `joined`, and `agentCount`. A multi-agent
`sessionId` is not written to the session file, so every request must pass it (the CLI also reads
`DCU_SESSION_ID`). Observation IDs belong to the agent session that created them; another agent's
ID fails with `stale_observation`. `session.status` lists `agents` (`agentId`, `multiAgent`,
`headless`, `busy`, `idleMs`, `current`) and shows only the caller's own `sessionId`.
`session.stop {sessionId}` stops that agent only, cancels its running operation, and returns
`{stopped, agentId, agentCount}`; stopping the last agent, or `session.stop` without a session ID,
Esc twice, or a 120-second operation timeout stops the whole desktop session. Each agent session
expires separately after 120 idle seconds. Only the agent that turned a toggle on may change it,
and its stop fails with `toggles_active` until released; while a toggle is held, another agent's
physical input fails with `toggles_active`.

Agents run at the same time. On Windows observations, screenshots, list methods, and headless
actions of different agents run concurrently; physical input (non-headless actions, `toggle.set`,
`toggle.release-all`, and `get-app-state` with `activate: true`) runs one at a time because there is
one pointer and keyboard. On Linux AT-SPI and Xlib are not thread-safe, so the daemon runs one
operation at a time across agents. A request waits up to 45 seconds for its turn and then fails
with `busy`; a second request from the same agent session while its first is running fails with
`busy` at once.

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
On Windows `get-app-state` also adds `ownedWindows`, the window records of visible non-modal
windows owned by the target (for example Chrome permission and download bubbles), and a `notice`
naming them, because the target's per-window screenshot excludes them. Some apps, Chrome included, still
expose such popups in the target's accessibility tree.

Actions return `delivered: true` only when the provider accepted the input sequence. Their
verification state is `unverified` unless the follow-up observation asserts the intended state.
Transport errors after request bytes were written are terminal for that mutation: inspect fresh
state before deciding whether to act again.

## CLI output

The CLI prints compact JSON by default: the request `id`, timings, capture diagnostics, empty
`toggles`, and the duplicate accessibility `tree` text are dropped; window records and
accessibility elements become one-line strings; elements with no area, actions, or focus are
omitted and counted in `hidden`. MCP text content uses the same shape. `--raw` prints the native
response unchanged.

## Linux setup

`dcu setup` selects the legacy GNOME extension archive for GNOME 42–44 or the modern archive for
GNOME 45–50, installs it under the current user's extension directory, compiles its schemas when
`glib-compile-schemas` is available, and requests enablement without logging out. Missing commands
or native libraries are reported with package names and an apt command; without passwordless sudo
it reports the command for a root shell and leaves `installed` false. `dcu doctor` stays
fail-closed until the native backend and indicator are ready. X11 actions may require XTest/AT-SPI
packages; Wayland may ask for RemoteDesktop/ScreenCast portal approval.

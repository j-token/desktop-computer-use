# Desktop Computer Use GNOME indicator

This directory contains the user-local GNOME Shell indicator used by the
Desktop Computer Use native backend. It is deliberately split into the
GNOME 45–50 ES-module extension and the GNOME 42–44 legacy extension. Both
variants use the same UUID and D-Bus wire contract, so only the variant for
the running shell should be installed.

The service owns:

```text
bus       org.desktopcomputeruse.Shell
object    /org/desktopcomputeruse/Shell
interface org.desktopcomputeruse.Shell
```

`Start(sessionId)` returns a JSON string with `ready: true` only after the
banner, panel indicator, and high-contrast cursor actor are visible.
`Heartbeat()` keeps the session alive; five seconds without one hides the UI
and emits `Stopped("heartbeat-timeout")`. `Stop()` and the global
`Esc` accelerator, pressed twice within one second, release the UI and emit
`Stopped` as well. A single `Esc` only shows a reminder on the banner for one
second. The accelerator is bound with every Shift/Control/Alt/Super
combination so held modifiers cannot block the stop, and auto-repeat is
ignored. `SuspendStopKey(milliseconds)` removes the binding for at most two
seconds so an `Esc` injected by the native backend reaches the application
instead of counting toward the stop.

The Escape binding is registered only while a computer-use session is active, so normal Escape behavior remains available after stop. Each monitor receives four nonreactive blue edge strips whose alpha fades inward across 18 logical pixels. The border, banner, and cursor marker never reserve screen space or accept clicks.
`ListWindows()` returns JSON records with `id`, `title`, `app`, `pid`, `x`,
`y`, `width`, and `height` in GNOME logical screen coordinates. Untitled
windows are retained so borderless/windowless games can still be bound by
their stable Meta window ID. `Activate(windowId)` activates a matching Meta
window. `Pointer(x, y)` updates the ring immediately while a 16 ms poll keeps
it aligned with the real shell pointer.

Every visual actor has `reactive: false` and `can_focus: false`; the indicator
does not take focus or intercept application input. The cursor theme is not
changed globally. The shell extension does not grant input permission or
inject input; that remains the native backend's responsibility.

## Build and install

On Linux, package both archives with:

```sh
./extensions/package.sh
```

The skill packaging step should copy these exact files:

```text
extensions/dist/desktop-computer-use-gnome-modern.zip
extensions/dist/desktop-computer-use-gnome-legacy.zip
```

The archive contains `metadata.json` at its root, which is the layout
expected by `gnome-extensions install`; the command installs it under the
UUID from that metadata. Install the variant matching the current shell with
`./extensions/install.sh auto` (or pass `modern`/`legacy`) and enable it
separately:

Each archive includes the GNOME-required `extension.js` entrypoint.

```sh
gnome-extensions enable desktop-computer-use@local
```

`package.ps1` creates the same archive names on Windows build hosts. Users
need a GNOME Shell session, `gnome-extensions` (or `unzip` for the fallback
installer), and `glib-compile-schemas` when the shell does not compile the
included schema automatically. Wayland RemoteDesktop/ScreenCast portal
permissions remain a native-backend setup concern.

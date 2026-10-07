---
name: desktop-computer-use
description: >-
  Inspect and operate visible native desktop applications and games with the standalone
  authenticated computer-use CLI. Use for OS-level UI, window, screenshot, keyboard, mouse,
  and paced drag work on Windows or Linux; use browser/page automation for DOM-only tasks.
---

# Desktop Computer Use

Use the bundled `scripts/dcu.mjs` entrypoint (run it as `node <skill-root>/scripts/dcu.mjs`; below,
`dcu` is that command). It starts the packaged native daemon on demand and prints one JSON
response per command. Set `DCU_NATIVE_PATH` only for development when a packaged platform binary
is not present.

## Safe observation loop

1. Run `dcu capabilities`, then `dcu session start` and wait for the visible “computer in use”
   indicator. While active, the indicator shows a top banner, a high-contrast cursor ring, and
   a blue screen-edge border that fades inward. End your work with `dcu session stop` (release
   any toggles first). The user can stop the session at any time by pressing `Esc` twice within 1 second.
2. Use `dcu list-apps`, `dcu list-windows --app <app>`, and
   `dcu get-app-state --app <app>` before choosing a target. The state response includes a fresh
   observation ID, the window rectangle, a screenshot path, and optional accessibility elements.
3. Use an element index only from the latest observation. The observation's `screenshot` is a
   reduced image (0.5x when the window is larger than 1280x720, otherwise original size). Pass
   `--x`/`--y` exactly as read from that reduced screenshot; the daemon converts them using the
   window's latest screenshot observation (or `--observation-id`), so do no scaling yourself. A
   coordinate action without a screenshot observation of the window fails with
   `observation_required`. When the reduced image is not detailed enough, run
   `get-full-screenshot --observation-id ID` for the original and pass `--coords full` with points
   read from it. Re-observe when the window rectangle or monitor scale changes.
4. After every UI-changing action, inspect its returned observation when requested with
   `--observe screenshot|text|both`, or call `get-app-state` again before selecting another index.
A delivered synthetic input is unverified unless a returned state proves the requested change.
5. Dialogs (message boxes, save/open pickers, browser permission or file prompts) are separate
   windows. When an observation reports `modal` or an action fails with `modal_active`, observe
   and act on that dialog's `--window-id`; `list-windows` shows dialogs with `ownerWindowId`. On
   Windows the main window's screenshot does not include the dialog. Check `list-windows` for a
   dialog before handing work back to the user.

On Linux, run `dcu setup` when installing the skill. It selects the legacy archive for GNOME
42–44 or the modern archive for GNOME 45–50, installs it under the current user's GNOME extension
directory, compiles its schemas when `glib-compile-schemas` is available, and requests enablement
without logging out or restarting the shell. If a command or native library is missing, setup
reports the package names and an apt command; when passwordless sudo is unavailable, it reports
the command for a root shell and leaves `installed` false. Resolve those diagnostics and confirm
`dcu doctor` before starting a session. Doctor remains fail-closed until the native backend and
indicator are ready.

## Commands and input

```text
dcu doctor | capabilities
dcu session start | status | stop
dcu list-apps | list-windows --app APP | get-app-state --app APP [--include-text]
dcu get-full-screenshot --app APP --observation-id ID
dcu click --app APP (--element-index N | --x X --y Y [--coords full]) [--modifiers shift+ctrl]
dcu drag --app APP (--from-element-index N --to-element-index N | --from-x X --from-y Y --to-x X --to-y Y [--coords full]) [--modifiers shift]
dcu scroll --app APP --x X --y Y --direction down [--amount 3] [--coords full]
dcu type-text --app APP --text TEXT
dcu press-key --app APP --key KEY
dcu hotkey --app APP --key MODIFIER+KEY
dcu set-value --app APP --element-index N --value TEXT
dcu paste-text --app APP --text TEXT
dcu toggle on --key shift|ctrl|alt|win|space | toggle off --key KEY | toggle off --all | toggle status
```

`drag` defaults to `--duration-ms 240 --steps 12 --hold-before-ms 50 --hold-after-ms 50`.
It emits each intermediate move immediately so native applications and games observe a real
button-held drag. The daemon releases held buttons and modifiers when a drag is cancelled, a
session stops, or the connection is lost. Never resend a mutation after a timeout until a fresh
observation establishes whether it arrived.

## Holding keys (toggles)

When a key must stay held across several actions, toggle it. Examples are Photoshop shift-click
to add to a selection, space-drag to pan, and a Ctrl or Alt modifier held over several clicks.

1. Run `dcu toggle on --key shift` (keys: `shift`, `ctrl`, `alt`, `win`, `space`).
2. Run the clicks and drags. While any key is held, every result, error included, lists
   `toggles` (for example `["shift"]`) and a `notice` such as
   `` Toggle still on: shift. Release with `dcu toggle off --all` before ending. ``
3. Release as soon as the held-key work is done: `dcu toggle off --key shift`, or
   `dcu toggle off --all`. `dcu toggle status` lists the held keys.

`dcu session stop` fails with `toggles_active` (exit 1, session kept) until every toggle is
released. `type-text` is refused with `toggles_active` while `ctrl`, `alt`, or `win` is toggled
on Windows, and while any toggle is on Linux; use `paste-text` or release first. Pressing a toggled
key itself (for example `press-key --key space` while space is toggled) is also refused. When the
hold is for one click or drag only, prefer `--modifiers shift` (or `ctrl+alt`) on that action
instead of a toggle. Esc pressed twice, the 120-second idle expiry, and a lost connection end the
session and release every toggle.

Use `--text-stdin` or `--value-stdin` for sensitive values. Use `--no-screenshot` only when the
tree or action result is sufficient. Linux requires a logged-in desktop session; Wayland may ask
for RemoteDesktop/ScreenCast portal approval, and X11 actions may require XTest/AT-SPI packages.

## Deciding each step

You, the model running this skill, make every decision. There is no separate model or API key.
Plan the task yourself, inspect the current accessibility tree, choose one current element, and
call the corresponding native command. Prefer `get-app-state --include-text --no-screenshot` for
accessible controls and request a screenshot when the text is insufficient. Use
`--observation-id` and `--window-id` for element actions and `--observe text` to receive the next
state in the same round trip; use that returned state for the next decision instead of taking a
duplicate observation. Generate needed text yourself from the user's goal and verify the result
after input. On an uncertain transport result, inspect state before any further input. On
Esc/session loss, stop; do not restart the session automatically.

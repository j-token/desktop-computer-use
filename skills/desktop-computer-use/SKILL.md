---
name: desktop-computer-use
description: >-
  Inspect and operate visible native desktop applications and games with the standalone
  authenticated computer-use CLI. Use for OS-level UI, window, screenshot, keyboard, mouse,
  and paced drag work on Windows or Linux; use browser/page automation for DOM-only tasks.
---

# Desktop Computer Use

`dcu` below means `node <skill-root>/scripts/dcu.mjs`. It starts the native daemon on demand and
prints one compact JSON line per command (`--raw` prints the full native response). You make every
decision; there is no other model.

## Loop

1. `dcu session start` (shows the "computer in use" indicator). End with `dcu session stop` after
   releasing toggles.
2. Find the target: `dcu list-windows --app APP` (or `list-apps`).
3. Observe: `dcu get-app-state --app APP --include-text --no-screenshot`. Add a screenshot only
   when the elements are not enough.
4. Act with `--observation-id ID` and `--observe text` so the result carries the next state; decide
   from that state instead of observing again. Element indexes are valid only for their observation.
5. Input is unverified until a later state shows the change. After a timeout or transport error,
   observe before sending anything again.

## Output

- Window: `"hwnd:1 \"Title\" app=x 10,20 800x600 fg"` (`fg` foreground, `min`, `disabled`,
  `owner=ID modal` for dialogs).
- Element: `"12 Button \"OK\" 10,20 80x24 invoke focused"` = index, type, name (or `#automationId`),
  `="value"` when it differs, window-local rect, actions (`invoke`, `value`, `select`, `toggle`),
  flags. Elements with no area, actions, or focus are omitted and counted in `hidden`. Text over 300
  characters is clipped; use `--raw` for the full value.
- `screenshot: {path, width, height, full?}`; `full: "WxH"` means the image was reduced.

## Coordinates

`--x/--y` are pixels of the latest reduced screenshot of that window (or `--observation-id`); the
daemon converts them. Without a screenshot observation, coordinate actions fail with
`observation_required`. For more detail run `get-full-screenshot --observation-id ID` and pass
`--coords full` with points from that image. Re-observe after the window moves or resizes.

## Dialogs

Dialogs are separate windows. On `modal` in a state or a `modal_active` error, act on the dialog's
`--window-id` (from the notice or `list-windows`). On Windows the owner's screenshot excludes it.

Popups that do not block the owner, such as browser permission and download bubbles, appear in a
Windows state as `ownedWindows` with a notice; the screenshot usually excludes them too. When an action
seems to do nothing (a download click saves no file, a page shows no change), check `ownedWindows`
and the elements for such a prompt before reporting failure. Ask the user before allowing a
permission.

## Commands

```text
dcu doctor | capabilities | setup
dcu session start | status | stop
dcu list-apps | list-windows --app APP | get-app-state --app APP [--include-text] [--no-screenshot]
dcu get-full-screenshot --app APP --observation-id ID
dcu click --app APP (--element-index N | --x X --y Y [--coords full]) [--modifiers shift+ctrl]
dcu drag --app APP (--from-element-index N --to-element-index N | --from-x X --from-y Y --to-x X --to-y Y)
dcu scroll --app APP --x X --y Y --direction up|down|left|right [--amount 3]
dcu type-text|paste-text --app APP --text TEXT     (--text-stdin for secrets)
dcu set-value --app APP --element-index N --value TEXT   (--value-stdin for secrets)
dcu press-key --app APP --key KEY | hotkey --app APP --key MODIFIER+KEY
dcu toggle on|off --key shift|ctrl|alt|win|space | toggle off --all | toggle status
```

Element indexes require `--observation-id`. `--window-id ID` can replace `--app`. Drag defaults: `--duration-ms 240 --steps 12
--hold-before-ms 50 --hold-after-ms 50`, emitted as a real held-button drag.

## Held keys

Prefer `--modifiers` on one click or drag. To hold a key across actions, `toggle on --key K`, act,
then `toggle off --all` promptly. While held, results show `toggles` and a `notice`. `session stop`
fails with `toggles_active` until released. `type-text` is refused while ctrl/alt/win (Windows) or
any key (Linux) is toggled; use `paste-text`. Pressing a toggled key is refused.

## Linux

Run `dcu setup` once, resolve the packages it reports, then confirm `dcu doctor` is ready before a
session. Wayland may ask for portal approval. Details: `references/protocol.md`.

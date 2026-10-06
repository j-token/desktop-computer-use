#!/usr/bin/env bash
set -euo pipefail
check_call() {
  local expected="$1"
  shift
  local method="$1"
  shift
  local response
  response=$(gdbus call --session --dest org.desktopcomputeruse.Shell --object-path /org/desktopcomputeruse/Shell --method "org.desktopcomputeruse.Shell.$method" "$@")
  printf '%s\n' "$response"
  [[ "$response" == *"$expected"* ]]
}
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
Xvfb :99 -screen 0 1280x720x24 -ac >/tmp/xvfb.log 2>&1 &
xvfb_pid=$!
shell_pid=''
trap 'if [ -n "$shell_pid" ]; then kill "$shell_pid" 2>/dev/null || true; fi; kill "$xvfb_pid" 2>/dev/null || true' EXIT
sleep 1
gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell enabled-extensions "['desktop-computer-use@local']"
GSETTINGS_SCHEMA_DIR="$HOME/.local/share/gnome-shell/extensions/desktop-computer-use@local/schemas" \
  gsettings set org.gnome.shell.extensions.desktop-computer-use desktop-computer-use-stop-accelerator "['<Control><Alt>Pause']"
gnome-shell --nested --wayland >/tmp/shell.log 2>&1 &
shell_pid=$!
ready=false
for attempt in $(seq 1 40); do
  if gdbus introspect --session --dest org.desktopcomputeruse.Shell --object-path /org/desktopcomputeruse/Shell >/tmp/extension.xml 2>/dev/null; then ready=true; break; fi
  if ! kill -0 "$shell_pid" 2>/dev/null; then break; fi
  sleep 1
done
if [ "$ready" != true ]; then cat /tmp/shell.log; exit 1; fi
gnome-shell --version
WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland /test/.venv/bin/python /test/gtk-fixture.py /tmp/fixture.ndjson >/tmp/fixture.log 2>&1 &
sleep 1
check_call '"ready":true' Start isolated-gnome
check_call '(true,)' Heartbeat
check_call 'DCU native drag fixture' ListWindows
window_response=$(gdbus call --session --dest org.desktopcomputeruse.Shell --object-path /org/desktopcomputeruse/Shell --method org.desktopcomputeruse.Shell.ListWindows)
fixture_window_id=$(/test/.venv/bin/python /test/select-window.py "$window_response")
check_call '(true,)' Activate "$fixture_window_id"
sleep 1
check_call 'screen-border' GetOverlayRegions
if [ -d /results ]; then
  GDK_BACKEND=x11 DISPLAY=:99 /test/.venv/bin/python - <<'PY'
import gi
gi.require_version('Gdk', '3.0')
from gi.repository import Gdk
window = Gdk.get_default_root_window()
Gdk.pixbuf_get_from_window(window, 0, 0, window.get_width(), window.get_height()).savev('/results/indicator.png', 'png', [], [])
PY
fi
check_call '(true,)' Stop
check_call '"ready":true' Start escape-check
sleep 0.25
# The nested compositor must receive XTest input rather than the Xvfb root.
nested_window_id=$(DISPLAY=:99 xdotool search --onlyvisible --pid "$shell_pid" | head -n 1)
DISPLAY=:99 xdotool windowfocus --sync "$nested_window_id"
# A single Escape only arms the stop.
DISPLAY=:99 xdotool key --clearmodifiers Escape
sleep 0.25
check_call '(true,)' Heartbeat
sleep 1.2
# While the stop key is suspended, Escape reaches applications and is not counted.
check_call '(true,)' SuspendStopKey 1500
DISPLAY=:99 xdotool key --clearmodifiers Escape
sleep 0.2
DISPLAY=:99 xdotool key --clearmodifiers Escape
sleep 0.25
check_call '(true,)' Heartbeat
sleep 1.5
# Two Escapes within one second stop the session, even with a modifier held.
DISPLAY=:99 xdotool key Escape
sleep 0.2
DISPLAY=:99 xdotool key shift+Escape
sleep 0.25
check_call '(false,)' Heartbeat
check_call '[]' GetOverlayRegions
check_call '"ready":true' Start watchdog-check
sleep 7
check_call '(false,)' Heartbeat
cat /tmp/shell.log

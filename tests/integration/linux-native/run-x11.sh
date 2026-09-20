#!/usr/bin/env bash
set -Eeuo pipefail

RESULTS_DIR="${RESULTS_DIR:-/results}"
DISPLAY="${DISPLAY:-:99}"
PYTHON_BIN="${PYTHON_BIN:-/test/.venv/bin/python}"
NODE_BIN="${NODE_BIN:-/test/node/bin/node}"
DCU_CLI="${DCU_CLI:-/test/project/skills/desktop-computer-use/scripts/dcu.mjs}"
DCU_NATIVE_PATH="${DCU_NATIVE_PATH:-/test/native/desktop-computer-use-native}"
DCU_RUNTIME_DIR="${DCU_RUNTIME_DIR:-/tmp/dcu-runtime/linux-native-x11}"

export DISPLAY
export PYTHON_BIN NODE_BIN DCU_CLI DCU_NATIVE_PATH DCU_RUNTIME_DIR
export XDG_SESSION_TYPE=x11
export GDK_BACKEND=x11
unset WAYLAND_DISPLAY

mkdir -p "$RESULTS_DIR"
if ! touch "$RESULTS_DIR/.write-check"; then
  printf '%s\n' '{"status":"blocked","reason":"results directory is not writable"}' \
    > "$RESULTS_DIR/preflight.json" 2>/dev/null || true
  exit 77
fi
rm -f "$RESULTS_DIR/.write-check"

# A derived image intentionally does not contain Node or the native daemon.
# The two mounts make the test independent of the host's desktop and build SDK.
if [[ ! -x "$NODE_BIN" ]] && [[ "$NODE_BIN" == "/test/node/bin/node" ]] && command -v node >/dev/null 2>&1; then
  NODE_BIN="$(command -v node)"
  export NODE_BIN
fi
if [[ ! -f "$DCU_CLI" ]] && [[ "$DCU_CLI" == "/test/project/skills/desktop-computer-use/scripts/dcu.mjs" ]]; then
  for candidate in /test/cli/dcu.mjs /test/cli/scripts/dcu.mjs; do
    if [[ -f "$candidate" ]]; then
      DCU_CLI="$candidate"
      export DCU_CLI
      break
    fi
  done
fi

missing=()
[[ -x "$NODE_BIN" ]] || missing+=("node runtime: $NODE_BIN")
[[ -f "$DCU_CLI" ]] || missing+=("CLI bundle: $DCU_CLI")
[[ -x "$DCU_NATIVE_PATH" ]] || missing+=("native daemon: $DCU_NATIVE_PATH")
[[ -x "$PYTHON_BIN" ]] || missing+=("Python fixture runtime: $PYTHON_BIN")
if (( ${#missing[@]} > 0 )); then
  {
    printf '{"status":"blocked","reason":"required runtime mount is missing","missing":['
    for ((index = 0; index < ${#missing[@]}; index += 1)); do
      if ((index > 0)); then printf ','; fi
      printf '"%s"' "${missing[index]}"
    done
    printf ']}\n'
  } > "$RESULTS_DIR/preflight.json"
  printf 'Blocked before X11 startup:\n' >&2
  printf '  %s\n' "${missing[@]}" >&2
  exit 77
fi

rm -rf "$DCU_RUNTIME_DIR"
mkdir -p "$DCU_RUNTIME_DIR"
chmod 700 "$DCU_RUNTIME_DIR"
: > "$RESULTS_DIR/commands.log"

xvfb_pid=""
shell_pid=""
fixture_pid=""
cleanup() {
  set +e
  if [[ -x "$NODE_BIN" && -f "$DCU_CLI" ]]; then
    "$NODE_BIN" "$DCU_CLI" session stop >"$RESULTS_DIR/cleanup-stop.json" 2>"$RESULTS_DIR/cleanup-stop.stderr"
    "$NODE_BIN" "$DCU_CLI" daemon shutdown >"$RESULTS_DIR/cleanup-shutdown.json" 2>"$RESULTS_DIR/cleanup-shutdown.stderr"
  fi
  if [[ -n "$fixture_pid" ]]; then kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true; fi
  if [[ -n "$shell_pid" ]]; then kill "$shell_pid" 2>/dev/null || true; wait "$shell_pid" 2>/dev/null || true; fi
  if [[ -n "$xvfb_pid" ]]; then kill "$xvfb_pid" 2>/dev/null || true; wait "$xvfb_pid" 2>/dev/null || true; fi
  if [[ -f "$DCU_RUNTIME_DIR/daemon.log" ]]; then cp "$DCU_RUNTIME_DIR/daemon.log" "$RESULTS_DIR/daemon.log"; fi
}
trap cleanup EXIT INT TERM

run_cli() {
  local name="$1"
  shift
  {
    printf '%q ' "$NODE_BIN" "$DCU_CLI" "$@"
    printf '\n'
  } >> "$RESULTS_DIR/commands.log"
  set +e
  "$NODE_BIN" "$DCU_CLI" "$@" >"$RESULTS_DIR/$name.json" 2>"$RESULTS_DIR/$name.stderr"
  local code=$?
  set -e
  printf '%s\n' "$code" > "$RESULTS_DIR/$name.exit"
  return "$code"
}

run_cli_retry_busy() {
  local name="$1"
  shift
  for ((attempt = 1; attempt <= 12; attempt += 1)); do
    if run_cli "$name" "$@"; then return 0; fi
    if ! grep -Fq '"code":"busy"' "$RESULTS_DIR/$name.json"; then return 1; fi
    sleep 0.25
  done
  return 1
}

wait_for_file_line() {
  local path="$1"
  local needle="$2"
  local attempts="$3"
  for ((attempt = 1; attempt <= attempts; attempt += 1)); do
    if grep -Fq "$needle" "$path" 2>/dev/null; then return 0; fi
    sleep 0.05
  done
  return 1
}

wait_for_new_file_line() {
  local path="$1"
  local needle="$2"
  local baseline_lines="$3"
  local attempts="$4"
  for ((attempt = 1; attempt <= attempts; attempt += 1)); do
    local current_lines
    current_lines="$(wc -l < "$path")"
    if ((current_lines > baseline_lines)) &&
       tail -n "+$((baseline_lines + 1))" "$path" | grep -Fq "$needle"; then
      return 0
    fi
    sleep 0.05
  done
  return 1
}

Xvfb "$DISPLAY" -screen 0 1280x720x24 -ac +extension RANDR >"$RESULTS_DIR/xvfb.log" 2>&1 &
xvfb_pid=$!
sleep 0.5
if ! kill -0 "$xvfb_pid" 2>/dev/null; then
  printf '%s\n' 'Xvfb exited before the nested session started' >&2
  exit 1
fi

gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell enabled-extensions "['desktop-computer-use@local']"
gnome-shell --x11 --sm-disable >"$RESULTS_DIR/shell.log" 2>&1 &
shell_pid=$!
shell_ready=false
for ((attempt = 1; attempt <= 80; attempt += 1)); do
  if gdbus introspect --session --dest org.desktopcomputeruse.Shell \
      --object-path /org/desktopcomputeruse/Shell >"$RESULTS_DIR/extension.xml" 2>/dev/null; then
    shell_ready=true
    break
  fi
  if ! kill -0 "$shell_pid" 2>/dev/null; then break; fi
  sleep 0.25
done
if [[ "$shell_ready" != true ]]; then
  printf '%s\n' 'GNOME X11 extension did not become available' >&2
  cat "$RESULTS_DIR/shell.log" >&2 || true
  exit 1
fi

"$PYTHON_BIN" /test/gtk-fixture.py "$RESULTS_DIR/fixture-events.ndjson" \
  >"$RESULTS_DIR/fixture.stdout" 2>"$RESULTS_DIR/fixture.stderr" &
fixture_pid=$!
if ! wait_for_file_line "$RESULTS_DIR/fixture-events.ndjson" '"event": "ready"' 100; then
  printf '%s\n' 'GTK fixture did not emit ready' >&2
  cat "$RESULTS_DIR/fixture.stderr" >&2 || true
  exit 1
fi

run_cli setup setup
run_cli doctor doctor
run_cli capabilities capabilities
run_cli session-start session start
"$PYTHON_BIN" /test/capture-root.py "$RESULTS_DIR/session-start-screen.png" \
  >"$RESULTS_DIR/capture-root.stdout" 2>"$RESULTS_DIR/capture-root.stderr" || true
run_cli list-apps list-apps
run_cli list-windows list-windows
WINDOW_ID="$("$PYTHON_BIN" /test/parse-window.py "$RESULTS_DIR/list-windows.json")"
if [[ -z "$WINDOW_ID" ]]; then
  printf '%s\n' 'Fixture window ID was empty' >&2
  exit 1
fi
printf '%s\n' "$WINDOW_ID" > "$RESULTS_DIR/window-id.txt"

# GNOME Shell can leave a newly discovered X11 client inactive while its
# native window activation request is still settling.  Focus the disposable
# fixture before the first input action so a missing press is attributable to
# the backend rather than to the test compositor's startup race.
fixture_x11_window="$(DISPLAY="$DISPLAY" xdotool search --name "DCU native drag fixture" 2>/dev/null | head -n 1 || true)"
if [[ -n "$fixture_x11_window" ]]; then
  DISPLAY="$DISPLAY" xdotool windowraise "$fixture_x11_window" \
    >"$RESULTS_DIR/focus-initial-raise.stdout" 2>"$RESULTS_DIR/focus-initial-raise.stderr" || true
  DISPLAY="$DISPLAY" xdotool windowfocus --sync "$fixture_x11_window" \
    >"$RESULTS_DIR/focus-initial.stdout" 2>"$RESULTS_DIR/focus-initial.stderr" || true
  DISPLAY="$DISPLAY" xdotool windowactivate --sync "$fixture_x11_window" \
    >"$RESULTS_DIR/focus-initial-activate.stdout" 2>"$RESULTS_DIR/focus-initial-activate.stderr" || true
  DISPLAY="$DISPLAY" xdotool mousemove --sync --window "$fixture_x11_window" 148 211 \
    >"$RESULTS_DIR/focus-initial-move.stdout" 2>"$RESULTS_DIR/focus-initial-move.stderr" || true
  printf '%s\n' "$fixture_x11_window" > "$RESULTS_DIR/focus-initial-window.txt"
else
  printf '%s\n' 'fixture window was not found by xdotool' > "$RESULTS_DIR/focus-initial.stderr"
fi
sleep 0.25

run_cli observe-state get-app-state --window-id "$WINDOW_ID" --observe screenshot --format jpeg --max-edge 1600
run_cli drag drag --window-id "$WINDOW_ID" \
  --from-x 148 --from-y 211 --to-x 548 --to-y 211 \
  --duration-ms 240 --steps 12 --hold-before-ms 50 --hold-after-ms 50 \
  --observe screenshot --format jpeg
run_cli observe-after get-app-state --window-id "$WINDOW_ID" --observe screenshot --format jpeg

# Observation paths point into the daemon runtime.  Copy the generated JPEGs
# while that runtime is alive so the result directory remains useful after the
# container exits, even though the JSON path is intentionally runtime-local.
observation_dir="${XDG_RUNTIME_DIR:-/tmp/dcu-runtime}/desktop-computer-use"
if [[ -d "$observation_dir" ]]; then
  find "$observation_dir" -maxdepth 1 -type f -name '*.jpg' \
    -exec cp -f {} "$RESULTS_DIR/" \; || true
fi

# Stop a second, deliberately long drag from a different client.  Keeping the
# same session removes a lease-renewal race from this cancellation probe. The
# daemon's urgent session.stop path must interrupt the active operation and
# release the held button before the drag client receives its cancelled response.
# X11 window activation is asynchronous under the bare Xvfb window manager.
# Focus the disposable fixture explicitly so the cancellation probe measures
# input release, while the earlier normal drag still exercises backend Activate.
fixture_x11_window="$(DISPLAY="$DISPLAY" xdotool search --name "DCU native drag fixture" 2>/dev/null | head -n 1 || true)"
if [[ -n "$fixture_x11_window" ]]; then
  DISPLAY="$DISPLAY" xdotool windowraise "$fixture_x11_window" \
    >"$RESULTS_DIR/focus-raise.stdout" 2>"$RESULTS_DIR/focus-raise.stderr" || true
  DISPLAY="$DISPLAY" xdotool windowfocus --sync "$fixture_x11_window" \
    >"$RESULTS_DIR/focus.stdout" 2>"$RESULTS_DIR/focus.stderr" || true
  DISPLAY="$DISPLAY" xdotool windowactivate --sync "$fixture_x11_window" \
    >"$RESULTS_DIR/focus-activate.stdout" 2>"$RESULTS_DIR/focus-activate.stderr" || true
  DISPLAY="$DISPLAY" xdotool mousemove --sync --window "$fixture_x11_window" 148 211 \
    >"$RESULTS_DIR/focus-move.stdout" 2>"$RESULTS_DIR/focus-move.stderr" || true
  printf '%s\n' "$fixture_x11_window" > "$RESULTS_DIR/focus-window.txt"
else
  printf '%s\n' 'fixture window was not found by xdotool' > "$RESULTS_DIR/focus.stderr"
fi
sleep 0.25
cancel_event_baseline="$(wc -l < "$RESULTS_DIR/fixture-events.ndjson")"
printf '%s\n' "$cancel_event_baseline" > "$RESULTS_DIR/cancel-event-baseline.txt"
cancel_drag_args=(
  drag --window-id "$WINDOW_ID"
  --from-x 148 --from-y 211 --to-x 548 --to-y 211
  --duration-ms 5000 --steps 100 --hold-before-ms 300 --hold-after-ms 50
)
{
  printf '%q ' "$NODE_BIN" "$DCU_CLI" "${cancel_drag_args[@]}"
  printf '\n'
} >> "$RESULTS_DIR/commands.log"
date -u '+%Y-%m-%dT%H:%M:%S.%3NZ' > "$RESULTS_DIR/cancel-start-time.txt"
set +e
"$NODE_BIN" "$DCU_CLI" "${cancel_drag_args[@]}" \
  >"$RESULTS_DIR/drag-cancel.json" 2>"$RESULTS_DIR/drag-cancel.stderr" &
drag_pid=$!
set -e
if ! wait_for_new_file_line "$RESULTS_DIR/fixture-events.ndjson" '"event": "down"' \
    "$cancel_event_baseline" 100; then
  # Keep the recovery sequence running even when the native backend reports
  # success before the fixture sees a press.  The validator can then retain
  # the daemon response, stop response, and fixture trace in one result set.
  printf '%s\n' 'Long drag did not reach the GTK fixture' \
    | tee "$RESULTS_DIR/cancel-wait-failure.txt" >&2
  printf '%s\n' 'down-not-observed-before-stop-timeout' > "$RESULTS_DIR/cancel-stop-reason.txt"
else
  printf '%s\n' 'down-observed' > "$RESULTS_DIR/cancel-stop-reason.txt"
fi
date -u '+%Y-%m-%dT%H:%M:%S.%3NZ' > "$RESULTS_DIR/cancel-stop-time.txt"
run_cli cancel-stop session stop
if wait "$drag_pid"; then
  drag_exit=0
else
  drag_exit=$?
fi
printf '%s\n' "$drag_exit" > "$RESULTS_DIR/drag-cancel.exit"
run_cli cancel-status session status

run_cli_retry_busy restart-session session start
run_cli final-stop session stop

"$PYTHON_BIN" /test/validate-results.py "$RESULTS_DIR"

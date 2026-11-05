#!/usr/bin/env bash
# Runs set-state, scroll-to, and select-option against the GTK semantic fixture
# in the same nested GNOME X11 session that run-x11.sh uses, then validates the
# fixture's own event log with validate-semantic.py.
set -Eeuo pipefail

RESULTS_DIR="${RESULTS_DIR:-/results}"
DISPLAY="${DISPLAY:-:99}"
PYTHON_BIN="${PYTHON_BIN:-/test/.venv/bin/python}"
NODE_BIN="${NODE_BIN:-/test/node/bin/node}"
DCU_CLI="${DCU_CLI:-/test/project/skills/desktop-computer-use/scripts/dcu.mjs}"
DCU_NATIVE_PATH="${DCU_NATIVE_PATH:-/test/native/desktop-computer-use-native}"
DCU_RUNTIME_DIR="${DCU_RUNTIME_DIR:-/tmp/dcu-runtime/linux-native-semantic}"
FIXTURE_TITLE="DCU semantic fixture"

export DISPLAY PYTHON_BIN NODE_BIN DCU_CLI DCU_NATIVE_PATH DCU_RUNTIME_DIR
export XDG_SESSION_TYPE=x11
export GDK_BACKEND=x11
unset WAYLAND_DISPLAY

mkdir -p "$RESULTS_DIR"
missing=()
[[ -x "$NODE_BIN" ]] || missing+=("node runtime: $NODE_BIN")
[[ -f "$DCU_CLI" ]] || missing+=("CLI bundle: $DCU_CLI")
[[ -x "$DCU_NATIVE_PATH" ]] || missing+=("native daemon: $DCU_NATIVE_PATH")
if (( ${#missing[@]} > 0 )); then
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
  "$NODE_BIN" "$DCU_CLI" session stop >"$RESULTS_DIR/cleanup-stop.json" 2>&1
  "$NODE_BIN" "$DCU_CLI" daemon shutdown >"$RESULTS_DIR/cleanup-shutdown.json" 2>&1
  for pid in "$fixture_pid" "$shell_pid" "$xvfb_pid"; do
    if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; fi
  done
  if [[ -f "$DCU_RUNTIME_DIR/daemon.log" ]]; then cp "$DCU_RUNTIME_DIR/daemon.log" "$RESULTS_DIR/daemon.log"; fi
}
trap cleanup EXIT INT TERM

# Records the uncompacted response and exit code under NAME and never fails the
# script, so expected errors reach the validator.
run_cli() {
  local name="$1"
  shift
  printf '%s\n' "$name: $*" >> "$RESULTS_DIR/commands.log"
  set +e
  "$NODE_BIN" "$DCU_CLI" "$@" --raw >"$RESULTS_DIR/$name.json" 2>"$RESULTS_DIR/$name.stderr"
  printf '%s\n' "$?" > "$RESULTS_DIR/$name.exit"
  set -e
}

# Observes the fixture again (indexes shift when a row expands) and sets
# OBSERVATION and INDEX for the element with ROLE and NAME.
target() {
  local name="$1" role="$2" element_name="$3" nth="${4:-0}"
  run_cli "$name-observe" get-app-state --window-id "$WINDOW_ID" --include-text
  read -r OBSERVATION INDEX < <("$PYTHON_BIN" /test/find-element.py \
    "$RESULTS_DIR/$name-observe.json" "$role" "$element_name" "$nth")
}

act() {
  local name="$1" role="$2" element_name="$3" nth="$4"
  shift 4
  target "$name" "$role" "$element_name" "$nth"
  run_cli "$name" "$@" --window-id "$WINDOW_ID" --observation-id "$OBSERVATION" --element-index "$INDEX"
  mark_events "$name"
}

# The fixture's event count after an action lets the validator attribute each
# event to the action that caused it.
mark_events() {
  sleep 0.3
  wc -l < "$RESULTS_DIR/fixture-events.ndjson" > "$RESULTS_DIR/$1.lines"
}

wait_for_ready() {
  for ((attempt = 1; attempt <= 100; attempt += 1)); do
    if grep -Fq '"event": "ready"' "$RESULTS_DIR/fixture-events.ndjson" 2>/dev/null; then return 0; fi
    sleep 0.05
  done
  return 1
}

Xvfb "$DISPLAY" -screen 0 1280x720x24 -ac +extension RANDR >"$RESULTS_DIR/xvfb.log" 2>&1 &
xvfb_pid=$!
sleep 0.5

gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell enabled-extensions "['desktop-computer-use@local']"
gnome-shell --x11 --sm-disable >"$RESULTS_DIR/shell.log" 2>&1 &
shell_pid=$!
shell_ready=false
for ((attempt = 1; attempt <= 80; attempt += 1)); do
  if gdbus introspect --session --dest org.desktopcomputeruse.Shell \
      --object-path /org/desktopcomputeruse/Shell >/dev/null 2>&1; then
    shell_ready=true
    break
  fi
  sleep 0.25
done
if [[ "$shell_ready" != true ]]; then
  printf '%s\n' 'GNOME X11 extension did not become available' >&2
  exit 1
fi

"$PYTHON_BIN" /test/semantic-fixture.py "$RESULTS_DIR/fixture-events.ndjson" \
  >"$RESULTS_DIR/fixture.stdout" 2>"$RESULTS_DIR/fixture.stderr" &
fixture_pid=$!
if ! wait_for_ready; then
  printf '%s\n' 'GTK semantic fixture did not emit ready' >&2
  exit 1
fi

run_cli capabilities capabilities
run_cli session-start session start
run_cli list-windows list-windows
WINDOW_ID="$("$PYTHON_BIN" /test/parse-window.py "$RESULTS_DIR/list-windows.json" "$FIXTURE_TITLE")"

# The first hard gate: without the fixture's AT-SPI tree nothing below means anything.
target accessibility-gate "check box" Agree
mark_events accessibility-gate

act check-on "check box" Agree 0 set-state --state checked
act check-again "check box" Agree 0 set-state --state checked
act expand "toggle button" Details 0 set-state --state expanded
act collapse "toggle button" Details 0 set-state --state collapsed
act select-row "list item" "Row 3" 0 set-state --state selected
# Row 10 lies wholly below the scroll area but inside the window, with real
# extents; Row 90 is not drawn yet, so GTK gives it no usable position.
act scroll-near "list item" "Row 10" 0 scroll-to
act scroll-far "list item" "Row 90" 0 scroll-to
# Kept for diagnosis: the scroll bars' values and the rows' extents after scrolling.
run_cli after-scroll get-app-state --window-id "$WINDOW_ID" --include-text
"$PYTHON_BIN" /test/probe-scroll-bars.py "$FIXTURE_TITLE" \
  >"$RESULTS_DIR/scroll-bar-probe.json" 2>"$RESULTS_DIR/scroll-bar-probe.stderr" || true
mark_events scroll-bar-probe
act option-row "list box" Rows 0 select-option --option "row 50"
act option-combo "combo box" - 0 select-option --option medium
act option-missing "combo box" - 0 select-option --option Huge
act option-ambiguous "combo box" - 1 select-option --option Same

run_cli final-stop session stop
"$PYTHON_BIN" /test/validate-semantic.py "$RESULTS_DIR"

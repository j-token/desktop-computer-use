#!/usr/bin/env bash
set -Eeuo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
harness_root="$repository_root/tests/integration/eis"
build_directory="${DCU_EIS_OUTPUT_DIR:-}"
if [[ -z "$build_directory" ]]; then
  build_directory="$(mktemp -d "${TMPDIR:-/tmp}/dcu-eis-build.XXXXXX")"
else
  mkdir -p "$build_directory"
fi

libei_include="${LIBEI_INCLUDE:-/usr/include/libei-1.0}"
libei_library_directory="${LIBEI_LIBRARY_DIR:-/usr/lib/x86_64-linux-gnu}"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

run_client() {
  local test_name="$1"
  DCU_EIS_TEST_SOCKET="$build_directory/$2.sock" \
    cargo test --manifest-path "$repository_root/Cargo.toml" --locked \
      -p dcu-linux --lib "eis_client::tests::$test_name" \
      -- --ignored --exact --nocapture
}

wait_for_socket() {
  local socket_path="$1"
  for _ in $(seq 1 50); do
    [[ -S "$socket_path" ]] && return 0
    sleep 0.1
  done
  printf 'Timed out waiting for %s\n' "$socket_path" >&2
  return 1
}

gcc -std=c11 -Wall -Wextra -Wpedantic \
  "$harness_root/eis_smoke_server.c" \
  -I"$libei_include" -L"$libei_library_directory" \
  -Wl,-rpath,"$libei_library_directory" -Wl,-l:libeis.so.1 \
  -o "$build_directory/eis-smoke-server"
rm -f "$build_directory/smoke.sock" "$build_directory/smoke.log"
"$build_directory/eis-smoke-server" "$build_directory/smoke.sock" \
  >"$build_directory/smoke.log" 2>&1 &
server_pid=$!
wait_for_socket "$build_directory/smoke.sock"
run_client mapped_input_releases_held_button_and_key smoke
wait "$server_pid"
server_pid=""
cat "$build_directory/smoke.log"
for expected in \
  'absolute 101.50 202.50' 'button 272 1' 'scroll 0.00 -3.00' \
  'key 30 1' 'button 272 0' 'key 30 0'; do
  grep -Fxq "$expected" "$build_directory/smoke.log"
done

gcc -std=c11 -Wall -Wextra -Wpedantic \
  "$harness_root/eis_multiregion_server.c" \
  -I"$libei_include" -L"$libei_library_directory" \
  -Wl,-rpath,"$libei_library_directory" -Wl,-l:libeis.so.1 \
  -o "$build_directory/eis-multiregion-server"
rm -f "$build_directory/multiregion.sock" "$build_directory/multiregion.log"
"$build_directory/eis-multiregion-server" "$build_directory/multiregion.sock" \
  >"$build_directory/multiregion.log" 2>&1 &
server_pid=$!
wait_for_socket "$build_directory/multiregion.sock"
run_client global_point_uses_second_region_without_mapping_ids multiregion
wait "$server_pid"
server_pid=""
cat "$build_directory/multiregion.log"
grep -Fxq 'absolute 901.50 202.50' "$build_directory/multiregion.log"

run_client stalled_handshake_observes_cancellation cancellation | tee "$build_directory/cancellation.log"

for symbol in \
  ei_new_sender ei_configure_name ei_setup_backend_fd ei_get_fd ei_dispatch \
  ei_get_event ei_event_unref ei_event_get_type ei_event_get_seat \
  ei_event_get_device ei_seat_ref ei_seat_unref ei_seat_has_capability \
  ei_seat_bind_capabilities ei_device_ref ei_device_unref \
  ei_device_has_capability ei_device_get_region ei_region_get_x \
  ei_region_get_y ei_region_get_width ei_region_get_height \
  ei_region_get_mapping_id ei_device_start_emulating \
  ei_device_stop_emulating ei_device_frame \
  ei_device_pointer_motion_absolute ei_device_button_button \
  ei_device_scroll_delta ei_device_keyboard_key ei_now ei_device_close ei_unref; do
  nm -D --defined-only "$libei_library_directory/libei.so.1" | grep -q " $symbol$"
done
printf '%s\n' 'all-required-libei-symbols-present'
printf 'EIS integration output: %s\n' "$build_directory"

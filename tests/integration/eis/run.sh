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
g++ -std=c++20 -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror \
  "$repository_root/native/linux/eis_client.cpp" \
  "$harness_root/eis_smoke_client.cpp" \
  -I"$harness_root/stubs" -I"$repository_root/native/linux" \
  -ldl -o "$build_directory/eis-smoke-client"
rm -f "$build_directory/smoke.sock" "$build_directory/smoke.log"
"$build_directory/eis-smoke-server" "$build_directory/smoke.sock" \
  >"$build_directory/smoke.log" 2>&1 &
server_pid=$!
wait_for_socket "$build_directory/smoke.sock"
"$build_directory/eis-smoke-client" "$build_directory/smoke.sock"
wait "$server_pid"
server_pid=""
cat "$build_directory/smoke.log"

gcc -std=c11 -Wall -Wextra -Wpedantic \
  "$harness_root/eis_multiregion_server.c" \
  -I"$libei_include" -L"$libei_library_directory" \
  -Wl,-rpath,"$libei_library_directory" -Wl,-l:libeis.so.1 \
  -o "$build_directory/eis-multiregion-server"
g++ -std=c++20 -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror \
  "$repository_root/native/linux/eis_client.cpp" \
  "$harness_root/eis_multiregion_client.cpp" \
  -I"$harness_root/stubs" -I"$repository_root/native/linux" \
  -ldl -o "$build_directory/eis-multiregion-client"
rm -f "$build_directory/multiregion.sock" "$build_directory/multiregion.log"
"$build_directory/eis-multiregion-server" "$build_directory/multiregion.sock" \
  >"$build_directory/multiregion.log" 2>&1 &
server_pid=$!
wait_for_socket "$build_directory/multiregion.sock"
"$build_directory/eis-multiregion-client" "$build_directory/multiregion.sock"
wait "$server_pid"
server_pid=""
cat "$build_directory/multiregion.log"

g++ -std=c++20 -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror \
  "$repository_root/native/linux/eis_client.cpp" \
  "$harness_root/eis_cancel_client.cpp" \
  -I"$harness_root/stubs" -I"$repository_root/native/linux" \
  -ldl -pthread -o "$build_directory/eis-cancel-client"
"$build_directory/eis-cancel-client" | tee "$build_directory/cancellation.log"

g++ -std=c++20 -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror \
  -fsanitize=address,undefined -fno-omit-frame-pointer \
  "$repository_root/native/linux/eis_client.cpp" \
  "$harness_root/eis_multiregion_client.cpp" \
  -I"$harness_root/stubs" -I"$repository_root/native/linux" \
  -ldl -o "$build_directory/eis-multiregion-client-asan"
rm -f "$build_directory/multiregion-asan.sock" \
  "$build_directory/asan-multiregion.log"
"$build_directory/eis-multiregion-server" \
  "$build_directory/multiregion-asan.sock" \
  >"$build_directory/asan-multiregion.log" 2>&1 &
server_pid=$!
wait_for_socket "$build_directory/multiregion-asan.sock"
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 \
  "$build_directory/eis-multiregion-client-asan" \
  "$build_directory/multiregion-asan.sock"
wait "$server_pid"
server_pid=""
cat "$build_directory/asan-multiregion.log"

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
ldd "$build_directory/eis-smoke-client" | grep -E 'libei|libeis' || \
  printf '%s\n' 'no-libei-linkage'
readelf -d "$build_directory/eis-smoke-client" | grep NEEDED
printf 'EIS integration output: %s\n' "$build_directory"

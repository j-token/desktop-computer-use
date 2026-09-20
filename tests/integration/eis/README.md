# Real libei/EIS transport harness

This directory contains the standalone EIS server/client checks used for the
Linux transport. The server uses the public `libeis` API and the client links
only to `libdl`; `libei.so.1` is loaded by `native/linux/eis_client.cpp` at
runtime.

The primary harness exercises the complete sender sequence: connect, seat
capability binding, device-added/device-resumed handling, emulation start,
mapped absolute motion, button, smooth scroll, keyboard input, frame
timestamps, and release of held input. The multi-region harness creates two
regions without mapping IDs and checks `absolute_global()` against the second
region. The cancellation harness checks that a stalled handshake observes the
shared `Context` cancellation.

These checks use a local Unix socket and a real `libeis.so.1` server. They do
not claim that the GNOME RemoteDesktop portal granted permission or that a
graphical session was controlled. Run the portal and GNOME integration checks
separately in the logged-in session.

## Prerequisites

Run from the repository root on Ubuntu 22.04 or newer x86_64:

* `build-essential` and `libei1`/`libeis1` runtime packages;
* `libei-dev` for the test server header and linker library;
* the extracted or installed public header directory, normally
  `/usr/include/libei-1.0`.

The validated Ubuntu host had Ubuntu 24.04.4, GNOME Shell 46.0, GCC 13.3,
and `/usr/lib/x86_64-linux-gnu/libei.so.1.2.1` and
`/usr/lib/x86_64-linux-gnu/libeis.so.1.2.1`. Its development header was
extracted under `/tmp/eis-dev.*/root/usr/include/libei-1.0` because installing
packages with `sudo` was unavailable.

## Reproduce

Set `LIBEI_INCLUDE` to the directory containing `libei.h`, then run:

~~~text
bash tests/integration/eis/run.sh
~~~

The script compiles into a new directory below `${TMPDIR:-/tmp}` and prints
that directory when it finishes, so no binaries or logs are written to the
repository. Set `DCU_EIS_OUTPUT_DIR` to choose another external output
directory. The minimal test-only context is in `stubs/dcu/backend.hpp`; the
production build uses `native/include/dcu/backend.hpp`.

The output directory contains:

* `smoke.log` — mapped absolute point, button, scroll, key, and release;
* `multiregion.log` — global point routed to the second region without IDs;
* `cancellation.log` — cancellation during a quiet handshake;
* `asan-multiregion.log` — sanitized global-region run.

The symbol and linkage checks are printed to standard output. Redirect the
script output if those checks also need to be retained for a test run.

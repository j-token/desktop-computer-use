# Isolated Linux native X11 harness

This directory adds a derived test image and runner for the Linux backend. It
does not change `tests/integration/gnome-container/Dockerfile` or its Wayland
runner.
The test starts Xvfb, launches GNOME Shell with the X11 backend as its window
manager, enables the already packaged extension, and starts the existing GTK
native fixture inside that isolated desktop. It never mounts the host display,
host D-Bus, or input devices.

Build the unchanged base image first from the repository root:

~~~text
docker build -t dcu-gnome42-test -f tests/integration/gnome-container/Dockerfile .
~~~

Then build the derived image:

~~~text
docker build --build-arg BASE_IMAGE=dcu-gnome42-test \
  -t dcu-linux-native-x11 -f tests/integration/linux-native/Dockerfile .
~~~

Run it on an x86-64 Linux host with a Linux native daemon and Node runtime
mounted into the container. The CLI bundle can be mounted from the repository;
the Node tree should be a Linux x64 distribution such as Node 24.

~~~text
DCU_RESULTS_DIR="${TMPDIR:-/tmp}/dcu-linux-native-results"
mkdir -p "$DCU_RESULTS_DIR"
docker run --rm --network none \
  --mount type=bind,src="$PWD",dst=/test/project,readonly \
  --mount type=bind,src="/absolute/path/desktop-computer-use-native",dst=/test/native/desktop-computer-use-native,readonly \
  --mount type=bind,src="/absolute/path/node-v24.18.0-linux-x64",dst=/test/node,readonly \
  --mount type=bind,src="$DCU_RESULTS_DIR",dst=/results \
  -e DCU_NATIVE_PATH=/test/native/desktop-computer-use-native \
  -e NODE_BIN=/test/node/bin/node \
  -e DCU_CLI=/test/project/skills/desktop-computer-use/scripts/dcu.mjs \
  dcu-linux-native-x11
~~~

The runner exits 0 only when the CLI sequence and fixture evidence pass. It
records every JSON response and exit code, the GNOME/Xvfb/fixture logs, the
indicator capture, and `summary.json` below the mounted external results
directory (`$DCU_RESULTS_DIR` in the example):

- doctor, capabilities, session start, list-apps, and list-windows exercise the
  CLI and native daemon in the nested X11 session.
- get-app-state with screenshot observation checks the default screenshot route,
  observation ID, JPEG metadata, and banner/cursor overlay regions.
- The normal drag uses 12 individually paced moves over 240 ms with 50 ms holds
  and checks the GTK fixture held-button events, displacement, duration, and
  drag-complete record.
- A second 5-second drag in the same session is interrupted by a separate
  session stop client; the report requires a cancelled drag response, a prompt
  button-up, and an inactive session before restarting and stopping cleanly.

If the Node or native mount is absent, the runner exits 77 before starting Xvfb
and writes preflight.json with the missing path. This is an explicit runtime
limitation, not a passing desktop test. A prebuilt daemon is required; the
image does not compile C++ or install a development SDK.

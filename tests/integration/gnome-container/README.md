# Isolated GNOME extension runtime checks

From the repository root, build `docker build -t dcu-gnome42-test -f tests/integration/gnome-container/Dockerfile .` and run `docker run --rm dcu-gnome42-test`.
For GNOME 46, add `--build-arg UBUNTU_VERSION=24.04 --build-arg EXTENSION_VARIANT=modern` and use a separate image name.

This starts an actual nested GNOME Wayland compositor in an Xvfb display, with independent system/session D-Bus services. It mounts no host display, D-Bus socket, or device. GNOME uses its built-in fallback for environments without logind; package-created empty `/run/systemd/seats` is removed inside the image. This is extension runtime validation, not a full logged-in desktop or portal permission test.

The test loads the extension, opens and activates a native GTK fixture, and checks Start/Heartbeat/ListWindows/GetOverlayRegions/Stop. It focuses the nested compositor and checks that a single Escape and Escapes sent during `SuspendStopKey` keep the session alive, while two Escapes within one second (the second with Shift held) make Heartbeat false. A separate restart verifies the five-second watchdog after seven seconds without a heartbeat. Mount a writable output directory at `/results` to capture the banner, blue gradient border, and cursor marker. Review JSON readiness and false heartbeats, not just the shell exit status. Missing system services produce expected nested-session warnings; extension JS errors are failures.

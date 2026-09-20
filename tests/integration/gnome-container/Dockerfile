ARG UBUNTU_VERSION=22.04
FROM ubuntu:${UBUNTU_VERSION}
ARG EXTENSION_VARIANT=legacy
COPY --from=ghcr.io/astral-sh/uv:0.8.22 /uv /usr/local/bin/uv
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends gnome-shell dbus-x11 xvfb xauth mesa-utils libgl1-mesa-dri gnome-session-bin python3 gir1.2-gtk-3.0 && rm -rf /var/lib/apt/lists/*
RUN if getent passwd 1000 >/dev/null; then usermod -l tester -d /home/tester -m "$(getent passwd 1000 | cut -d: -f1)" && groupadd -f tester; else useradd -m -u 1000 tester; fi
RUN apt-get update && apt-get install -y --no-install-recommends python3-gi xwayland adwaita-icon-theme fonts-noto-cjk && rm -rf /var/lib/apt/lists/*
# This isolated container has no systemd login session. Remove the empty marker
# installed by package scripts so GNOME uses its built-in nested-session fallback.
RUN rmdir /run/systemd/seats 2>/dev/null || true
COPY extensions/gnome-${EXTENSION_VARIANT} /home/tester/.local/share/gnome-shell/extensions/desktop-computer-use@local
RUN glib-compile-schemas /home/tester/.local/share/gnome-shell/extensions/desktop-computer-use@local/schemas && chown -R tester:tester /home/tester
COPY tests/integration/gnome-container/run.sh /test/run.sh
COPY tests/integration/gnome-container/select-window.py /test/select-window.py
COPY tests/integration/gtk-fixture.py /test/gtk-fixture.py
RUN uv venv --python /usr/bin/python3 --system-site-packages /test/.venv
RUN apt-get update && apt-get install -y --no-install-recommends xdotool && rm -rf /var/lib/apt/lists/*
RUN chmod +x /test/run.sh
ENV HOME=/home/tester XDG_RUNTIME_DIR=/tmp/dcu-runtime DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1
ENTRYPOINT ["bash", "-c", "mkdir -p /run/dbus /tmp/dcu-runtime && chown tester:tester /tmp/dcu-runtime && chmod 700 /tmp/dcu-runtime && dbus-daemon --system --fork && exec runuser -u tester -- dbus-run-session -- bash /test/run.sh"]

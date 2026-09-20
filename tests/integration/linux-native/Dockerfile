ARG BASE_IMAGE=dcu-gnome42-test
FROM ${BASE_IMAGE}

USER root

# The GNOME validation image supplies the compositor, GTK fixture, D-Bus, and
# XTest command line tools.  Add only runtime libraries needed by the mounted
# native daemon; the daemon itself is deliberately built outside this image.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        libxtst6 libxrandr2 libjpeg-turbo8 libatspi2.0-0 \
        libpipewire-0.3-0 libpng16-16 || \
    apt-get install -y --no-install-recommends \
        libxtst6 libxrandr2 libjpeg-turbo8 libatspi2.0-0 \
        libpipewire-0.3-0t64 libpng16-16t64
RUN rm -rf /var/lib/apt/lists/*

COPY tests/integration/linux-native/run-x11.sh /test/run-x11.sh
COPY tests/integration/linux-native/parse-window.py /test/parse-window.py
COPY tests/integration/linux-native/capture-root.py /test/capture-root.py
COPY tests/integration/linux-native/validate-results.py /test/validate-results.py
RUN chmod 0755 /test/run-x11.sh

ENV HOME=/home/tester \
    XDG_RUNTIME_DIR=/tmp/dcu-runtime \
    DISPLAY=:99 \
    XDG_SESSION_TYPE=x11 \
    GDK_BACKEND=x11 \
    LIBGL_ALWAYS_SOFTWARE=1

ENTRYPOINT ["bash", "-c", "mkdir -p /run/dbus /tmp/dcu-runtime && chown tester:tester /tmp/dcu-runtime && chmod 700 /tmp/dcu-runtime && dbus-daemon --system --fork && exec runuser -u tester -- dbus-run-session -- bash /test/run-x11.sh"]

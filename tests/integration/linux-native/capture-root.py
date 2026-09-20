"""Capture the nested X11 root so an indicator image is retained as evidence."""

import sys

import gi

gi.require_version("Gdk", "3.0")
from gi.repository import Gdk


def capture(path: str) -> None:
    root = Gdk.get_default_root_window()
    if root is None:
        raise RuntimeError("GDK did not expose an X11 root window")
    width = root.get_width()
    height = root.get_height()
    if width <= 0 or height <= 0:
        raise RuntimeError(f"invalid root geometry: {width}x{height}")
    pixbuf = Gdk.pixbuf_get_from_window(root, 0, 0, width, height)
    if pixbuf is None:
        raise RuntimeError("GDK could not capture the X11 root")
    pixbuf.savev(path, "png", [], [])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: capture-root.py OUTPUT-PNG")
    capture(sys.argv[1])

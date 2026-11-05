"""Print what the semantic fixture's scroll bars expose over AT-SPI as JSON.

Diagnostic for scroll-to: states, Value range, and whether setting the
current value moves the bar, with any D-Bus error.
"""

import json
import sys

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib


def find(node, wanted_role, found):
    if node is None:
        return
    if node.get_role() == wanted_role:
        found.append(node)
    for index in range(node.get_child_count()):
        find(node.get_child_at_index(index), wanted_role, found)


def describe(bar):
    states = [state.value_nick for state in bar.get_state_set().get_states()]
    report = {"states": states, "interfaces": bar.get_interfaces()}
    value = bar.get_value_iface()
    if value is None:
        return report
    before = value.get_current_value()
    report.update(minimum=value.get_minimum_value(), maximum=value.get_maximum_value(), current=before)
    try:
        report["setReturned"] = value.set_current_value(before + 100)
    except GLib.Error as error:
        report["setError"] = error.message
    report["after"] = value.get_current_value()
    return report


def fixture_window(title):
    desktop = Atspi.get_desktop(0)
    for app_index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(app_index)
        for window_index in range(app.get_child_count() if app else 0):
            window = app.get_child_at_index(window_index)
            if window and window.get_name() == title:
                return window
    raise RuntimeError(f"No AT-SPI window named {title!r}")


if __name__ == "__main__":
    bars = []
    find(fixture_window(sys.argv[1]), Atspi.Role.SCROLL_BAR, bars)
    print(json.dumps([describe(bar) for bar in bars], indent=2))

#!/usr/bin/env python3
"""GTK3 fixture for set-state, scroll-to, and select-option.

Every change the daemon makes is written by GTK itself as one NDJSON event, so
the validator checks the application's view rather than the daemon's readback.
"""
import json
import sys
import time

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk

TITLE = "DCU semantic fixture"
ROW_COUNT = 100

output = open(sys.argv[1], "w", encoding="utf-8", buffering=1)
origin = time.monotonic_ns()


def record(event, **fields):
    micros = (time.monotonic_ns() - origin) // 1000
    output.write(json.dumps(dict(event=event, us=micros, **fields)) + "\n")


def combo(name, items, active):
    box = Gtk.ComboBoxText()
    for item in items:
        box.append_text(item)
    box.set_active(active)
    box.connect("changed", lambda widget: record("combo", combo=name, active=widget.get_active_text()))
    return box


window = Gtk.Window(title=TITLE)
window.set_default_size(560, 600)
layout = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin=12)

check = Gtk.CheckButton(label="Agree")
check.connect("toggled", lambda widget: record("toggled", active=widget.get_active()))
layout.pack_start(check, False, False, 0)

expander = Gtk.Expander(label="Details")
expander.add(Gtk.Label(label="Hidden details"))
expander.connect("notify::expanded", lambda widget, _: record("expanded", expanded=widget.get_expanded()))
layout.pack_start(expander, False, False, 0)

rows = Gtk.ListBox()
rows.set_selection_mode(Gtk.SelectionMode.SINGLE)
for number in range(1, ROW_COUNT + 1):
    row = Gtk.ListBoxRow()
    row.add(Gtk.Label(label=f"Row {number}", xalign=0))
    row.get_accessible().set_name(f"Row {number}")
    rows.add(row)
rows.get_accessible().set_name("Rows")
rows.connect(
    "row-selected",
    lambda _, row: record("row-selected", row=row.get_accessible().get_name() if row else None),
)
scroller = Gtk.ScrolledWindow()
scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.ALWAYS)
scroller.set_size_request(-1, 160)
scroller.add(rows)
scroller.get_vadjustment().connect("value-changed", lambda adjustment: record("scrolled", value=adjustment.get_value()))
layout.pack_start(scroller, False, False, 0)

layout.pack_start(combo("size", ["Small", "Medium", "Large"], 0), False, False, 0)
layout.pack_start(combo("duplicate", ["Same", "Same", "Other"], 2), False, False, 0)

window.add(layout)
window.connect("destroy", Gtk.main_quit)
window.show_all()
record("ready")
Gtk.main()

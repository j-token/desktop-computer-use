#!/usr/bin/env python3
"""Disposable GTK3 native event fixture; host runtime dependencies only."""
import json
import math
import sys
import time
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, Gdk

output = open(sys.argv[1], "w", encoding="utf-8", buffering=1)
origin = time.monotonic_ns()
held = False
start = (0, 0)
pointer = (0, 0)
moves = completed = 0

def record(event, x, y, left):
    global held, start, pointer, moves, completed
    micros = (time.monotonic_ns() - origin) // 1000
    output.write(json.dumps(dict(event=event, us=micros, x=x, y=y, left=left)) + "\n")
    pointer = (x, y)
    if event == "down":
        held, start, moves = True, pointer, 0
    elif event == "move" and held and left:
        moves += 1
    elif event == "up":
        if held and moves >= 2 and math.hypot(x-start[0], y-start[1]) > 40:
            completed += 1
            output.write(json.dumps(dict(event="drag-complete", us=micros, moves=moves, count=completed)) + "\n")
        held = False

def update():
    status.set_text(f"Completed drags: {completed} | held: {held} | moves: {moves}")

def button(widget, event):
    if event.button == 1:
        pressed = event.type == Gdk.EventType.BUTTON_PRESS
        record("down" if pressed else "up", event.x, event.y, pressed)
        update()
    return True

def motion(widget, event):
    record("move", event.x, event.y, bool(event.state & Gdk.ModifierType.BUTTON1_MASK))
    update()
    return True

def close(widget):
    record("closed", *pointer, held)
    Gtk.main_quit()

window = Gtk.Window(title="DCU native drag fixture")
window.set_default_size(740, 440)
area = Gtk.EventBox()
area.set_above_child(True)
area.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK | Gdk.EventMask.POINTER_MOTION_MASK)
area.connect("button-press-event", button)
area.connect("button-release-event", button)
area.connect("motion-notify-event", motion)
layout = Gtk.Fixed()
title = Gtk.Label(label="DCU native drag fixture: drag blue square to green square")
layout.put(title, 20, 20)
style = Gtk.CssProvider()
style.load_from_data(b"#blue {background: #2d6ee6;} #green {background: #23b469;} #fixture {background: white; color: black;}")
Gtk.StyleContext.add_provider_for_screen(Gdk.Screen.get_default(), style, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
area.set_name("fixture")
for x, name in [(80, "blue"), (480, "green")]:
    square = Gtk.Label(label=name)
    square.set_name(name)
    square.set_size_request(120, 120)
    layout.put(square, x, 120)
status = Gtk.Label(label="Completed drags: 0")
layout.put(status, 20, 320)
area.add(layout)
window.add(area)
window.connect("destroy", close)
window.show_all()
record("ready", 0, 0, False)
Gtk.main()

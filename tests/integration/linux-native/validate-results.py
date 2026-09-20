"""Validate the isolated X11 CLI run from its JSON and GTK event evidence."""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any


def load_json(results: Path, name: str, failures: list[str]) -> dict[str, Any] | None:
    path = results / f"{name}.json"
    try:
        with path.open(encoding="utf-8") as stream:
            value = json.load(stream)
    except Exception as error:  # noqa: BLE001 - preserve the concrete file failure.
        failures.append(f"{name}: cannot read JSON ({error})")
        return None
    if not isinstance(value, dict):
        failures.append(f"{name}: response is not an object")
        return None
    return value


def response_ok(response: dict[str, Any] | None, name: str, failures: list[str]) -> None:
    if response is None:
        return
    if response.get("ok") is not True:
        failures.append(f"{name}: expected ok=true, got {response.get('error')}")


def response_exit(results: Path, name: str, failures: list[str], expected: int = 0) -> None:
    path = results / f"{name}.exit"
    try:
        code = int(path.read_text(encoding="utf-8").strip())
    except Exception as error:  # noqa: BLE001
        failures.append(f"{name}: cannot read exit code ({error})")
        return
    if code != expected:
        failures.append(f"{name}: expected exit {expected}, got {code}")


def read_events(results: Path, failures: list[str]) -> list[dict[str, Any]]:
    path = results / "fixture-events.ndjson"
    events: list[dict[str, Any]] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except Exception as error:  # noqa: BLE001
        failures.append(f"fixture-events: cannot read NDJSON ({error})")
        return events
    for line_number, line in enumerate(lines, 1):
        try:
            event = json.loads(line)
        except Exception as error:  # noqa: BLE001
            failures.append(f"fixture-events:{line_number}: invalid JSON ({error})")
            continue
        if isinstance(event, dict):
            events.append(event)
        else:
            failures.append(f"fixture-events:{line_number}: record is not an object")
    return events


def read_event_baseline(results: Path, event_count: int, failures: list[str]) -> int:
    path = results / "cancel-event-baseline.txt"
    try:
        baseline = int(path.read_text(encoding="utf-8").strip())
    except Exception as error:  # noqa: BLE001
        failures.append(f"cancel-event-baseline: cannot read count ({error})")
        return 0
    if baseline < 0 or baseline > event_count:
        failures.append(
            f"cancel-event-baseline: count {baseline} is outside event range 0..{event_count}"
        )
        return min(max(baseline, 0), event_count)
    return baseline


def drag_records(events: list[dict[str, Any]], failures: list[str]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    for event in events:
        kind = event.get("event")
        if kind == "down":
            if current is not None:
                failures.append("fixture-events: second down arrived before the prior up")
            current = {"down": event, "moves": []}
        elif kind == "move" and current is not None and event.get("left") is True:
            current["moves"].append(event)
        elif kind == "up" and current is not None:
            down = current["down"]
            moves = current["moves"]
            try:
                duration_ms = (float(event["us"]) - float(down["us"])) / 1000.0
                displacement = math.hypot(
                    float(event["x"]) - float(down["x"]),
                    float(event["y"]) - float(down["y"]),
                )
            except (KeyError, TypeError, ValueError) as error:
                failures.append(f"fixture-events: malformed drag record ({error})")
                current = None
                continue
            records.append(
                {
                    "down": down,
                    "up": event,
                    "moves": moves,
                    "heldMoves": len(moves),
                    "durationMs": duration_ms,
                    "displacement": displacement,
                }
            )
            current = None
    if current is not None:
        failures.append("fixture-events: final button-down has no button-up")
    return records


def main(results: Path) -> int:
    failures: list[str] = []

    required_commands = [
        "setup",
        "doctor",
        "capabilities",
        "session-start",
        "list-apps",
        "list-windows",
        "observe-state",
        "drag",
        "observe-after",
        "cancel-stop",
        "cancel-status",
        "restart-session",
        "final-stop",
    ]
    responses = {name: load_json(results, name, failures) for name in required_commands}
    for name in required_commands:
        response_ok(responses[name], name, failures)
        response_exit(results, name, failures)
    response_exit(results, "drag-cancel", failures, expected=1)

    list_windows = responses["list-windows"]
    fixture_window = None
    if list_windows and list_windows.get("ok"):
        windows = list_windows.get("result", {}).get("windows", [])
        fixture_window = next(
            (window for window in windows if window.get("title") == "DCU native drag fixture"),
            None,
        )
    if fixture_window is None:
        failures.append("list-windows: fixture title was not returned")

    observe = responses["observe-state"]
    observation: dict[str, Any] | None = None
    if observe and observe.get("ok"):
        result = observe.get("result", {})
        # get-app-state returns the observation as its result object.  Keep
        # accepting a nested object as well because action responses (such as
        # drag --observe) wrap it under result.observation.
        candidate = result.get("observation") if isinstance(result, dict) else None
        if not isinstance(candidate, dict) and isinstance(result, dict):
            candidate = result
        if isinstance(candidate, dict):
            observation = candidate
        else:
            failures.append("observe-state: missing observation object")
    if observation is not None:
        if not isinstance(observation.get("observationId"), str) or not observation["observationId"]:
            failures.append("observe-state: missing observationId")
        screenshot = observation.get("screenshot")
        if not isinstance(screenshot, dict):
            failures.append("observe-state: missing screenshot metadata")
        else:
            screenshot_path = screenshot.get("path")
            screenshot_file = Path(screenshot_path) if isinstance(screenshot_path, str) else None
            if screenshot_file is not None and not screenshot_file.is_file():
                # The runner copies runtime-local frames beside the JSON before
                # teardown, allowing an archived result to be revalidated.
                screenshot_file = results / screenshot_file.name
            if screenshot_file is None or not screenshot_file.is_file():
                failures.append(f"observe-state: screenshot path is not a file ({screenshot_path!r})")
            if screenshot.get("mimeType") != "image/jpeg":
                failures.append(f"observe-state: expected JPEG, got {screenshot.get('mimeType')!r}")
        overlays = observation.get("overlayRegions")
        kinds = (
            {item.get("kind") for item in overlays if isinstance(item, dict)}
            if isinstance(overlays, list)
            else set()
        )
        for required_kind in ("banner", "cursor"):
            if required_kind not in kinds:
                failures.append(f"observe-state: overlay region {required_kind!r} is absent")

    if not (results / "session-start-screen.png").is_file():
        failures.append("session-start-screen.png: indicator capture was not recorded")

    capabilities = responses["capabilities"]
    if capabilities and capabilities.get("ok"):
        values = capabilities.get("result", {})
        if values.get("sessionType") != "x11":
            failures.append(f"capabilities: expected sessionType=x11, got {values.get('sessionType')!r}")
        if values.get("input", {}).get("drag") is not True:
            failures.append("capabilities: drag is not advertised")

    drag = responses["drag"]
    if drag and drag.get("ok"):
        if drag.get("result", {}).get("delivered") is not True:
            failures.append("drag: delivered=true was not returned")
        if not isinstance(drag.get("result", {}).get("observation"), dict):
            failures.append("drag: requested screenshot observation was not returned")

    cancel_stop = responses["cancel-stop"]
    if cancel_stop and cancel_stop.get("ok") and cancel_stop.get("result", {}).get("stopped") is not True:
        failures.append("cancel-stop: stopped=true was not returned")
    cancel_status = responses["cancel-status"]
    if cancel_status and cancel_status.get("ok") and cancel_status.get("result", {}).get("active") is not False:
        failures.append("cancel-status: session remained active")

    cancel_drag = load_json(results, "drag-cancel", failures)
    if cancel_drag and cancel_drag.get("ok") is not False:
        failures.append("drag-cancel: expected a cancelled action response")
    if cancel_drag and cancel_drag.get("error", {}).get("code") != "cancelled":
        failures.append(f"drag-cancel: expected error code cancelled, got {cancel_drag.get('error')}")

    events = read_events(results, failures)
    event_baseline = read_event_baseline(results, len(events), failures)
    normal_records = drag_records(events[:event_baseline], failures)
    cancellation_records = drag_records(events[event_baseline:], failures)
    records = normal_records + cancellation_records
    completed = sum(1 for event in events if event.get("event") == "drag-complete")
    if not normal_records:
        failures.append("fixture-events: no released button sequence was recorded")
    else:
        normal_drag = normal_records[0]
        if normal_drag["heldMoves"] < 2:
            failures.append(f"fixture-events: normal drag had only {normal_drag['heldMoves']} held moves")
        if normal_drag["displacement"] <= 40:
            failures.append(f"fixture-events: normal drag displacement was {normal_drag['displacement']:.1f}")
        if normal_drag["durationMs"] < 150:
            failures.append(f"fixture-events: normal drag lasted only {normal_drag['durationMs']:.1f} ms")
        if completed < 1:
            failures.append("fixture-events: GTK fixture did not report drag-complete")
    if not cancellation_records:
        failures.append("fixture-events: cancellation did not produce a released second drag")
    elif cancellation_records[0]["durationMs"] >= 3000:
        failures.append(
            f"fixture-events: cancellation release took {cancellation_records[0]['durationMs']:.1f} ms"
        )

    summary = {
        "status": "passed" if not failures else "failed",
        "requiredCommands": required_commands,
        "window": fixture_window,
        "observationId": observation.get("observationId") if observation else None,
        "dragRecords": [
            {
                "durationMs": record["durationMs"],
                "heldMoves": record["heldMoves"],
                "displacement": record["displacement"],
            }
            for record in records
        ],
        "completedEvents": completed,
        "failures": failures,
    }
    (results / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 1 if failures else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: validate-results.py RESULTS-DIRECTORY")
    raise SystemExit(main(Path(sys.argv[1])))

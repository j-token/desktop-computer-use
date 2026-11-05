"""Check run-semantic.sh results against the GTK fixture's own event log.

Each action's events are the fixture lines written between the previous
action's mark and its own, so a pass means GTK saw the change, not only that
the daemon read its own AT-SPI state back.
"""

import json
import sys
from pathlib import Path

GATE = "accessibility-gate"

# Action name, expected error code (None for success), expected result fields,
# and a check over the action's fixture events.
EXPECTATIONS = [
    ("check-on", None, {"changed": True}, lambda events: {"event": "toggled", "active": True} in events),
    ("check-again", None, {"changed": False}, lambda events: not any(e["event"] == "toggled" for e in events)),
    ("expand", None, {"changed": True}, lambda events: {"event": "expanded", "expanded": True} in events),
    ("collapse", None, {"changed": True}, lambda events: {"event": "expanded", "expanded": False} in events),
    ("select-row", None, {"changed": True}, lambda events: {"event": "row-selected", "row": "Row 3"} in events),
    ("scroll-near", None, {"visible": True}, lambda events: any(e["event"] == "scrolled" for e in events)),
    ("scroll-far", None, {"visible": True}, lambda events: any(e["event"] == "scrolled" for e in events)),
    ("option-row", None, {"changed": True}, lambda events: {"event": "row-selected", "row": "Row 50"} in events),
    ("option-combo", None, {"changed": True},
     lambda events: {"event": "combo", "combo": "size", "active": "Medium"} in events),
    ("option-missing", "option_not_found", {}, lambda events: events == []),
    ("option-ambiguous", "ambiguous_target", {}, lambda events: events == []),
]


def load_events(results: Path) -> list[dict]:
    events = []
    for line in (results / "fixture-events.ndjson").read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        event.pop("us", None)
        events.append(event)
    return events


def mark(results: Path, name: str) -> int:
    return int((results / f"{name}.lines").read_text(encoding="utf-8").strip())


def check(results: Path) -> dict:
    events = load_events(results)
    failures = []
    passed = []
    previous = mark(results, GATE)
    for name, error_code, fields, events_match in EXPECTATIONS:
        response = json.loads((results / f"{name}.json").read_text(encoding="utf-8"))
        current = mark(results, name)
        own_events = events[previous:current]
        previous = current
        problems = []
        if error_code is None:
            if not response.get("ok"):
                problems.append(f"failed with {response.get('error')}")
            for key, value in fields.items():
                if response.get("result", {}).get(key) != value:
                    problems.append(f"result {key} is {response.get('result', {}).get(key)!r}, expected {value!r}")
        elif response.get("ok") or response.get("error", {}).get("code") != error_code:
            problems.append(f"expected error {error_code}, got {response.get('error') or 'success'}")
        if not events_match(own_events):
            problems.append(f"fixture events did not match: {own_events}")
        if problems:
            failures.append({"action": name, "problems": problems, "result": response.get("result")})
        else:
            passed.append(name)
    return {"status": "passed" if not failures else "failed", "passed": passed, "failures": failures}


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: validate-semantic.py RESULTS-DIR")
    results_dir = Path(sys.argv[1])
    summary = check(results_dir)
    (results_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    raise SystemExit(0 if summary["status"] == "passed" else 1)

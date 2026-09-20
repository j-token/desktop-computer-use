"""Print the stable ID of the GTK fixture from a CLI list-windows response."""

import json
import sys


def fixture_window_id(path: str) -> str:
    with open(path, encoding="utf-8") as stream:
        response = json.load(stream)
    if not response.get("ok"):
        raise RuntimeError(f"list-windows failed: {response.get('error')}")
    for window in response.get("result", {}).get("windows", []):
        if window.get("title") == "DCU native drag fixture":
            identifier = window.get("id")
            if isinstance(identifier, str) and identifier:
                return identifier
    raise RuntimeError("The native GTK fixture is absent from list-windows")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: parse-window.py LIST-WINDOWS-JSON")
    print(fixture_window_id(sys.argv[1]))

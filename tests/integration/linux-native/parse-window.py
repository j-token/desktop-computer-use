"""Print the stable ID of the GTK fixture from a CLI list-windows response."""

import json
import sys


def fixture_window_id(path: str, title: str) -> str:
    with open(path, encoding="utf-8") as stream:
        response = json.load(stream)
    if not response.get("ok"):
        raise RuntimeError(f"list-windows failed: {response.get('error')}")
    for window in response.get("result", {}).get("windows", []):
        if window.get("title") == title:
            identifier = window.get("id")
            if isinstance(identifier, str) and identifier:
                return identifier
    raise RuntimeError("The native GTK fixture is absent from list-windows")


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        raise SystemExit("Usage: parse-window.py LIST-WINDOWS-JSON [TITLE]")
    print(fixture_window_id(sys.argv[1], sys.argv[2] if len(sys.argv) == 3 else "DCU native drag fixture"))

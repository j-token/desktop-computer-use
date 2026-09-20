"""Print the native fixture ID from a gdbus ListWindows response."""

import ast
import json
import sys


def fixture_window_id(response: str) -> str:
    serialized_windows = ast.literal_eval(response)[0]
    windows = json.loads(serialized_windows)
    for window in windows:
        if window["title"] == "DCU native drag fixture":
            return window["id"]
    raise RuntimeError("The native fixture is absent from ListWindows")


if __name__ == "__main__":
    print(fixture_window_id(sys.argv[1]))

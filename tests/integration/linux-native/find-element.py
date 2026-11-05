"""Print "OBSERVATION_ID ELEMENT_INDEX" for an element of a get-app-state response.

The element is chosen by its AT-SPI role and, optionally, its name; NTH picks
among several matches in tree order (default 0).
"""

import json
import sys


def find_element(path: str, role: str, name: str | None, nth: int) -> tuple[str, int]:
    with open(path, encoding="utf-8") as stream:
        response = json.load(stream)
    if not response.get("ok"):
        raise RuntimeError(f"get-app-state failed: {response.get('error')}")
    observation = response["result"]
    matches = [
        element["index"]
        for element in observation.get("accessibility", {}).get("elements", [])
        if element.get("controlType") == role and (name is None or element.get("name") == name)
    ]
    if len(matches) <= nth:
        raise RuntimeError(f"No element #{nth} with role {role!r} and name {name!r}; found {len(matches)}")
    return observation["observationId"], matches[nth]


if __name__ == "__main__":
    if not 3 <= len(sys.argv) <= 5:
        raise SystemExit("Usage: find-element.py GET-APP-STATE-JSON ROLE [NAME|-] [NTH]")
    wanted_name = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] != "-" else None
    wanted_nth = int(sys.argv[4]) if len(sys.argv) > 4 else 0
    observation_id, index = find_element(sys.argv[1], sys.argv[2], wanted_name, wanted_nth)
    print(observation_id, index)

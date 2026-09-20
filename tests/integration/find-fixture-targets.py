"""Read-only pixel measurement of the fixture's blue/green rectangles."""
import json
import sys
from PIL import Image

image = Image.open(sys.argv[1]).convert("RGB")
groups = {"blue": [], "green": []}
for y in range(image.height):
    for x in range(image.width):
        r, g, b = image.getpixel((x, y))
        if r < 90 and 65 < g < 150 and b > 170:
            groups["blue"].append((x, y))
        if r < 90 and g > 140 and 55 < b < 150:
            groups["green"].append((x, y))
result = {"imageWidth": image.width, "imageHeight": image.height}
for name, points in groups.items():
    remaining = set(points)
    largest = []
    while remaining:
        pending = [remaining.pop()]
        component = []
        while pending:
            point = pending.pop()
            component.append(point)
            px, py = point
            for adjacent in [(px-1, py), (px+1, py), (px, py-1), (px, py+1)]:
                if adjacent in remaining:
                    remaining.remove(adjacent)
                    pending.append(adjacent)
        if len(component) > len(largest):
            largest = component
    if len(largest) < 100:
        raise ValueError("Missing fixture target: " + name)
    xs, ys = zip(*largest)
    result[name] = {"x": (min(xs)+max(xs))/2, "y": (min(ys)+max(ys))/2,
                    "bounds": [min(xs), min(ys), max(xs)+1, max(ys)+1]}
print(json.dumps(result))

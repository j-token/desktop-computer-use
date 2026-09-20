import { readFileSync } from "node:fs";
const events = readFileSync(process.argv[2], "utf8")
  .trim()
  .split("\n")
  .filter(Boolean)
  .map(JSON.parse);
const drags = [];
let current;
const anomalies = [];
for (const event of events) {
  if (event.event === "down") {
    if (current) anomalies.push({ event: "down-before-release", us: event.us });
    current = { down: event, moves: [] };
  } else if (event.event === "move" && current && event.left)
    current.moves.push(event);
  else if (event.event === "up" && current) {
    const times = [current.down, ...current.moves, event].map((e) => e.us);
    drags.push({
      durationMs: (event.us - current.down.us) / 1000,
      heldMoves: current.moves.length,
      displacement: Math.hypot(
        event.x - current.down.x,
        event.y - current.down.y,
      ),
      intervalsMs: times.slice(1).map((t, i) => (t - times[i]) / 1000),
      released: true,
    });
    current = undefined;
  }
}
const passed = drags.some(
  (d) => d.heldMoves >= 2 && d.displacement > 40 && d.durationMs >= 150,
);
console.log(
  JSON.stringify(
    {
      passed: passed && !current && !anomalies.length,
      drags,
      unreleasedDrag: Boolean(current),
      anomalies,
      completedEvents: events.filter((e) => e.event === "drag-complete").length,
    },
    null,
    2,
  ),
);
if (!passed || current || anomalies.length) process.exitCode = 1;

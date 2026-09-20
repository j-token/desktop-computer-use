import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";

export async function readFixtureEvents(logPath, firstEventIndex = 0) {
  const log = await readFile(logPath, "utf8");
  // A writer can be midway through its next record during polling.
  const completeLog = log.slice(0, log.lastIndexOf("\n") + 1);
  return completeLog
    .split("\n")
    .filter(Boolean)
    .slice(firstEventIndex)
    .map(JSON.parse);
}

export async function waitForFixtureEvent(
  logPath,
  firstEventIndex,
  eventName,
  timeoutMs,
) {
  const deadlineMs = performance.now() + timeoutMs;
  let events = [];
  while (performance.now() < deadlineMs) {
    events = await readFixtureEvents(logPath, firstEventIndex);
    const event = events.find((entry) => entry.event === eventName);
    if (event) return { event, events };
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  return { event: undefined, events };
}

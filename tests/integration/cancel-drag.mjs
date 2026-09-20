import { writeFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { DcuClient } from "../../dist/client.js";
import { readSession } from "../../dist/runtime.js";
import { readFixtureEvents, waitForFixtureEvent } from "./fixture-events.mjs";

const [windowId, logPath, outputPath] = process.argv.slice(2);
if (!windowId || !logPath || !outputPath)
  throw Error("Usage: node tests/integration/cancel-drag.mjs WINDOW LOG OUTPUT");
const client = new DcuClient();
const session = await readSession(client.paths);
if (!session) throw Error("Start a session first");
const firstEventIndex = (await readFixtureEvents(logPath)).length;

// Wait for native button-down rather than guessing when input delivery starts.
const dragResponsePromise = client.request("drag", {
  sessionId: session.sessionId,
  windowId,
  fromX: 148,
  fromY: 211,
  toX: 548,
  toY: 211,
  durationMs: 5000,
  steps: 100,
  holdBeforeMs: 300,
  holdAfterMs: 50,
});
const { event: buttonDown } = await waitForFixtureEvent(
  logPath,
  firstEventIndex,
  "down",
  5000,
);
if (!buttonDown) {
  await client.request("session.stop");
  await dragResponsePromise;
  throw Error("Fixture did not receive button-down");
}

const stopStartedMs = performance.now();
const stop = await client.request("session.stop");
const stopRoundtripMs = performance.now() - stopStartedMs;
const action = await dragResponsePromise;
const { event: buttonUp, events } = await waitForFixtureEvent(
  logPath,
  firstEventIndex,
  "up",
  3000,
);
const status = await client.request("session.status");

const report = {
  recordedAt: new Date().toISOString(),
  stopRoundtripMs,
  heldMs: buttonUp ? (buttonUp.us - buttonDown.us) / 1000 : null,
  stop,
  action,
  status,
  events,
  passed: Boolean(
    stop.ok &&
      !action.ok &&
      action.error?.code === "cancelled" &&
      buttonUp &&
      status.result?.active === false,
  ),
};
await writeFile(outputPath, JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report, null, 2));
if (!report.passed) process.exitCode = 1;

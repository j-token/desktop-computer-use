import { createConnection } from "node:net";
import { readFile, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { performance } from "node:perf_hooks";
import { DcuClient } from "../../dist/client.js";
import { readSession } from "../../dist/runtime.js";
import { readFixtureEvents, waitForFixtureEvent } from "./fixture-events.mjs";

const [mode, windowId, logPath, outputPath, daemonPid] = process.argv.slice(2);
if (
  !["disconnect", "kill"].includes(mode) ||
  !windowId ||
  !logPath ||
  !outputPath ||
  (mode === "kill" && !daemonPid)
)
  throw Error(
    "Usage: node tests/integration/interrupt-drag.mjs disconnect|kill WINDOW LOG OUTPUT [OWNED_DAEMON_PID]",
  );
const client = new DcuClient();
const session = await readSession(client.paths);
if (!session) throw Error("Start a session first");
const token = (await readFile(client.paths.tokenFile, "utf8")).trim();
const firstEventIndex = (await readFixtureEvents(logPath)).length;

const socket = createConnection(client.paths.endpoint);
let reply = "";
socket.on("data", (chunk) => (reply += chunk));
// Connection failure is expected; fixture button events determine the result.
socket.on("error", () => {});
await new Promise((resolve, reject) => {
  socket.once("connect", resolve);
  socket.once("error", reject);
});
socket.write(
  JSON.stringify({
    id: randomUUID(),
    token,
    method: "drag",
    params: {
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
    },
  }) + "\n",
);
const { event: buttonDown } = await waitForFixtureEvent(
  logPath,
  firstEventIndex,
  "down",
  5000,
);
if (!buttonDown) {
  socket.destroy();
  await client.request("session.stop");
  throw Error("Fixture did not receive button-down");
}
const interruptionStartedMs = performance.now();
if (mode === "kill") process.kill(Number(daemonPid), "SIGKILL");
else socket.destroy();
const { event: buttonUp, events } = await waitForFixtureEvent(
  logPath,
  firstEventIndex,
  "up",
  3000,
);
const observedReleaseAfterInterruptMs =
  performance.now() - interruptionStartedMs;
socket.destroy();
// Querying with autostart would hide the fact that the daemon was terminated.
const status =
  mode === "disconnect"
    ? await client.request("session.status", {}, { autoStart: false })
    : undefined;
const report = {
  recordedAt: new Date().toISOString(),
  mode,
  heldMs: buttonUp ? (buttonUp.us - buttonDown.us) / 1000 : null,
  observedReleaseAfterInterruptMs,
  passed: Boolean(
    buttonUp && (mode === "kill" || status?.result?.active === false),
  ),
  status,
  reply,
  events,
};
await writeFile(outputPath, JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report, null, 2));
if (!report.passed) process.exitCode = 1;

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { stat, writeFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { resolve } from "node:path";
import { DcuClient } from "../../dist/client.js";
import { readSession } from "../../dist/runtime.js";

// Requires an existing active DCU session and a visible disposable fixture.
const [windowId, fixturePid, output] = process.argv.slice(2);
if (!windowId || !fixturePid || !output)
  throw Error(
    "Usage: node tests/integration/benchmark-observation.mjs WINDOW_ID FIXTURE_PID OUTPUT_JSON",
  );
const client = new DcuClient();
const session = await readSession(client.paths);
if (!session) throw Error("Start a session before benchmarking");
const samples = { dcu: [], orca: [] };
function summarize(values) {
  const ordered = values
    .slice(1)
    .map((v) => v.roundtripMs)
    .sort((a, b) => a - b);
  return {
    coldMs: values[0].roundtripMs,
    warmCount: ordered.length,
    p50Ms: ordered[Math.ceil(ordered.length * 0.5) - 1],
    p95Ms: ordered[Math.ceil(ordered.length * 0.95) - 1],
  };
}
for (let i = 0; i < 21; i++) {
  const started = performance.now();
  const response = await client.call("get-app-state", {
    sessionId: session.sessionId,
    windowId,
    includeScreenshot: true,
    includeText: false,
  });
  const roundtripMs = performance.now() - started;
    samples.dcu.push({
    roundtripMs,
    timings: response.timings,
    bytes: response.screenshot?.path
      ? (await stat(response.screenshot.path)).size
      : 0,
    width: response.screenshot?.width,
    height: response.screenshot?.height,
      mimeType: response.screenshot?.mimeType,
      backend: response.screenshot?.backend,
      freshFrame: response.screenshot?.freshFrame,
      cachedFrame: response.screenshot?.cachedFrame,
  });
}
const startup = performance.now();
const child = spawn(
  "powershell.exe",
  [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    resolve("orca-research/native/computer-use-windows/runtime.ps1"),
    "-Serve",
  ],
  { windowsHide: true, stdio: ["pipe", "pipe", "pipe"] },
);
const reader = createInterface({ input: child.stdout });
let stderr = "";
child.stderr.on("data", (chunk) => (stderr += chunk.toString()));
const lines = reader[Symbol.asyncIterator]();
async function nextJson() {
  let timer;
  try {
    const value = await Promise.race([
      lines.next(),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(Error("Orca provider timeout " + stderr)),
          60000,
        );
      }),
    ]);
    if (value.done) throw Error("Orca exited " + stderr);
    return {
      json: JSON.parse(value.value),
      wireBytes: Buffer.byteLength(value.value),
    };
  } finally {
    clearTimeout(timer);
  }
}
try {
  const ready = await nextJson();
  if (!ready.json.ready) throw Error("Orca readiness missing");
  const orcaStartupMs = performance.now() - startup;
  for (let i = 0; i < 21; i++) {
    const started = performance.now();
    child.stdin.write(
      JSON.stringify({
        requestId: i + 1,
        tool: "get_app_state",
        app: `pid:${fixturePid}`,
        noScreenshot: false,
        restoreWindow: false,
      }) + "\n",
    );
    const response = await nextJson();
    const roundtripMs = performance.now() - started;
    if (!response.json.ok) throw Error(JSON.stringify(response.json));
    samples.orca.push({ roundtripMs, wireBytes: response.wireBytes });
  }
  const report = {
    recordedAt: new Date().toISOString(),
    platform: process.platform,
    fixturePid,
    windowId,
    boundary:
      "Persistent provider request through parsed response; excludes CLI process startup for both. DCU includes named-pipe transport and JSON parsing; Orca includes stdin/stdout and base64 PNG/tree response parsing.",
    caveat:
      "DCU JPEG screenshot-only default compared with original Orca PNG plus accessibility snapshot; this compares the requested defaults, not identical content/encoding.",
    orcaStartupMs,
    summary: { dcu: summarize(samples.dcu), orca: summarize(samples.orca) },
    samples,
  };
  await writeFile(output, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report.summary, null, 2));
} finally {
  child.stdin.end();
  reader.close();
  child.kill();
}

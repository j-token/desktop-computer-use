#!/usr/bin/env node

import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const nativePath = process.env.DCU_NATIVE_PATH;
if (!nativePath) throw new Error("Set DCU_NATIVE_PATH to the corrected native daemon before running this real MCP smoke test");
await stat(nativePath);
await stat(join(root, "dist", "cli.js"));

const ownsRuntime = !process.env.DCU_RUNTIME_DIR;
const runtimeDirectory = process.env.DCU_RUNTIME_DIR || await mkdtemp(join(tmpdir(), "dcu-mcp-native-smoke-"));
const childEnv = { ...process.env, DCU_NATIVE_PATH: resolve(nativePath), DCU_RUNTIME_DIR: runtimeDirectory };
delete childEnv.DCU_ENDPOINT;
const child = spawn(process.execPath, [join(root, "dist", "cli.js"), "mcp", "serve"], {
  cwd: root,
  env: childEnv,
  stdio: ["pipe", "pipe", "pipe"],
  windowsHide: true
});

const pending = new Map();
let nextId = 1;
let stdoutError = "";
const lines = createInterface({ input: child.stdout });
lines.on("line", line => {
  if (!line.trim()) return;
  let message;
  try { message = JSON.parse(line); }
  catch (error) {
    stdoutError += `invalid MCP JSON: ${error instanceof Error ? error.message : String(error)}\n${line}\n`;
    return;
  }
  if (message && Object.prototype.hasOwnProperty.call(message, "id")) {
    const waiter = pending.get(message.id);
    if (waiter) {
      pending.delete(message.id);
      waiter.resolve(message);
    }
  }
});
const childFailure = new Promise((_, reject) => {
  child.once("error", reject);
  child.once("exit", (code, signal) => {
    if (code !== null && code !== 0) reject(new Error(`MCP server exited with code ${code}${signal ? ` (${signal})` : ""}`));
  });
});

function rpc(method, params = {}) {
  const id = nextId++;
  const request = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
  return Promise.race([
    new Promise((resolvePromise, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`Timed out waiting for MCP ${method}`));
      }, Number(process.env.DCU_MCP_SMOKE_TIMEOUT_MS || 30000));
      pending.set(id, {
        resolve: message => { clearTimeout(timer); resolvePromise(message); },
        reject: error => { clearTimeout(timer); reject(error); }
      });
      try { child.stdin.write(request); }
      catch (error) { clearTimeout(timer); pending.delete(id); reject(error); }
    }),
    childFailure
  ]);
}

async function notify(method, params = {}) {
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
}

function toolText(result, name) {
  if (result?.isError) {
    const message = result.content?.find(item => item.type === "text")?.text || "unknown MCP tool error";
    throw new Error(`${name} failed: ${message}`);
  }
  const text = result?.content?.find(item => item.type === "text")?.text;
  assert.equal(typeof text, "string", `${name} did not return a JSON text block`);
  return text;
}

async function callTool(name, argumentsValue = {}) {
  const response = await rpc("tools/call", { name, arguments: argumentsValue });
  if (response.error) throw new Error(`${name}: ${response.error.message || JSON.stringify(response.error)}`);
  return response.result;
}

async function shutdownDaemon() {
  await new Promise(resolvePromise => {
    const shutdown = spawn(process.execPath, [join(root, "dist", "cli.js"), "daemon", "shutdown"], {
      cwd: root,
      env: childEnv,
      stdio: ["ignore", "ignore", "ignore"],
      windowsHide: true
    });
    shutdown.once("error", () => resolvePromise());
    shutdown.once("exit", () => resolvePromise());
  });
}

let sessionStarted = false;
try {
  const initialize = await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "desktop-computer-use-native-smoke", version: "1" }
  });
  if (initialize.error) throw new Error(`initialize: ${initialize.error.message || JSON.stringify(initialize.error)}`);
  await notify("notifications/initialized");

  const started = await callTool("dcu_session_start");
  const startPayload = JSON.parse(toolText(started, "dcu_session_start"));
  assert.equal(startPayload.ready, true, "native session did not report ready");
  assert.equal(typeof startPayload.sessionId, "string", "native session did not return a session ID");
  sessionStarted = true;

  const listed = await callTool("dcu_list_windows");
  const listedPayload = JSON.parse(toolText(listed, "dcu_list_windows"));
  const windows = Array.isArray(listedPayload.windows) ? listedPayload.windows : [];
  const requestedWindowId = process.env.DCU_SMOKE_WINDOW_ID;
  const target = requestedWindowId
    ? windows.find(window => window?.id === requestedWindowId)
    : windows.find(window => typeof window?.id === "string" && window.isMinimized !== true && Number(window.width) > 0 && Number(window.height) > 0) || windows[0];
  assert.equal(typeof target?.id, "string", "native backend returned no usable window");

  const state = await callTool("dcu_get_app_state", { windowId: target.id, includeScreenshot: true, includeText: false, format: "jpeg" });
  const image = state?.content?.find(item => item.type === "image");
  assert.equal(typeof image?.data, "string", "MCP response did not include image content");
  assert.ok(image.data.length > 16, "MCP image content was empty");
  const bytes = Buffer.from(image.data, "base64");
  assert.ok(bytes.length > 16, "MCP image data was not valid base64");
  const jpeg = bytes[0] === 0xff && bytes[1] === 0xd8;
  const png = bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47;
  assert.ok(jpeg || png, `MCP image bytes do not match JPEG/PNG (${image.mimeType || "unknown"})`);

  console.log(JSON.stringify({ ok: true, nativePath: resolve(nativePath), runtimeDirectory, windowId: target.id, mimeType: image.mimeType, imageBytes: bytes.length }));
} catch (error) {
  const detail = error instanceof Error ? error.message : String(error);
  throw new Error(`${detail}${stdoutError ? `\n${stdoutError}` : ""}`);
} finally {
  if (sessionStarted) {
    try { await callTool("dcu_session_stop"); } catch { /* cleanup continues even if the daemon already stopped */ }
  }
  try { await shutdownDaemon(); } catch { /* cleanup continues; the runtime directory reports a lock if this fails */ }
  child.kill();
  if (child.exitCode === null && child.signalCode === null) await new Promise(resolvePromise => child.once("exit", resolvePromise));
  if (ownsRuntime) await rm(runtimeDirectory, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

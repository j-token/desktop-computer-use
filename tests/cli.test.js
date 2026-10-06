import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "../dist/cli.js";
import { resultContent } from "../dist/mcp.js";
import { clearSession, ensureRuntime, readSession, usageInstructions, writeSession } from "../dist/runtime.js";
import { parseGnomeShellMajor, selectGnomeVariant } from "../dist/setup.js";

test("CLI option conversion preserves paced drag defaults and aliases", () => {
  const parsed = parseArgs(["drag", "--app", "game", "--from-x", "10", "--from-y", "20", "--to-x", "90", "--to-y", "120", "--no-screenshot"]);
  assert.deepEqual(parsed.command, ["drag"]);
  assert.equal(parsed.options.app, "game");
  assert.equal(parsed.options.includeScreenshot, false);
  assert.equal(parsed.options.fromX, 10);
  assert.deepEqual(parseArgs(["get-app-state", "notepad"]).positional, ["notepad"]);
  assert.deepEqual(parseArgs(["session", "start"]).command, ["session", "start"]);
});

test("MCP converts a private screenshot path into image content", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-mcp-"));
  const path = join(directory, "shot.jpg");
  await writeFile(path, Buffer.from([0xff, 0xd8, 0xff]));
  try {
    const content = await resultContent({ screenshot: { path, mimeType: "image/jpeg", scale: 1 } });
    assert.equal(content[0].type, "text");
    assert.equal(content[1].type, "image");
    assert.equal(content[1].data, "/9j/");
    assert.equal(content[1].mimeType, "image/jpeg");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("MCP emits an image for an observed action screenshot", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-mcp-"));
  const path = join(directory, "action.jpg");
  await writeFile(path, Buffer.from([0xff, 0xd8, 0xff]));
  try {
    const content = await resultContent({ delivered: true, observation: { screenshot: { path, mimeType: "image/jpeg" } } });
    assert.equal(content.filter(item => item.type === "image").length, 1);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("MCP embeds only the reduced image of an observation and the full image of get-full-screenshot", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-mcp-"));
  const reduced = join(directory, "obs.jpg");
  const full = join(directory, "obs-full.jpg");
  await writeFile(reduced, Buffer.from([0xff, 0xd8, 0xff]));
  await writeFile(full, Buffer.from([0x89, 0x50, 0x4e]));
  try {
    const observed = await resultContent({
      observationId: "obs",
      screenshot: { path: reduced, mimeType: "image/jpeg", variant: "reduced", fullAvailable: true, fullWidth: 2560, fullHeight: 1440 }
    });
    const observedImages = observed.filter(item => item.type === "image");
    assert.equal(observedImages.length, 1);
    assert.equal(observedImages[0].data, "/9j/");
    const fullContent = await resultContent({
      observationId: "obs",
      screenshot: { path: full, mimeType: "image/jpeg", variant: "full" },
      notice: "To click a point read from this image, pass --coords full."
    });
    const fullImages = fullContent.filter(item => item.type === "image");
    assert.equal(fullImages.length, 1);
    assert.equal(fullImages[0].data, "iVBO");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

async function runCliProcess(args) {
  const { spawn } = await import("node:child_process");
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["dist/cli.js", ...args], { cwd: process.cwd() });
    let text = "";
    child.stdout.on("data", chunk => { text += chunk; });
    child.once("error", reject);
    child.once("exit", code => resolve({ code, response: JSON.parse(text.trim().split("\n").at(-1)) }));
  });
}

// Every case fails validation before a native request, so no daemon starts.
test("CLI validates --coords before sending input", async () => {
  const { code, response } = await runCliProcess(["click", "--session-id", "s", "--app", "x", "--x", "1", "--y", "1", "--coords", "window"]);
  assert.equal(code, 1);
  assert.equal(response.error.code, "invalid_argument");
  assert.match(response.error.message, /coords must be reduced, full/);
  assert.equal(parseArgs(["click", "--coords", "full"]).options.coords, "full");
  const negative = await runCliProcess(["click", "--session-id", "s", "--app", "x", "--x", "-1", "--y", "1"]);
  assert.equal(negative.response.error.code, "invalid_argument");
});

test("CLI get-full-screenshot requires a target window and an observation ID", async () => {
  const missingId = await runCliProcess(["get-full-screenshot", "--session-id", "s", "--app", "x"]);
  assert.equal(missingId.code, 1);
  assert.match(missingId.response.error.message, /requires --observation-id/);
  const missingWindow = await runCliProcess(["get-full-screenshot", "--session-id", "s", "--observation-id", "o"]);
  assert.match(missingWindow.response.error.message, /requires --app or --window-id/);
});

test("session state is persisted in the supplied runtime directory and can be cleared", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-session-"));
  const paths = {
    directory,
    endpoint: join(directory, "daemon.sock"),
    tokenFile: join(directory, "token"),
    sessionFile: join(directory, "session.json")
  };
  try {
    await writeSession("session-test", paths);
    assert.equal((await readSession(paths)).sessionId, "session-test");
    await clearSession(paths);
    assert.equal(await readSession(paths), undefined);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("setup guidance documents the active indicator and Esc emergency stop", () => {
  for (const platform of ["win32", "linux"]) {
    const guidance = usageInstructions(platform).join("\n");
    assert.match(guidance, /blue inward-fading screen-edge border/);
    assert.match(guidance, /Press Esc/);
  }
});

test("GNOME setup selects the archive matching supported Shell generations", () => {
  assert.equal(parseGnomeShellMajor("GNOME Shell 42.9"), 42);
  assert.equal(parseGnomeShellMajor("GNOME Shell 46.0"), 46);
  assert.equal(selectGnomeVariant("GNOME Shell 41.9"), undefined);
  assert.equal(selectGnomeVariant("GNOME Shell 42.9"), "legacy");
  assert.equal(selectGnomeVariant("GNOME Shell 44.8"), "legacy");
  assert.equal(selectGnomeVariant("GNOME Shell 45.0"), "modern");
  assert.equal(selectGnomeVariant("GNOME Shell 46.0"), "modern");
  assert.equal(selectGnomeVariant("GNOME Shell 51.0"), undefined);
  assert.equal(selectGnomeVariant(undefined), undefined);
});

test("Windows runtime directories outside user-local roots are rejected", async () => {
  if (process.platform !== "win32") return;
  const directory = "C:\\desktop-computer-use-public-test";
  await assert.rejects(
    () => ensureRuntime({
      directory,
      endpoint: "\\\\.\\pipe\\dcu-public-test",
      tokenFile: `${directory}\\token`,
      sessionFile: `${directory}\\session.json`
    }),
    error => error?.code === "invalid_environment"
  );
});

test("the skill entrypoint is runnable without a native binary for help", async () => {
  const { spawn } = await import("node:child_process");
  const output = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["skills/desktop-computer-use/scripts/dcu.mjs", "--help"], { cwd: process.cwd() });
    let text = "";
    child.stdout.on("data", chunk => { text += chunk; });
    child.stderr.on("data", chunk => { text += chunk; });
    child.once("error", reject);
    child.once("exit", code => code === 0 ? resolve(text) : reject(new Error(`exit ${code}: ${text}`)));
  });
  assert.match(String(output), /desktop-computer-use/);
  assert.match(String(output), /Emergency stop: press Esc/);
  assert.match(String(output), /blue inward-fading screen-edge border/);
});

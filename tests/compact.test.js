import test from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { compactResponse, compactResult, elementLine } from "../dist/compact.js";

const fixtureDir = join("tests", "fixtures", "uia");

test("compact observation keeps decision fields and drops diagnostics", () => {
  const result = compactResult({
    observationId: "obs",
    sessionId: "0123456789abcdef",
    coordinateSpace: "window",
    window: { id: "hwnd:1", app: "notepad.exe", pid: 7, title: "Untitled", x: 10, y: 20, width: 800, height: 600,
      isMinimized: false, isForeground: true, isOffscreen: false, isEnabled: true },
    overlayRegions: [],
    timings: { totalMs: 3 },
    capturedAt: "t",
    observedAt: "t",
    toggles: [],
    screenshot: { path: "/p/obs.jpeg", mimeType: "image/jpeg", width: 400, height: 300, variant: "reduced",
      fullWidth: 800, fullHeight: 600, scale: 0.5, backend: "wgc", freshFrame: true },
    accessibility: {
      tree: "[0] Window",
      elementCount: 3,
      elements: [
        { index: 0, controlType: "Button", name: "OK", automationId: "ok", enabled: true, focused: false,
          offscreen: false, bounds: { x: 1, y: 2, width: 30, height: 20 }, patterns: ["invoke"], runtimeId: [1, 2] },
        { index: 1, controlType: "Text", name: "", automationId: "", bounds: { x: 0, y: 0, width: 0, height: 0 }, patterns: [] },
        { index: 2, controlType: "Edit", name: "Body", value: "hello", enabled: false,
          bounds: { x: 0, y: 30, width: 400, height: 200 }, patterns: ["value"] }
      ]
    }
  });
  assert.deepEqual(result, {
    observationId: "obs",
    window: "hwnd:1 \"Untitled\" app=notepad.exe 10,20 800x600 fg",
    screenshot: { path: "/p/obs.jpeg", width: 400, height: 300, full: "800x600" },
    accessibility: {
      elements: ["0 Button \"OK\" 1,2 30x20 invoke", "2 Edit \"Body\" =\"hello\" 0,30 400x200 value disabled"],
      hidden: 1
    }
  });
});

test("compact keeps toggles, notices, modal hints, and action results", () => {
  const result = compactResult({
    delivered: true,
    verification: { state: "unverified" },
    toggles: ["shift"],
    notice: "Toggle still on: shift.",
    observation: { observationId: "o", modal: { windowId: "hwnd:2", title: "Save", app: "x" } }
  });
  assert.deepEqual(result, {
    delivered: true,
    toggles: ["shift"],
    notice: "Toggle still on: shift.",
    observation: { observationId: "o", modal: { windowId: "hwnd:2", title: "Save", app: "x" } }
  });
});

test("compact lists windows and apps as one line per window", () => {
  const dialog = { id: "hwnd:2", app: "n.exe", title: "Save", x: 0, y: 0, width: 10, height: 10,
    isForeground: false, isEnabled: true, ownerWindowId: "hwnd:1", modal: true };
  assert.deepEqual(compactResult({ windows: [dialog] }), {
    windows: ["hwnd:2 \"Save\" app=n.exe 0,0 10x10 owner=hwnd:1 modal"]
  });
  assert.deepEqual(compactResult({ apps: [{ id: "n.exe", name: "n.exe", windows: [dialog] }] }), {
    apps: { "n.exe": ["hwnd:2 \"Save\" 0,0 10x10 owner=hwnd:1 modal"] }
  });
});

test("compact understands Linux AT-SPI element records", () => {
  assert.equal(
    elementLine({ index: 4, controlType: "push button", name: "Open", value: "", isSelected: true,
      actions: ["click"], frame: { x: 5, y: 6, width: 7, height: 8 } }),
    "4 push button \"Open\" 5,6 7x8 click selected"
  );
  assert.equal(elementLine({ index: 5, controlType: "label", name: "x", frame: null, actions: [] }), undefined);
});

test("compact clips long text and keeps errors unchanged", () => {
  const line = elementLine({ index: 0, controlType: "Edit", name: "", value: "a".repeat(400),
    bounds: { x: 0, y: 0, width: 1, height: 1 }, patterns: ["value"] });
  assert.match(line, /…\(400\)"/);
  const error = { code: "modal_active", message: "m", details: { toggles: [] } };
  assert.deepEqual(compactResponse({ id: "x", ok: false, error }), { ok: false, error });
});

test("compact output is much smaller than the native accessibility JSON", async () => {
  let raw = 0;
  let compact = 0;
  for (const name of (await readdir(fixtureDir)).filter(file => file.endsWith(".json"))) {
    const value = JSON.parse(await readFile(join(fixtureDir, name), "utf8"));
    raw += JSON.stringify(value).length;
    compact += JSON.stringify(compactResult(value)).length;
  }
  assert.ok(compact < raw * 0.3, `compact ${compact} vs raw ${raw}`);
});

import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

async function runPackage(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["scripts/package-skill.mjs", ...args], { cwd: process.cwd(), windowsHide: true });
    let output = "";
    child.stdout.on("data", chunk => { output += chunk; });
    child.stderr.on("data", chunk => { output += chunk; });
    child.once("error", reject);
    child.once("exit", code => resolve({ code, output }));
  });
}

test("full package mode requires and copies both native binaries and GNOME archives", async () => {
  const root = await mkdtemp(join(tmpdir(), "dcu-package-"));
  const native = join(root, "native");
  const extensions = join(root, "extensions");
  const output = join(root, "full");
  await mkdir(join(native, "win32-x64"), { recursive: true });
  await mkdir(join(native, "linux-x64"), { recursive: true });
  await mkdir(extensions, { recursive: true });
  await writeFile(join(native, "win32-x64", "desktop-computer-use-native.exe"), "windows");
  await writeFile(join(native, "linux-x64", "desktop-computer-use-native"), "linux");
  await writeFile(join(extensions, "desktop-computer-use-gnome-modern.zip"), "modern");
  await writeFile(join(extensions, "desktop-computer-use-gnome-legacy.zip"), "legacy");
  try {
    const result = await runPackage(["--native-dir", native, "--extension-dir", extensions, "--out", output]);
    assert.equal(result.code, 0, result.output);
    await stat(join(output, "skills", "desktop-computer-use", "bin", "win32-x64", "desktop-computer-use-native.exe"));
    await stat(join(output, "skills", "desktop-computer-use", "bin", "linux-x64", "desktop-computer-use-native"));
    assert.equal(await readFile(join(output, "skills", "desktop-computer-use", "extensions", "desktop-computer-use-gnome-modern.zip"), "utf8"), "modern");
    assert.equal(await readFile(join(output, "skills", "desktop-computer-use", "extensions", "desktop-computer-use-gnome-legacy.zip"), "utf8"), "legacy");
    const bundledLicenses = await readFile(join(output, "skills", "desktop-computer-use", "references", "THIRD_PARTY_LICENSES.md"), "utf8");
    assert.match(bundledLicenses, /@modelcontextprotocol\/sdk@/);
    assert.match(bundledLicenses, /## zod@/);
    const nativeNotices = await readFile(join(output, "skills", "desktop-computer-use", "references", "THIRD_PARTY_NOTICES.md"), "utf8");
    assert.match(nativeNotices, /nlohmann\/json/);
    const plugin = JSON.parse(await readFile(join(output, ".claude-plugin", "plugin.json"), "utf8"));
    assert.equal(plugin.name, "desktop-computer-use");
    const mcp = JSON.parse(await readFile(join(output, ".mcp.json"), "utf8"));
    assert.deepEqual(mcp.mcpServers["desktop-computer-use"].args, [
      "${CLAUDE_PLUGIN_ROOT}/skills/desktop-computer-use/scripts/dcu.mjs", "mcp", "serve"
    ]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("the packaged skill directory alone finds its native binary, as an npx skills copy would", async () => {
  const root = await mkdtemp(join(tmpdir(), "dcu-standalone-"));
  const native = join(root, "native");
  const extensions = join(root, "extensions");
  const output = join(root, "full");
  const installed = join(root, "installed", "desktop-computer-use");
  const binary = process.platform === "win32" ? "desktop-computer-use-native.exe" : "desktop-computer-use-native";
  const triplet = process.platform === "win32" ? "win32-x64" : "linux-x64";
  await mkdir(join(native, "win32-x64"), { recursive: true });
  await mkdir(join(native, "linux-x64"), { recursive: true });
  await mkdir(extensions, { recursive: true });
  await writeFile(join(native, "win32-x64", "desktop-computer-use-native.exe"), "windows");
  await writeFile(join(native, "linux-x64", "desktop-computer-use-native"), "linux");
  await writeFile(join(extensions, "desktop-computer-use-gnome-modern.zip"), "modern");
  await writeFile(join(extensions, "desktop-computer-use-gnome-legacy.zip"), "legacy");
  try {
    const result = await runPackage(["--native-dir", native, "--extension-dir", extensions, "--out", output]);
    assert.equal(result.code, 0, result.output);
    await cp(join(output, "skills", "desktop-computer-use"), installed, { recursive: true });
    const { resolveNativePath } = await import("../dist/runtime.js");
    const previous = process.env.DCU_NATIVE_PATH;
    delete process.env.DCU_NATIVE_PATH;
    try {
      assert.equal(await resolveNativePath(installed), join(installed, "bin", triplet, binary));
    } finally {
      if (previous !== undefined) process.env.DCU_NATIVE_PATH = previous;
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("adapter-only mode is explicit and omits native artifacts", async () => {
  const root = await mkdtemp(join(tmpdir(), "dcu-adapters-"));
  const output = join(root, "adapters");
  const secretName = `.ENV.package-test-${process.pid}`;
  const sourceSecret = join(process.cwd(), "skills", "desktop-computer-use", secretName);
  await writeFile(sourceSecret, "PACKAGE_TEST_SECRET=must-not-ship\n", "utf8");
  try {
    const result = await runPackage(["--adapters-only", "--out", output]);
    assert.equal(result.code, 0, result.output);
    await stat(join(output, "skills", "desktop-computer-use", "scripts", "dcu.mjs"));
    await assert.rejects(() => stat(join(output, "skills", "desktop-computer-use", ".env")));
    await assert.rejects(() => stat(join(output, "skills", "desktop-computer-use", secretName)));
    await assert.rejects(() => stat(join(output, "skills", "desktop-computer-use", "bin")));
    await assert.rejects(() => stat(join(output, "skills", "desktop-computer-use", "extensions")));
  } finally {
    await rm(sourceSecret, { force: true });
    await rm(root, { recursive: true, force: true });
  }
});

test("full package mode fails clearly when a platform binary is missing", async () => {
  const root = await mkdtemp(join(tmpdir(), "dcu-package-missing-"));
  try {
    const result = await runPackage(["--native-dir", root, "--out", join(root, "out")]);
    assert.notEqual(result.code, 0);
    assert.match(result.output, /Missing required win32-x64 native binary/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("package output guard refuses source-tree paths", async () => {
  const output = join(process.cwd(), "src", "unsafe-package-output");
  const result = await runPackage(["--adapters-only", "--out", output]);
  assert.notEqual(result.code, 0);
  assert.match(result.output, /must stay below/);
});

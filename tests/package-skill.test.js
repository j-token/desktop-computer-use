import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
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
    await stat(join(output, "desktop-computer-use", "bin", "win32-x64", "desktop-computer-use-native.exe"));
    await stat(join(output, "desktop-computer-use", "bin", "linux-x64", "desktop-computer-use-native"));
    assert.equal(await readFile(join(output, "desktop-computer-use", "extensions", "desktop-computer-use-gnome-modern.zip"), "utf8"), "modern");
    assert.equal(await readFile(join(output, "desktop-computer-use", "extensions", "desktop-computer-use-gnome-legacy.zip"), "utf8"), "legacy");
    const bundledLicenses = await readFile(join(output, "desktop-computer-use", "references", "THIRD_PARTY_LICENSES.md"), "utf8");
    assert.match(bundledLicenses, /@modelcontextprotocol\/sdk@/);
    assert.match(bundledLicenses, /## zod@/);
    const nativeNotices = await readFile(join(output, "desktop-computer-use", "references", "THIRD_PARTY_NOTICES.md"), "utf8");
    assert.match(nativeNotices, /nlohmann\/json/);
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
    await stat(join(output, "desktop-computer-use", "scripts", "dcu.mjs"));
    await stat(join(output, "desktop-computer-use", ".env.example"));
    await assert.rejects(() => stat(join(output, "desktop-computer-use", ".env")));
    await assert.rejects(() => stat(join(output, "desktop-computer-use", secretName)));
    await assert.rejects(() => stat(join(output, "desktop-computer-use", "bin")));
    await assert.rejects(() => stat(join(output, "desktop-computer-use", "extensions")));
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

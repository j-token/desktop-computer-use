import test from "node:test";
import assert from "node:assert/strict";
import { copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const execute = promisify(execFile);

async function installedSkill(t) {
  const directory = await mkdtemp(join(tmpdir(), "dcu-installed-config-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const skill = join(directory, "installed", "desktop-computer-use");
  const cwd = join(directory, "unrelated");
  await mkdir(join(skill, "scripts"), { recursive: true });
  await mkdir(cwd);
  const entrypoint = join(skill, "scripts", "dcu.mjs");
  await copyFile(new URL("../skills/desktop-computer-use/scripts/dcu.mjs", import.meta.url), entrypoint);
  await writeFile(join(skill, ".env"), [
    "DCU_AGENT_MODE=llm+jev",
    "DCU_JEV_API_KEY=installed-test-secret",
    "DCU_JEV_BASE_URL=https://user:password@jev.example.test/v1?token=hidden",
    "DCU_JEV_MODEL=installed-model"
  ].join("\n"), "utf8");
  await writeFile(join(cwd, ".env"), "DCU_AGENT_MODE=unsupported-cwd-mode\n", "utf8");

  // These commands must resolve configuration before any native or provider I/O.
  const guard = join(directory, "deny-io.mjs");
  await writeFile(guard, `
    import childProcess from "node:child_process";
    import net from "node:net";
    import { syncBuiltinESMExports } from "node:module";
    const denied = () => { throw new Error("Unexpected native or model I/O"); };
    globalThis.fetch = denied;
    childProcess.spawn = denied;
    childProcess.execFile = denied;
    net.createConnection = denied;
    net.connect = denied;
    syncBuiltinESMExports();
  `, "utf8");
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
    !/^(DCU_|TYPESAFE_API_KEY$|NODE_OPTIONS$)/i.test(key)
  ));
  async function run(args, overrides = {}) {
    const result = await execute(process.execPath,
      ["--import", pathToFileURL(guard).href, entrypoint, "agent", ...args],
      { cwd, env: { ...env, ...overrides }, timeout: 10000, windowsHide: true });
    assert.equal(result.stderr, "");
    assert.doesNotMatch(result.stdout, /installed-test-secret|password|hidden/);
    return JSON.parse(result.stdout);
  }
  return { run };
}

test("installed bundle reads its own skill env from an unrelated cwd without native or model I/O", async t => {
  const { run } = await installedSkill(t);
  const response = await run(["config"]);
  assert.equal(response.ok, true);
  assert.equal(response.result.mode, "llm+jev");
  assert.equal(response.result.llm, "harness");
  assert.equal(response.result.jev.apiKeyPresent, true);
  assert.equal(response.result.jev.host, "jev.example.test");
  assert.equal(response.result.jev.model, "installed-model");
});

test("installed bundle honors process values and a temporary config mode override", async t => {
  const { run } = await installedSkill(t);
  const response = await run(["config", "--mode", "llm"], {
    DCU_JEV_API_KEY: "",
    DCU_JEV_MODEL: "process-model"
  });
  assert.equal(response.result.mode, "llm");
  assert.equal(response.result.jev.apiKeyPresent, false);
  assert.equal(response.result.jev.model, "process-model");
  assert.equal(response.result.ready, true);
  assert.equal((await run(["config"])).result.mode, "llm+jev");
});

test("explicit Jev helpers reject mode overrides before native or model I/O", async t => {
  const { run } = await installedSkill(t);
  for (const command of ["run", "decide", "doctor"]) {
    await assert.rejects(run([command, "--mode", "llm"]), error => {
      assert.equal(error.code, 1);
      const response = JSON.parse(error.stdout);
      assert.equal(response.ok, false);
      assert.equal(response.error.code, "invalid_argument");
      assert.match(response.error.message, /mode/);
      assert.doesNotMatch(error.stdout, /Unexpected native or model I\/O|installed-test-secret/);
      return true;
    });
  }
});

test("hybrid configuration without a key returns an offline fallback signal", async t => {
  const { run } = await installedSkill(t);
  await assert.rejects(run(["config"], { DCU_JEV_API_KEY: "" }), error => {
    assert.equal(error.code, 1);
    const response = JSON.parse(error.stdout);
    assert.equal(response.ok, false);
    assert.equal(response.result.mode, "llm+jev");
    assert.equal(response.result.ready, false);
    assert.equal(response.result.jev.apiKeyPresent, false);
    assert.doesNotMatch(error.stdout, /Unexpected native or model I\/O|installed-test-secret/);
    return true;
  });
});

import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { defaultAgentEnvFile, loadAgentEnv } from "../dist/agent/env.js";

test("default agent env paths distinguish the source build from the installed bundle", () => {
  const repositoryRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
  const sourceModule = pathToFileURL(join(repositoryRoot, "dist", "agent", "env.js"));
  assert.equal(
    defaultAgentEnvFile(sourceModule),
    join(repositoryRoot, "skills", "desktop-computer-use", ".env")
  );

  const installedModule = pathToFileURL(join(repositoryRoot, "installed", "desktop-computer-use", "scripts", "dcu.mjs"));
  assert.equal(
    defaultAgentEnvFile(installedModule),
    join(repositoryRoot, "installed", "desktop-computer-use", ".env")
  );
});

test("an explicit env file wins over DCU_ENV_FILE while same-name process values win over the file", async t => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-agent-env-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await writeFile(join(directory, "selected.env"), [
    "export FROM_FILE=loaded",
    "PRESERVED=file-value",
    "QUOTED=\"two words\""
  ].join("\n"), "utf8");
  await writeFile(join(directory, "ignored.env"), "FROM_FILE=ignored\n", "utf8");

  const env = {
    DCU_ENV_FILE: "ignored.env",
    PRESERVED: "process-value"
  };
  const result = await loadAgentEnv({ env, envFile: "selected.env", cwd: directory });

  assert.equal(result.source, "cli");
  assert.equal(result.path, join(directory, "selected.env"));
  assert.equal(result.found, true);
  assert.deepEqual(result.loadedKeys.sort(), ["FROM_FILE", "QUOTED"]);
  assert.equal(env.FROM_FILE, "loaded");
  assert.equal(env.PRESERVED, "process-value");
  assert.equal(env.QUOTED, "two words");
});

test("the default location is optional and never falls back to a cwd .env", async t => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-agent-env-cwd-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const cwd = join(directory, "unrelated");
  await mkdir(cwd);
  await writeFile(join(cwd, ".env"), "SHOULD_NOT_LOAD=yes\n", "utf8");

  const env = {};
  const moduleUrl = pathToFileURL(join(directory, "dist", "agent", "env.js"));
  const result = await loadAgentEnv({ env, cwd, moduleUrl });

  assert.equal(result.source, "default");
  assert.equal(result.path, join(directory, "skills", "desktop-computer-use", ".env"));
  assert.equal(result.found, false);
  assert.equal(env.SHOULD_NOT_LOAD, undefined);
});

test("DCU_ENV_FILE selects one file relative to cwd when the CLI has no override", async t => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-agent-env-variable-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await writeFile(join(directory, "configured.env"), "CONFIGURED_VALUE=loaded\n", "utf8");
  const env = { DCU_ENV_FILE: "configured.env" };

  const result = await loadAgentEnv({ env, cwd: directory });

  assert.equal(result.source, "environment");
  assert.equal(result.path, join(directory, "configured.env"));
  assert.equal(env.CONFIGURED_VALUE, "loaded");
});

test("a selected env file is required and reports a configuration error when missing", async () => {
  await assert.rejects(
    loadAgentEnv({ env: {}, envFile: "missing.env", cwd: tmpdir() }),
    error => error?.code === "agent_config" && /missing\.env/.test(error.message)
  );
});

import { spawn } from "node:child_process";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
const runtimeModule = pathToFileURL(resolve("dist/runtime.js")).href;
const code =
  "const {ensureToken,resolveRuntimePaths}=await import(process.argv[1]); const {createHash}=await import('node:crypto'); const token=await ensureToken(resolveRuntimePaths()); console.log(createHash('sha256').update(token).digest('hex'));";
const failures = [];
for (let round = 0; round < 5; round++) {
  const directory = await mkdtemp(join(tmpdir(), "dcu-token-race-"));
  const results = await Promise.all(
    Array.from(
      { length: 16 },
      () =>
        new Promise((resolveResult) => {
          const child = spawn(
            process.execPath,
            ["--input-type=module", "-e", code, runtimeModule],
            {
              windowsHide: true,
              env: { ...process.env, DCU_RUNTIME_DIR: directory },
            },
          );
          let stdout = "",
            stderr = "";
          child.stdout.on("data", (chunk) => (stdout += chunk));
          child.stderr.on("data", (chunk) => (stderr += chunk));
          child.on("exit", (exit) =>
            resolveResult({ exit, hash: stdout.trim(), stderr }),
          );
        }),
    ),
  );
  const hashes = new Set(
    results.filter((r) => r.exit === 0).map((r) => r.hash),
  );
  if (hashes.size !== 1 || results.some((r) => r.exit !== 0))
    failures.push({
      round,
      directory,
      distinctHashes: hashes.size,
      errors: results.filter((r) => r.exit !== 0),
    });
}
console.log(
  JSON.stringify(
    {
      passed: failures.length === 0,
      rounds: 5,
      processesPerRound: 16,
      failures,
    },
    null,
    2,
  ),
);
if (failures.length) process.exitCode = 1;

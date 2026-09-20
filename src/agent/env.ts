import { readFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseEnv } from "node:util";
import { DcuError, asError } from "../errors.js";

export type AgentEnvSource = "cli" | "environment" | "default";

export interface LoadAgentEnvOptions {
  /** `--env-file`; when present it wins over DCU_ENV_FILE. */
  envFile?: string;
  env?: NodeJS.ProcessEnv;
  cwd?: string;
  moduleUrl?: string | URL;
}

export interface AgentEnvLoadResult {
  path: string;
  source: AgentEnvSource;
  found: boolean;
  loadedKeys: string[];
}

/**
 * The source build lives below `<repo>/src`, while the shipped entry point is
 * `<skill>/scripts/dcu.mjs`. These are the only two implicit locations: the
 * caller's current directory is never searched for a convenient `.env` file.
 */
export function defaultAgentEnvFile(moduleUrl: string | URL = import.meta.url): string {
  const modulePath = fileURLToPath(moduleUrl);
  const moduleDirectory = dirname(modulePath);
  const isBundledEntrypoint = basename(modulePath) === "dcu.mjs" && basename(moduleDirectory) === "scripts";
  if (isBundledEntrypoint) return resolve(moduleDirectory, "..", ".env");
  return resolve(moduleDirectory, "..", "..", "skills", "desktop-computer-use", ".env");
}

function configuredFile(options: LoadAgentEnvOptions, env: NodeJS.ProcessEnv): {
  path: string;
  source: AgentEnvSource;
  required: boolean;
} {
  const cwd = options.cwd ?? process.cwd();
  const cliFile = options.envFile?.trim();
  if (cliFile) return { path: resolve(cwd, cliFile), source: "cli", required: true };

  const environmentFile = env.DCU_ENV_FILE?.trim();
  if (environmentFile) {
    return { path: resolve(cwd, environmentFile), source: "environment", required: true };
  }

  return {
    path: defaultAgentEnvFile(options.moduleUrl ?? import.meta.url),
    source: "default",
    required: false
  };
}

function isMissingFile(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

/**
 * Merge one selected dotenv file into the supplied environment. Values already
 * present in the process environment, including empty strings, always win.
 */
export async function loadAgentEnv(options: LoadAgentEnvOptions = {}): Promise<AgentEnvLoadResult> {
  const env = options.env ?? process.env;
  const selected = configuredFile(options, env);
  let contents: string;
  try {
    contents = await readFile(selected.path, "utf8");
  } catch (error) {
    if (!selected.required && isMissingFile(error)) {
      return { path: selected.path, source: selected.source, found: false, loadedKeys: [] };
    }
    throw new DcuError(
      "agent_config",
      `Could not read environment file ${selected.path}: ${asError(error).message}`
    );
  }

  let parsed: NodeJS.Dict<string>;
  try {
    parsed = parseEnv(contents);
  } catch (error) {
    throw new DcuError(
      "agent_config",
      `Could not parse environment file ${selected.path}: ${asError(error).message}`
    );
  }

  const loadedKeys: string[] = [];
  for (const [key, value] of Object.entries(parsed)) {
    if (value === undefined || env[key] !== undefined) continue;
    env[key] = value;
    loadedKeys.push(key);
  }
  return { path: selected.path, source: selected.source, found: true, loadedKeys };
}

import { DcuError } from "../errors.js";
import { jevHost, readJevConfig } from "./jev.js";

/** The outer skill either drives the native commands itself or delegates one atomic goal to Jev. */
export type HarnessMode = "llm" | "llm+jev";

export interface HarnessConfigReport {
  mode: HarnessMode;
  /** The LLM is the model already running the skill; there is no second provider to configure. */
  llm: "harness";
  jev: {
    required: boolean;
    apiKeyPresent: boolean;
    host: string;
    model: string;
    timeoutMs: number;
  };
  /** Required credentials are present. This offline report does not test connectivity. */
  ready: boolean;
}

/**
 * This mode belongs to the skill harness, not to the native agent loop. The
 * standalone `agent run`, `agent decide`, and `agent doctor` commands remain Jev
 * helpers; hybrid mode delegates an atomic subgoal to one of those helpers.
 */
export function readHarnessMode(env: NodeJS.ProcessEnv = process.env): HarnessMode {
  const value = env.DCU_AGENT_MODE?.trim().toLowerCase();
  if (value === undefined || value === "") return "llm";
  if (value === "llm" || value === "llm+jev") return value;
  throw new DcuError(
    "agent_config",
    `DCU_AGENT_MODE must be llm or llm+jev, not ${JSON.stringify(env.DCU_AGENT_MODE)}`
  );
}

/** A credential-safe report for `agent config`; the key value never leaves this module. */
export function readHarnessConfig(env: NodeJS.ProcessEnv = process.env): HarnessConfigReport {
  const mode = readHarnessMode(env);
  const config = readJevConfig(env);
  const apiKeyPresent = Boolean(config.apiKey);
  const required = mode === "llm+jev";
  return {
    mode,
    llm: "harness",
    jev: {
      required,
      apiKeyPresent,
      host: jevHost(config),
      model: config.model,
      timeoutMs: config.timeoutMs
    },
    ready: !required || apiKeyPresent
  };
}

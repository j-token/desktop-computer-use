import { stdin, stdout } from "node:process";
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { DcuClient, errorToResponse } from "./client.js";
import { DcuError } from "./errors.js";
import type { AgentConfig, AgentEvent } from "./agent/agent.js";
import { SESSIONLESS_METHODS, WINDOW_TARGET_METHODS } from "./methods.js";
import {
  clearSession,
  readSession,
  resolveRuntimePaths,
  writeSession
} from "./runtime.js";
import { runLocalSetup } from "./setup.js";
import type { JsonObject, JsonValue, NativeResponse } from "./types.js";

type OptionValue = string | number | boolean;

const OPTION_DEFS: Record<string, { key: string; type: "string" | "number" | "boolean" }> = {
  "app": { key: "app", type: "string" },
  "window-id": { key: "windowId", type: "string" },
  "observation-id": { key: "observationId", type: "string" },
  "element-index": { key: "elementIndex", type: "number" },
  "from-element-index": { key: "fromElementIndex", type: "number" },
  "to-element-index": { key: "toElementIndex", type: "number" },
  "x": { key: "x", type: "number" },
  "y": { key: "y", type: "number" },
  "from-x": { key: "fromX", type: "number" },
  "from-y": { key: "fromY", type: "number" },
  "to-x": { key: "toX", type: "number" },
  "to-y": { key: "toY", type: "number" },
  "button": { key: "button", type: "string" },
  "mouse-button": { key: "button", type: "string" },
  "duration-ms": { key: "durationMs", type: "number" },
  "steps": { key: "steps", type: "number" },
  "hold-before-ms": { key: "holdBeforeMs", type: "number" },
  "hold-after-ms": { key: "holdAfterMs", type: "number" },
  "direction": { key: "direction", type: "string" },
  "amount": { key: "amount", type: "number" },
  "text": { key: "text", type: "string" },
  "key": { key: "key", type: "string" },
  "value": { key: "value", type: "string" },
  "include-text": { key: "includeText", type: "boolean" },
  "include-screenshot": { key: "includeScreenshot", type: "boolean" },
  "screenshot": { key: "includeScreenshot", type: "boolean" },
  "format": { key: "format", type: "string" },
  "quality": { key: "quality", type: "number" },
  "max-edge": { key: "maxEdge", type: "number" },
  "observe": { key: "observe", type: "string" },
  "goal": { key: "goal", type: "string" },
  "env-file": { key: "envFile", type: "string" },
  "mode": { key: "mode", type: "string" },
  "max-candidates": { key: "maxCandidates", type: "number" },
  "min-confidence": { key: "minConfidence", type: "number" },
  "min-margin": { key: "minMargin", type: "number" },
  "save-exchange": { key: "saveExchange", type: "string" },
  "fixture": { key: "fixture", type: "string" },
  "max-steps": { key: "maxSteps", type: "number" },
  "dry-run": { key: "dryRun", type: "boolean" },
  "stream": { key: "stream", type: "boolean" },
  "on-destructive": { key: "onDestructive", type: "string" },
  // `--no-values` strips every value from the state before it leaves the machine.
  "values": { key: "values", type: "boolean" },
  "start-session": { key: "startSession", type: "boolean" },
  "resume-session": { key: "resumeSession", type: "boolean" },
  "expect-element-name": { key: "expectElementName", type: "string" },
  "session-id": { key: "sessionId", type: "string" },
  "modifiers": { key: "modifiers", type: "string" },
  "restore-window": { key: "restoreWindow", type: "boolean" },
  "activate": { key: "activate", type: "boolean" },
  "text-stdin": { key: "textStdin", type: "boolean" },
  "value-stdin": { key: "valueStdin", type: "boolean" },
  "timeout-ms": { key: "timeoutMs", type: "number" },
  "json": { key: "json", type: "boolean" },
  "pretty": { key: "pretty", type: "boolean" },
  "help": { key: "help", type: "boolean" }
};

interface ParsedArgs {
  command: string[];
  positional: string[];
  options: Record<string, OptionValue>;
  /**
   * Every occurrence of every option, in the order it was typed. `options` keeps
   * the last one, so nothing that reads it changes; a repeatable flag such as
   * `agent run --text key=value` reads this instead.
   */
  repeated: Record<string, OptionValue[]>;
}

function parseNumber(name: string, value: string): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) throw new DcuError("invalid_argument", `--${name} must be a finite number`);
  return parsed;
}

function parseBoolean(name: string, value: string | undefined): boolean {
  if (value === undefined) return true;
  if (value === "true" || value === "1") return true;
  if (value === "false" || value === "0") return false;
  throw new DcuError("invalid_argument", `--${name} expects true or false`);
}

function optionValue(name: string, raw: string | undefined): OptionValue {
  const definition = OPTION_DEFS[name];
  if (!definition) throw new DcuError("invalid_argument", `Unknown option --${name}`);
  if (definition.type === "boolean") return parseBoolean(name, raw);
  if (raw === undefined || raw === "") throw new DcuError("invalid_argument", `--${name} requires a value`);
  return definition.type === "number" ? parseNumber(name, raw) : raw;
}

function looksLikeOption(value: string): boolean {
  return value.startsWith("--") && !/^-\d+(?:\.\d+)?$/.test(value);
}

export function parseArgs(argv: string[]): ParsedArgs {
  const command: string[] = [];
  const positional: string[] = [];
  const options: Record<string, OptionValue> = {};
  const repeated: Record<string, OptionValue[]> = {};
  let optionsEnded = false;
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (optionsEnded) {
      positional.push(token);
      continue;
    }
    if (token === "--") {
      optionsEnded = true;
      continue;
    }
    if (!token.startsWith("--")) {
      const allowsSubcommand = command.length === 1 && ["session", "daemon", "mcp", "agent"].includes(command[0]);
      if (command.length === 0 || allowsSubcommand) {
        command.push(token);
      } else {
        positional.push(token);
      }
      continue;
    }
    const equals = token.indexOf("=");
    const rawName = equals >= 0 ? token.slice(2, equals) : token.slice(2);
    const negated = rawName.startsWith("no-");
    const name = negated ? rawName.slice(3) : rawName;
    const definition = OPTION_DEFS[name];
    if (!definition) throw new DcuError("invalid_argument", `Unknown option ${token}`);
    let rawValue: string | undefined = equals >= 0 ? token.slice(equals + 1) : undefined;
    if (
      rawValue === undefined &&
      definition.type !== "boolean" &&
      i + 1 < argv.length &&
      !looksLikeOption(argv[i + 1])
    ) {
      rawValue = argv[++i];
    }
    if (negated && definition.type !== "boolean") {
      throw new DcuError("invalid_argument", `--no-${name} is only valid for boolean options`);
    }
    const value = negated ? false : optionValue(name, rawValue);
    options[definition.key] = value;
    (repeated[definition.key] ??= []).push(value);
  }
  return { command, positional, options, repeated };
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function setIfDefined(params: JsonObject, key: string, value: OptionValue | undefined): void {
  if (value !== undefined) params[key] = value as JsonValue;
}

function validateChoice(params: JsonObject, key: string, choices: readonly string[]): void {
  const value = params[key];
  if (value !== undefined && !choices.includes(String(value))) {
    throw new DcuError("invalid_argument", `${key} must be ${choices.join(", ")}`);
  }
}

function validateNonNegativeNumber(params: JsonObject, key: string): void {
  const value = params[key];
  if (value !== undefined && (!Number.isFinite(Number(value)) || Number(value) < 0)) {
    throw new DcuError("invalid_argument", `${key} must be a non-negative number`);
  }
}

function validateNonNegativeInteger(params: JsonObject, key: string): void {
  const value = params[key];
  if (value !== undefined && (!Number.isInteger(value) || Number(value) < 0)) {
    throw new DcuError("invalid_argument", `${key} must be a non-negative integer`);
  }
}

function validateMaximum(params: JsonObject, key: string, maximum: number, message: string): void {
  const value = params[key];
  if (value !== undefined && Number(value) > maximum) {
    throw new DcuError("invalid_argument", message);
  }
}

function validateCommon(params: JsonObject, method: string): void {
  validateChoice(params, "button", ["left", "right", "middle"]);
  validateChoice(params, "direction", ["up", "down", "left", "right"]);
  validateChoice(params, "observe", ["none", "screenshot", "text", "both"]);
  validateChoice(params, "format", ["jpeg", "png"]);

  const hasValidQuality = params.quality === undefined || (
    Number.isInteger(params.quality) &&
    Number(params.quality) >= 1 &&
    Number(params.quality) <= 100
  );
  if (!hasValidQuality) {
    throw new DcuError("invalid_argument", "quality must be an integer between 1 and 100");
  }
  for (const key of ["durationMs", "steps", "holdBeforeMs", "holdAfterMs", "maxEdge", "amount"]) {
    validateNonNegativeNumber(params, key);
  }
  for (const key of ["x", "y", "fromX", "fromY", "toX", "toY"]) {
    validateNonNegativeNumber(params, key);
  }
  for (const key of ["elementIndex", "fromElementIndex", "toElementIndex"]) {
    validateNonNegativeInteger(params, key);
  }

  if (params.amount !== undefined && (Number(params.amount) < 1 || Number(params.amount) > 1000)) {
    throw new DcuError("invalid_argument", "amount must be between 1 and 1000");
  }
  validateMaximum(params, "maxEdge", 16384, "maxEdge must be at most 16384");
  for (const key of ["durationMs", "holdBeforeMs", "holdAfterMs"]) {
    validateMaximum(params, key, 10000, `${key} must be at most 10000`);
  }
  validateMaximum(params, "steps", 1000, "steps must be at most 1000");

  const hasElementIndex =
    params.elementIndex !== undefined ||
    params.fromElementIndex !== undefined ||
    params.toElementIndex !== undefined;
  if (hasElementIndex && params.observationId === undefined) {
    throw new DcuError("invalid_argument", "element indexes require --observation-id");
  }
  if (
    method === "drag" &&
    params.steps !== undefined &&
    (!Number.isInteger(params.steps) || Number(params.steps) < 1)
  ) {
    throw new DcuError("invalid_argument", "steps must be a positive integer");
  }
}

function paramsFromOptions(options: Record<string, OptionValue>, method: string): JsonObject {
  const params: JsonObject = {};
  const excluded = new Set([
    "json",
    "pretty",
    "help",
    "timeoutMs",
    "textStdin",
    "valueStdin",
    "goal",
    "envFile",
    "mode",
    "fixture",
    "maxCandidates",
    "minConfidence",
    "minMargin",
    "saveExchange",
    "maxSteps",
    "dryRun",
    "stream",
    "onDestructive",
    "values",
    "startSession",
    "resumeSession",
    "expectElementName"
  ]);
  for (const [key, value] of Object.entries(options)) {
    if (!excluded.has(key)) setIfDefined(params, key, value);
  }
  if (method === "get-app-state") {
    if (options.includeScreenshot === undefined) {
      params.includeScreenshot = true;
    }
    if (options.includeText === undefined) {
      params.includeText = false;
    }
  } else if (typeof options.observe === "string") {
    if (options.includeScreenshot === undefined) {
      params.includeScreenshot = options.observe === "screenshot" || options.observe === "both";
    }
    if (options.includeText === undefined) {
      params.includeText = options.observe === "text" || options.observe === "both";
    }
  }
  return params;
}

async function readStdinText(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of stdin) chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk)));
  return Buffer.concat(chunks).toString("utf8");
}

function methodFor(command: string[]): string {
  if (command.length === 1) return command[0];
  if (command[0] === "session" && ["start", "status", "stop"].includes(command[1])) return `session.${command[1]}`;
  if (command[0] === "daemon" && command[1] === "shutdown") return "daemon.shutdown";
  throw new DcuError("invalid_argument", `Unknown command ${command.join(" ")}`);
}

function helpText(): string {
  return [
    "desktop-computer-use — standalone native desktop control",
    "",
    "Usage: dcu <command> [options]",
    "",
    "Commands: setup, doctor, capabilities, session start|status|stop, daemon shutdown,",
    "  list-apps, list-windows, get-app-state,",
    "  click, drag, scroll, type-text, press-key, hotkey, set-value, paste-text, mcp serve,",
    "  agent config, agent explain, agent decide, agent run, agent doctor",
    "",
    "`agent config [--mode llm|llm+jev] [--env-file FILE]` reports the harness workflow and",
    "Jev key presence offline. The LLM is the model already running this skill; the default mode",
    "is llm. The override applies to this report only and does not persist configuration.",
    "",
    "`agent run --goal \"...\" --app APP|--window-id ID` is the Jev helper loop: it observes, asks Jev",
    "for one action, checks that action against the guards, sends it, and compares the observation",
    "before with the observation after. It prints exactly one JSON line and exits 0 only when the",
    "run succeeded. Options: --max-steps N (default 25, ceiling 100), --dry-run (observe once,",
    "decide once, act zero times), --stream (one NDJSON record per step, summary still last),",
    "--on-destructive stop|confirm|allow (default stop; confirm degrades to stop, never to allow),",
    "--text field=value (repeatable, for text the model cannot generate), --no-values,",
    "--start-session, --resume-session, --expect-element-name REGEX (the objective success check).",
    "--app matches the executable name first and the window title second, so a packaged app such",
    "as the Calculator can be named the way its title bar names it.",
    "Each run appends an NDJSON trace under the runtime directory; DCU_AGENT_TRACE_DIR moves it.",
    "",
    "`agent explain --app APP|--window-id ID|--fixture FILE [--goal \"...\"]` turns one observation",
    "into a short candidate list offline: no model, no network, no API key.",
    "",
    "`agent decide --goal \"...\" --app APP|--window-id ID|--fixture FILE` asks the hosted Jev model",
    "which single action comes next and prints it. It decides and reports only: it never clicks,",
    "types or scrolls. Tune the gate with --min-confidence and --min-margin, record the exchange",
    "with --save-exchange DIR.",
    "",
    "Agent commands merge the skill-root .env, or --env-file/DCU_ENV_FILE, before reading settings;",
    "same-name process environment values win and the current directory is never searched.",
    "`agent doctor` reports whether the Jev key is present, which host and model would be",
    "used, and how long one small request takes. It never prints the key itself.",
    "",
    "Actions use a saved session from `session start`; pass --session-id to override it.",
    "Coordinates are window-local logical coordinates. Observations are returned as JSON with screenshot paths.",
    "Drag defaults: 240ms, 12 steps, 50ms hold before and after. Use --duration-ms/--steps to tune them.",
    "Active sessions show a top banner, high-contrast cursor ring, and blue inward-fading screen-edge border.",
    "Emergency stop: press Esc or run `dcu session stop`."
  ].join("\n");
}

function sessionRequired(method: string): boolean {
  return !SESSIONLESS_METHODS.has(method);
}

function appRequired(method: string): boolean {
  return WINDOW_TARGET_METHODS.has(method);
}

function validateMethodParams(method: string, params: JsonObject): void {
  validateCommon(params, method);
  if (sessionRequired(method) && typeof params.sessionId !== "string") {
    throw new DcuError("session_required", "Start a computer-use session first or pass --session-id");
  }
  if (appRequired(method) && typeof params.app !== "string" && typeof params.windowId !== "string") {
    throw new DcuError("invalid_argument", `${method} requires --app or --window-id`);
  }
  if (method === "drag") {
    const byElements = params.fromElementIndex !== undefined || params.toElementIndex !== undefined;
    const byCoordinates = ["fromX", "fromY", "toX", "toY"].some(key => params[key] !== undefined);
    if (byElements && (params.fromElementIndex === undefined || params.toElementIndex === undefined)) {
      throw new DcuError("invalid_argument", "drag requires both from/to element indexes");
    }
    if (byCoordinates && ["fromX", "fromY", "toX", "toY"].some(key => params[key] === undefined)) {
      throw new DcuError("invalid_argument", "drag requires from-x, from-y, to-x, and to-y");
    }
    if (!byElements && !byCoordinates) {
      throw new DcuError("invalid_argument", "drag requires element indexes or coordinates");
    }
    params.durationMs ??= 240;
    params.steps ??= 12;
    params.holdBeforeMs ??= 50;
    params.holdAfterMs ??= 50;
  }
  if (method === "scroll") {
    if (params.direction === undefined) {
      throw new DcuError("invalid_argument", "scroll requires --direction");
    }
    if (params.x === undefined || params.y === undefined) {
      throw new DcuError("invalid_argument", "scroll requires --x and --y");
    }
    params.amount ??= 3;
  }
  if ((method === "type-text" || method === "paste-text") && params.text === undefined) {
    throw new DcuError("invalid_argument", `${method} requires --text or --text-stdin`);
  }
  if ((method === "press-key" || method === "hotkey") && params.key === undefined) {
    throw new DcuError("invalid_argument", `${method} requires --key`);
  }
  if (method === "set-value") {
    if (params.elementIndex === undefined) throw new DcuError("invalid_argument", "set-value requires --element-index");
    if (params.value === undefined) {
      throw new DcuError("invalid_argument", "set-value requires --value or --value-stdin");
    }
  }
}

function output(value: unknown, pretty: boolean): void {
  stdout.write(`${JSON.stringify(value, null, pretty ? 2 : 0)}\n`);
}

function optionalString(value: OptionValue | undefined): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function validateAgentOptionScope(parsed: ParsedArgs): void {
  const isAgentCommand = parsed.command[0] === "agent";
  const isAgentConfig = isAgentCommand && parsed.command[1] === "config";
  if (parsed.options.envFile !== undefined && !isAgentCommand) {
    throw new DcuError("invalid_argument", "--env-file is only valid with agent commands");
  }
  if (parsed.options.mode !== undefined && !isAgentConfig) {
    throw new DcuError("invalid_argument", "--mode is only valid with agent config");
  }
}

function harnessMode(value: OptionValue | undefined): "llm" | "llm+jev" | undefined {
  const mode = optionalString(value);
  if (mode === undefined || mode === "llm" || mode === "llm+jev") return mode;
  throw new DcuError("invalid_argument", "--mode must be llm or llm+jev");
}

async function runAgentConfig(parsed: ParsedArgs): Promise<number> {
  const pretty = Boolean(parsed.options.pretty);
  const mode = harnessMode(parsed.options.mode);
  const { readHarnessConfig } = await import("./agent/config.js");
  const result = readHarnessConfig(mode === undefined
    ? process.env
    : { ...process.env, DCU_AGENT_MODE: mode });
  output({ id: null, ok: result.ready, result }, pretty);
  return result.ready ? 0 : 1;
}

/**
 * `agent decide` is the online half: it observes once (or reads a recorded
 * fixture), asks the hosted model which single action comes next, and prints the
 * answer. It performs no input — no click, no keystroke, no scroll.
 */
async function runAgentDecide(parsed: ParsedArgs): Promise<number> {
  const pretty = Boolean(parsed.options.pretty);
  const { decide, renderDecision } = await import("./agent/decide.js");
  const goal = optionalString(parsed.options.goal);
  const fixturePath = optionalString(parsed.options.fixture);
  const options: Parameters<typeof decide>[0] = { goal: goal ?? "" };
  if (fixturePath !== undefined) options.fixturePath = fixturePath;
  const app = optionalString(parsed.options.app) ?? (fixturePath === undefined ? parsed.positional[0] : undefined);
  if (app !== undefined) options.app = app;
  const windowId = optionalString(parsed.options.windowId);
  if (windowId !== undefined) options.windowId = windowId;
  const saveExchange = optionalString(parsed.options.saveExchange);
  if (saveExchange !== undefined) options.saveExchangeDir = saveExchange;
  if (typeof parsed.options.maxCandidates === "number") options.maxCandidates = parsed.options.maxCandidates;
  if (typeof parsed.options.minConfidence === "number") options.minConfidence = parsed.options.minConfidence;
  if (typeof parsed.options.minMargin === "number") options.minMargin = parsed.options.minMargin;
  if (typeof parsed.options.timeoutMs === "number") options.timeoutMs = parsed.options.timeoutMs;
  if (fixturePath === undefined) {
    const paths = resolveRuntimePaths();
    const sessionId = optionalString(parsed.options.sessionId) ?? (await readSession(paths))?.sessionId;
    if (sessionId !== undefined) options.sessionId = sessionId;
  }
  let result;
  try {
    result = await decide(options);
  } catch (error) {
    output({ id: null, ...errorToResponse(error) }, pretty);
    return 1;
  }
  if (pretty) process.stderr.write(`${renderDecision(result)}\n\n`);
  output({ id: null, ok: true, result }, pretty);
  return result.decision ? 0 : 1;
}

function destructiveMode(value: OptionValue | undefined): "stop" | "confirm" | "allow" {
  const raw = optionalString(value) ?? "stop";
  if (raw === "stop" || raw === "confirm" || raw === "allow") return raw;
  throw new DcuError("invalid_argument", "--on-destructive must be stop, confirm or allow");
}

/** `--text label=value`, repeated once per field the run may have to type into. */
function textPresets(values: OptionValue[] | undefined): Record<string, string> {
  const presets: Record<string, string> = {};
  for (const value of values ?? []) {
    const raw = String(value);
    const equals = raw.indexOf("=");
    if (equals <= 0) {
      throw new DcuError("invalid_argument", `--text expects a field=value pair, not "${raw}"`);
    }
    presets[raw.slice(0, equals)] = raw.slice(equals + 1);
  }
  return presets;
}

function agentRunConfig(parsed: ParsedArgs, sessionId: string | undefined): AgentConfig {
  const config: AgentConfig = {};
  const app = optionalString(parsed.options.app) ?? parsed.positional[0];
  const windowId = optionalString(parsed.options.windowId);
  if (app === undefined && windowId === undefined) {
    throw new DcuError("invalid_argument", "agent run requires --app or --window-id");
  }
  if (app !== undefined) config.app = app;
  if (windowId !== undefined) config.windowId = windowId;
  if (sessionId !== undefined) config.sessionId = sessionId;
  if (typeof parsed.options.maxSteps === "number") config.maxSteps = parsed.options.maxSteps;
  if (typeof parsed.options.minConfidence === "number") config.minConfidence = parsed.options.minConfidence;
  if (typeof parsed.options.minMargin === "number") config.minMargin = parsed.options.minMargin;
  if (typeof parsed.options.maxCandidates === "number") config.maxCandidates = parsed.options.maxCandidates;
  if (typeof parsed.options.timeoutMs === "number") config.timeoutMs = parsed.options.timeoutMs;
  config.onDestructive = destructiveMode(parsed.options.onDestructive);
  config.texts = textPresets(parsed.repeated.text);
  if (parsed.options.values === false) config.noValues = true;
  if (parsed.options.startSession === true) config.startSession = true;
  if (parsed.options.resumeSession === true) config.resumeSession = true;
  const expect = optionalString(parsed.options.expectElementName);
  if (expect !== undefined) config.expectElementName = expect;
  // There is no TTY under MCP, and this loop owns the desktop while it runs, so
  // `--on-destructive confirm` has nowhere to ask and degrades to a stop.
  config.isTty = Boolean(stdin.isTTY && stdout.isTTY);
  return config;
}

/**
 * `agent run` is the loop: observe, decide, guard, act, compare. Default output
 * is exactly one JSON line and exit 0 only on `succeeded`. `--stream` adds one
 * NDJSON record per step ahead of it, and the last line stays the same envelope
 * so a consumer that reads only the last line keeps working.
 */
async function runAgentRun(parsed: ParsedArgs): Promise<number> {
  const streaming = Boolean(parsed.options.stream);
  const pretty = Boolean(parsed.options.pretty) && !streaming;
  const goal = optionalString(parsed.options.goal);
  if (!goal) throw new DcuError("invalid_argument", "agent run requires --goal");

  const paths = resolveRuntimePaths();
  const saved = await readSession(paths);
  const sessionId = optionalString(parsed.options.sessionId) ?? saved?.sessionId;
  const config = agentRunConfig(parsed, sessionId);
  const client = new DcuClient(paths);
  const { Agent, dryRun } = await import("./agent/agent.js");
  const { createTrace, nullTrace } = await import("./agent/trace.js");

  if (parsed.options.dryRun) {
    try {
      const result = await dryRun({ client, trace: nullTrace() }, config, goal);
      output({ id: null, ok: true, result }, pretty);
      return result.decision ? 0 : 1;
    } catch (error) {
      output({ id: null, ...errorToResponse(error) }, pretty);
      return 1;
    }
  }

  const runId = randomUUID();
  config.runId = runId;
  if (streaming) {
    config.stream = (event: AgentEvent): void => {
      stdout.write(`${JSON.stringify(event)}\n`);
    };
  }
  const trace = await createTrace({ runId });
  let result;
  try {
    result = await new Agent({ client, trace }, config).run(goal);
  } catch (error) {
    await trace.close();
    output({ id: null, ...errorToResponse(error) }, pretty);
    return 1;
  }
  // A run that started or resumed a session leaves the CLI's saved session behind.
  if (result.sessionId && result.sessionId !== saved?.sessionId) {
    await writeSession(result.sessionId, paths);
  }
  const ok = result.status === "succeeded";
  output({ id: null, ok, result }, pretty);
  return ok ? 0 : 1;
}

async function runAgentDoctor(parsed: ParsedArgs): Promise<number> {
  const pretty = Boolean(parsed.options.pretty);
  const { doctor } = await import("./agent/decide.js");
  const result = await doctor();
  output({ id: null, ok: result.ok, result }, pretty);
  return result.ok ? 0 : 1;
}

/**
 * `agent explain` is the offline half: it observes once (or reads a recorded
 * fixture) and turns the raw element list into a short candidate list. No model,
 * no network. The human table goes to stderr so stdout stays the JSON contract.
 */
async function runAgentCommand(parsed: ParsedArgs): Promise<number> {
  if (parsed.command[1] === "config") return runAgentConfig(parsed);
  if (parsed.command[1] === "decide") return runAgentDecide(parsed);
  if (parsed.command[1] === "run") return runAgentRun(parsed);
  if (parsed.command[1] === "doctor") return runAgentDoctor(parsed);
  if (parsed.command[1] !== "explain") {
    throw new DcuError("invalid_argument", `Unknown command ${parsed.command.join(" ")}`);
  }
  const pretty = Boolean(parsed.options.pretty);
  const fixturePath = optionalString(parsed.options.fixture);
  const { explain, renderTable } = await import("./agent/explain.js");
  const options: Parameters<typeof explain>[0] = {};
  if (fixturePath !== undefined) options.fixturePath = fixturePath;
  const app = optionalString(parsed.options.app) ?? (fixturePath === undefined ? parsed.positional[0] : undefined);
  if (app !== undefined) options.app = app;
  const windowId = optionalString(parsed.options.windowId);
  if (windowId !== undefined) options.windowId = windowId;
  const goal = optionalString(parsed.options.goal);
  if (goal !== undefined) options.goal = goal;
  if (typeof parsed.options.maxCandidates === "number") options.maxCandidates = parsed.options.maxCandidates;
  if (typeof parsed.options.timeoutMs === "number") options.timeoutMs = parsed.options.timeoutMs;
  if (fixturePath === undefined) {
    const paths = resolveRuntimePaths();
    const sessionId = optionalString(parsed.options.sessionId) ?? (await readSession(paths))?.sessionId;
    if (sessionId !== undefined) options.sessionId = sessionId;
  }
  let result;
  try {
    result = await explain(options);
  } catch (error) {
    output({ id: null, ...errorToResponse(error) }, pretty);
    return 1;
  }
  if (pretty) process.stderr.write(`${renderTable(result)}\n\n`);
  output({ id: null, ok: true, result }, pretty);
  return 0;
}

export async function runCli(argv = process.argv.slice(2)): Promise<number> {
  try {
    const parsed = parseArgs(argv);
    validateAgentOptionScope(parsed);
    if (parsed.options.help || parsed.command.length === 0) {
      stdout.write(`${helpText()}\n`);
      return 0;
    }
    if (parsed.command[0] === "mcp" && parsed.command[1] === "serve") {
      const { runMcpServer } = await import("./mcp.js");
      await runMcpServer(new DcuClient());
      return 0;
    }
    if (parsed.command[0] === "agent") {
      const { loadAgentEnv } = await import("./agent/env.js");
      await loadAgentEnv({ envFile: optionalString(parsed.options.envFile) });
      return await runAgentCommand(parsed);
    }
    if (parsed.command[0] === "setup") {
      output({ id: null, ok: true, result: await runLocalSetup() }, Boolean(parsed.options.pretty));
      return 0;
    }
    const method = methodFor(parsed.command);
    const params = paramsFromOptions(parsed.options, method);
    const acceptsPositionalApp = method === "get-app-state" || method === "list-windows";
    if (acceptsPositionalApp && parsed.positional.length > 0 && params.app === undefined) {
      params.app = parsed.positional[0];
    }
    if (parsed.options.textStdin) {
      params.text = await readStdinText();
    }
    if (parsed.options.valueStdin) {
      params.value = await readStdinText();
    }
    const paths = resolveRuntimePaths();
    const saved = await readSession(paths);
    const canUseSavedSession = sessionRequired(method) || method === "session.status";
    if (canUseSavedSession && params.sessionId === undefined && saved) {
      params.sessionId = saved.sessionId;
    }
    validateMethodParams(method, params);
    const client = new DcuClient(paths);
    const timeoutMs = typeof parsed.options.timeoutMs === "number" ? parsed.options.timeoutMs : undefined;
    let response: NativeResponse;
    try {
      response = await client.request(method, params, { timeoutMs });
    } catch (error) {
      output({ id: null, ...errorToResponse(error) }, Boolean(parsed.options.pretty));
      return 1;
    }
    if (response.ok && method === "session.start") {
      const result = asRecord(response.result);
      if (typeof result.sessionId === "string") await writeSession(result.sessionId, paths);
    } else if (response.ok && method === "session.stop") {
      await clearSession(paths);
    }
    output(response, Boolean(parsed.options.pretty));
    return response.ok ? 0 : 1;
  } catch (error) {
    output({ id: null, ...errorToResponse(error) }, false);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  runCli().then(code => {
    process.exitCode = code;
  });
}

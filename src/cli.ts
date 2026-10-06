import { stdin, stdout } from "node:process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { DcuClient, errorToResponse } from "./client.js";
import { DcuError } from "./errors.js";
import {
  SESSIONLESS_METHODS,
  TOGGLE_KEY_ALIASES,
  WINDOW_TARGET_METHODS,
  isValidModifiers
} from "./methods.js";
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
  "coords": { key: "coords", type: "string" },
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
  "session-id": { key: "sessionId", type: "string" },
  "modifiers": { key: "modifiers", type: "string" },
  "all": { key: "all", type: "boolean" },
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
      const allowsSubcommand = command.length === 1 && ["session", "daemon", "mcp", "toggle"].includes(command[0]);
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
  }
  return { command, positional, options };
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
  validateChoice(params, "coords", ["reduced", "full"]);
  if (params.modifiers !== undefined && !isValidModifiers(String(params.modifiers))) {
    throw new DcuError("invalid_argument", "modifiers must join shift, ctrl, alt, or win with +");
  }

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
    "all"
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

function toggleMethod(action: string | undefined, options: Record<string, OptionValue>): string {
  if (action === "status") return "toggle.status";
  if (action !== "on" && action !== "off") {
    throw new DcuError("invalid_argument", "Use `toggle on --key K`, `toggle off --key K`, `toggle off --all`, or `toggle status`");
  }
  if (options.all === true) {
    if (action === "on" || options.key !== undefined) {
      throw new DcuError("invalid_argument", "--all is only valid as `toggle off --all` without --key");
    }
    return "toggle.release-all";
  }
  if (typeof options.key !== "string") {
    throw new DcuError("invalid_argument", `toggle ${action} requires --key shift|ctrl|alt|win|space`);
  }
  if (!TOGGLE_KEY_ALIASES.includes(options.key.toLowerCase())) {
    throw new DcuError("invalid_argument", "--key must be shift, ctrl, alt, win, or space");
  }
  return "toggle.set";
}

export function methodFor(command: string[], options: Record<string, OptionValue> = {}): string {
  if (command[0] === "toggle") return toggleMethod(command[1], options);
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
    "  list-apps, list-windows, get-app-state, get-full-screenshot,",
    "  click, drag, scroll, type-text, press-key, hotkey, set-value, paste-text,",
    "  toggle on|off --key shift|ctrl|alt|win|space, toggle off --all, toggle status, mcp serve",
    "",
    "Actions use a saved session from `session start`; pass --session-id to override it.",
    "Observations are returned as JSON with a reduced screenshot path (0.5x above 1280x720).",
    "x/y coordinates are pixels of the window's latest reduced screenshot (or --observation-id).",
    "`get-full-screenshot --observation-id ID` returns the original image; click from it with --coords full.",
    "Drag defaults: 240ms, 12 steps, 50ms hold before and after. Use --duration-ms/--steps to tune them.",
    "click and drag accept --modifiers shift+ctrl to hold keys for that one action.",
    "`toggle on --key K` holds a key across actions; every result then lists `toggles` with a notice.",
    "`session stop` fails with toggles_active while a toggle is on; run `toggle off --all` first.",
    "Active sessions show a top banner, high-contrast cursor ring, and blue inward-fading screen-edge border.",
    "Emergency stop: press Esc twice within 1 second or run `dcu session stop`."
  ].join("\n");
}

// Decides how a native response changes the saved session file. A refused
// stop (for example toggles_active) keeps the session file.
export function sessionFileAction(method: string, response: NativeResponse): "write" | "clear" | undefined {
  if (!response.ok) return undefined;
  if (method === "session.start") return "write";
  if (method === "session.stop") return "clear";
  return undefined;
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
  if (method === "get-full-screenshot" && typeof params.observationId !== "string") {
    throw new DcuError("invalid_argument", "get-full-screenshot requires --observation-id");
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

export async function runCli(argv = process.argv.slice(2)): Promise<number> {
  try {
    const parsed = parseArgs(argv);
    if (parsed.options.help || parsed.command.length === 0) {
      stdout.write(`${helpText()}\n`);
      return 0;
    }
    if (parsed.command[0] === "mcp" && parsed.command[1] === "serve") {
      const { runMcpServer } = await import("./mcp.js");
      await runMcpServer(new DcuClient());
      return 0;
    }
    if (parsed.command[0] === "setup") {
      output({ id: null, ok: true, result: await runLocalSetup() }, Boolean(parsed.options.pretty));
      return 0;
    }
    const method = methodFor(parsed.command, parsed.options);
    const params = paramsFromOptions(parsed.options, method);
    if (method === "toggle.set") params.on = parsed.command[1] === "on";
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
    const sessionFile = sessionFileAction(method, response);
    if (sessionFile === "write") {
      const result = asRecord(response.result);
      if (typeof result.sessionId === "string") await writeSession(result.sessionId, paths);
    } else if (sessionFile === "clear") {
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

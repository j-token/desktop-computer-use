import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { DcuClient } from "../client.js";
import { DcuError } from "../errors.js";
import { explain } from "./explain.js";
import {
  DEFAULT_MIN_CONFIDENCE,
  DEFAULT_MIN_MARGIN,
  buildQuestions,
  parseAnswers
} from "./questions.js";
import type { Decision, GateFailure, QuestionPlan } from "./questions.js";
import { FetchJevTransport, jevHost, missingKeyError, readJevConfig } from "./jev.js";
import type { JevConfig, JevRequest, JevResponse, JevTransport } from "./jev.js";
import { estimateTokens } from "./state.js";
import type { HistoryStep, StateInputs } from "./state.js";

/**
 * A ceiling on the state alone, well inside the documented 32k that state plus the
 * longest single question must share. It is measured with the chars/4 estimate,
 * so it is a guard rail rather than a boundary.
 */
export const MAX_STATE_TOKENS = 24_000;

export interface DecideOptions {
  goal: string;
  app?: string;
  windowId?: string;
  sessionId?: string;
  fixturePath?: string;
  maxCandidates?: number;
  timeoutMs?: number;
  minConfidence?: number;
  minMargin?: number;
  history?: readonly HistoryStep[];
  notes?: readonly string[];
  saveExchangeDir?: string;
  client?: DcuClient;
  transport?: JevTransport;
  config?: JevConfig;
}

export interface DecideResult {
  source: "fixture" | "window";
  window: { app?: string; title?: string; id?: string };
  goal: string;
  model: string;
  /** chars/4, an estimate and not a count. */
  estimatedStateTokens: number;
  state: string[];
  questionKeys: string[];
  latencyMs: number;
  usage: JevResponse["usage"];
  decision?: Decision;
  gateFailure?: GateFailure;
  /** Always false: this stage decides and reports, and never touches the desktop. */
  performedInput: false;
}

export interface DoctorResult {
  apiKeyPresent: boolean;
  host: string;
  model: string;
  pingLatencyMs: number | null;
  ok: boolean;
  error?: { code: string; message: string };
}

function redactedExchange(url: string, request: JevRequest): unknown {
  return {
    url,
    method: "POST",
    headers: {
      "content-type": "application/json",
      // The key never reaches disk, a log line or an error message.
      authorization: "Bearer <redacted>"
    },
    body: request
  };
}

async function saveExchange(
  directory: string,
  url: string,
  request: JevRequest,
  response: JevResponse | undefined,
  error: unknown
): Promise<void> {
  await mkdir(directory, { recursive: true });
  const stamp = new Date().toISOString().replace(/[:.]/gu, "-");
  await writeFile(
    join(directory, `${stamp}-request.json`),
    `${JSON.stringify(redactedExchange(url, request), null, 2)}\n`,
    "utf8"
  );
  const body = response ?? {
    error: error instanceof DcuError
      ? { code: error.code, message: error.message }
      : { code: "agent_error", message: String(error) }
  };
  await writeFile(join(directory, `${stamp}-response.json`), `${JSON.stringify(body, null, 2)}\n`, "utf8");
}

/**
 * Observe once (or read a recorded observation), filter, ask Jev once, and report
 * what it decided. Nothing here presses, types or scrolls anything.
 */
export async function decide(options: DecideOptions): Promise<DecideResult> {
  const goal = options.goal?.trim();
  if (!goal) throw new DcuError("invalid_argument", "agent decide requires --goal");
  if (!options.fixturePath && options.app === undefined && options.windowId === undefined) {
    throw new DcuError("invalid_argument", "agent decide requires --app, --window-id, or --fixture");
  }

  const config = options.config ?? readJevConfig();
  // A missing key is worth saying before an observation is taken, not after.
  if (!options.transport && !config.apiKey) throw missingKeyError();

  const observed = await explain({
    goal,
    ...(options.fixturePath === undefined ? {} : { fixturePath: options.fixturePath }),
    ...(options.app === undefined ? {} : { app: options.app }),
    ...(options.windowId === undefined ? {} : { windowId: options.windowId }),
    ...(options.sessionId === undefined ? {} : { sessionId: options.sessionId }),
    ...(options.maxCandidates === undefined ? {} : { maxCandidates: options.maxCandidates }),
    ...(options.timeoutMs === undefined ? {} : { timeoutMs: options.timeoutMs }),
    ...(options.client === undefined ? {} : { client: options.client })
  });

  const inputs: StateInputs = {
    goal,
    window: {
      ...(observed.window.app === undefined ? {} : { app: observed.window.app }),
      ...(observed.window.title === undefined ? {} : { title: observed.window.title })
    },
    candidates: observed.candidates,
    disabledShortlist: observed.disabledShortlist,
    regions: observed.report.regions,
    history: options.history ?? [],
    notes: options.notes ?? [],
    gate: {
      minConfidence: options.minConfidence ?? DEFAULT_MIN_CONFIDENCE,
      minMargin: options.minMargin ?? DEFAULT_MIN_MARGIN
    }
  };

  const plan: QuestionPlan = buildQuestions(inputs);
  const estimatedStateTokens = estimateTokens(plan.state);
  if (estimatedStateTokens > MAX_STATE_TOKENS) {
    throw new DcuError(
      "agent_state_too_large",
      `The state is an estimated ${estimatedStateTokens} tokens, over the ${MAX_STATE_TOKENS} ceiling; filter harder`
    );
  }

  const transport = options.transport ?? new FetchJevTransport({ config });
  const request: JevRequest = { state: plan.state, model: config.model, questions: plan.questions };
  const url = `${config.baseUrl}/systemone`;
  const startedAt = performance.now();
  let response: JevResponse;
  try {
    response = await transport.ask(request, new AbortController().signal);
  } catch (error) {
    if (options.saveExchangeDir) await saveExchange(options.saveExchangeDir, url, request, undefined, error);
    throw error;
  }
  const latencyMs = Math.round(performance.now() - startedAt);
  if (options.saveExchangeDir) await saveExchange(options.saveExchangeDir, url, request, response, undefined);

  const parsed = parseAnswers(plan, response.answers);
  const result: DecideResult = {
    source: observed.source,
    window: {
      ...(observed.window.app === undefined ? {} : { app: observed.window.app }),
      ...(observed.window.title === undefined ? {} : { title: observed.window.title }),
      ...(observed.window.id === undefined ? {} : { id: observed.window.id })
    },
    goal,
    model: response.model || config.model,
    estimatedStateTokens,
    state: plan.state,
    questionKeys: Object.keys(plan.questions),
    latencyMs,
    usage: response.usage,
    performedInput: false
  };
  if (parsed.ok) result.decision = parsed;
  else result.gateFailure = parsed;
  return result;
}

/** A two-option choice on a one-line state: the smallest request that still proves the path. */
function pingRequest(model: string): JevRequest {
  return {
    state: ["This is a connection check from a desktop agent. Nothing is being decided."],
    model,
    questions: {
      reachable: {
        type: "choice",
        instructions: "Choose the description that fits the state above.",
        criteria: {
          check: "The text above describes a connection check and nothing more.",
          decision: "The text above describes a decision being made about a window."
        }
      }
    }
  };
}

export async function doctor(options: { config?: JevConfig; transport?: JevTransport } = {}): Promise<DoctorResult> {
  const config = options.config ?? readJevConfig();
  const result: DoctorResult = {
    apiKeyPresent: Boolean(config.apiKey),
    host: jevHost(config),
    model: config.model,
    pingLatencyMs: null,
    ok: false
  };
  if (!result.apiKeyPresent && !options.transport) {
    result.error = { code: "agent_config", message: missingKeyError().message };
    return result;
  }
  const transport = options.transport ?? new FetchJevTransport({ config });
  const startedAt = performance.now();
  try {
    await transport.ask(pingRequest(config.model), new AbortController().signal);
  } catch (error) {
    result.error = error instanceof DcuError
      ? { code: error.code, message: error.message }
      : { code: "agent_error", message: (error as Error).message };
    return result;
  }
  result.pingLatencyMs = Math.round(performance.now() - startedAt);
  result.ok = true;
  return result;
}

function percent(value: number): string {
  return `${Math.round(value * 1000) / 10}%`;
}

function vector(probabilities: unknown): string {
  if (!probabilities || typeof probabilities !== "object" || Array.isArray(probabilities)) return "—";
  return Object.entries(probabilities as Record<string, number>)
    .sort((left, right) => right[1] - left[1])
    .map(([key, value]) => `${key} ${percent(value)}`)
    .join(", ");
}

/** Human rendering for `--pretty`; the JSON envelope on stdout stays the machine contract. */
export function renderDecision(result: DecideResult): string {
  const lines: string[] = [];
  const title = [result.window.app, result.window.title].filter(Boolean).join(" — ");
  lines.push(title || "(unknown window)");
  lines.push(`goal: ${result.goal}`);
  lines.push(
    `model ${result.model} · ${result.latencyMs} ms · state about ${result.estimatedStateTokens} tokens (chars/4 estimate)`
  );
  lines.push(`usage: ${result.usage.input_tokens} in, ${result.usage.output_tokens} out`);
  lines.push("");

  const decision = result.decision;
  const failure = result.gateFailure;
  if (decision) {
    lines.push(`status: ${decision.status}`);
    const acted = decision.target
      ? `${decision.target.optionId} ${decision.target.role} "${decision.target.label}" — ${decision.target.place}`
      : decision.region
        ? `${decision.region.regionId} ${decision.region.label} — ${decision.region.place} — ${decision.direction ?? "?"}`
        : decision.key ?? decision.chord ?? "(nothing to act on)";
    lines.push(`operation: ${decision.operation} -> ${acted}`);
    lines.push(
      `gate: passed where it applies (confidence floor ${decision.gate.minConfidence}, ` +
      `margin floor ${decision.gate.minMargin})`
    );
    lines.push("");
    lines.push("readings:");
    for (const reading of decision.gate.readings) {
      // Only what the run stops or acts on is checked, so say which is which
      // rather than let a number imply a check that never happened.
      lines.push(
        `  ${reading.question}: ${reading.choice} — confidence ${percent(reading.confidence)}, ` +
        `margin ${percent(reading.margin)} — ${reading.gated ? "gated" : "not gated"}`
      );
    }
  } else if (failure) {
    lines.push(`status: no decision — ${failure.reason}`);
    lines.push(`gate: failed on ${failure.question} — ${failure.message}`);
  }

  lines.push("");
  lines.push("probabilities:");
  const answers = decision?.answers ?? failure?.answers ?? {};
  for (const key of result.questionKeys) {
    const answer = answers[key] as { probabilities?: unknown } | undefined;
    lines.push(`  ${key}: ${vector(answer?.probabilities)}`);
  }
  lines.push("");
  lines.push("This stage decided only. No click, keystroke or scroll was sent to the desktop.");
  return lines.join("\n");
}

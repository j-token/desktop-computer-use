import { DcuError } from "../errors.js";

/**
 * The hosted Jev protocol. Only the two question shapes the control path is
 * allowed to send are modelled here: `noul` is deliberately absent, because it
 * returns no confidence and so can never gate an action.
 */
export interface JevChoiceQuestion {
  type: "choice";
  instructions: string;
  criteria: Record<string, string | null>;
}

export interface JevScoreQuestion {
  type: "score";
  instructions: string;
  criteria: string[];
}

export type JevQuestion = JevChoiceQuestion | JevScoreQuestion;

export interface JevRequest {
  state: string[];
  model: string;
  questions: Record<string, JevQuestion>;
}

export interface JevChoiceAnswer {
  type?: string;
  choice: string;
  probabilities: Record<string, number>;
  confidence: number;
}

export interface JevScoreAnswer {
  score: number;
  probabilities: Record<string, number>;
  confidence: number;
  legend?: Record<string, string>;
}

export type JevAnswer = JevChoiceAnswer | JevScoreAnswer | Record<string, unknown>;
export type JevAnswers = Record<string, JevAnswer>;

export interface JevUsage {
  input_tokens: number;
  output_tokens: number;
}

export interface JevResponse {
  model: string;
  answers: JevAnswers;
  usage: JevUsage;
}

export interface JevTransport {
  ask(request: JevRequest, signal: AbortSignal): Promise<JevResponse>;
}

export interface JevConfig {
  /** Present or absent; the value itself never reaches a log, an error or an output. */
  apiKey?: string;
  baseUrl: string;
  model: string;
  timeoutMs: number;
}

export const DEFAULT_BASE_URL = "https://api.typesafe.ai/v1";
export const DEFAULT_MODEL = "jev-latest";
export const DEFAULT_TIMEOUT_MS = 8000;

/** 429 and 529 are worth a second look; three attempts total, never more. */
const MAX_RATE_LIMIT_ATTEMPTS = 3;
const BASE_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 8000;

type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

export interface FetchJevTransportOptions {
  config?: JevConfig;
  /** Injected so the retry, timeout and status paths can be tested without a network or a key. */
  fetchImpl?: FetchLike;
  log?: (line: string) => void;
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
}

function positiveNumber(raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

/** A mode-specific name wins even when explicitly empty, so a legacy value cannot leak through it. */
function preferred(env: NodeJS.ProcessEnv, name: string, legacyName: string): string | undefined {
  return env[name] !== undefined ? env[name] : env[legacyName];
}

/**
 * Configuration comes from the environment and from nowhere else. Reading a key
 * out of another program's config file is exactly the habit a tool that drives
 * someone's desktop must not normalise.
 */
export function readJevConfig(env: NodeJS.ProcessEnv = process.env): JevConfig {
  const apiKey = preferred(env, "DCU_JEV_API_KEY", "TYPESAFE_API_KEY")?.trim();
  const config: JevConfig = {
    baseUrl: (
      preferred(env, "DCU_JEV_BASE_URL", "DCU_AGENT_BASE_URL")?.trim() || DEFAULT_BASE_URL
    ).replace(/\/+$/u, ""),
    model: preferred(env, "DCU_JEV_MODEL", "DCU_AGENT_MODEL")?.trim() || DEFAULT_MODEL,
    timeoutMs: positiveNumber(
      preferred(env, "DCU_JEV_TIMEOUT_MS", "DCU_AGENT_TIMEOUT_MS"),
      DEFAULT_TIMEOUT_MS
    )
  };
  if (apiKey) config.apiKey = apiKey;
  return config;
}

export function jevHost(config: JevConfig): string {
  try {
    return new URL(config.baseUrl).host;
  } catch {
    // A malformed URL can still contain userinfo or a query token. Reports need
    // to say it is invalid without reflecting any part of it back to the caller.
    return "(invalid URL)";
  }
}

export function missingKeyError(): DcuError {
  return new DcuError(
    "agent_config",
    "DCU_JEV_API_KEY or TYPESAFE_API_KEY is not set; export one to let the agent ask the Jev model"
  );
}

function rejectedKeyError(): DcuError {
  return new DcuError(
    "agent_config",
    "The DCU_JEV_API_KEY or TYPESAFE_API_KEY value was rejected by the Jev endpoint (401)"
  );
}

/** Seconds, or an HTTP date. Anything else is treated as absent. */
export function retryAfterMs(header: string | null): number | undefined {
  if (!header) return undefined;
  const seconds = Number(header.trim());
  if (Number.isFinite(seconds) && seconds >= 0) return Math.min(seconds * 1000, MAX_BACKOFF_MS);
  const date = Date.parse(header);
  if (Number.isNaN(date)) return undefined;
  return Math.min(Math.max(0, date - Date.now()), MAX_BACKOFF_MS);
}

function backoffMs(attempt: number): number {
  const ceiling = Math.min(BASE_BACKOFF_MS * 2 ** (attempt - 1), MAX_BACKOFF_MS);
  return Math.round(ceiling / 2 + Math.random() * (ceiling / 2));
}

function defaultSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = (): void => {
      clearTimeout(timer);
      reject(new DcuError("agent_cancelled", "The Jev request was cancelled while backing off"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/** Question keys and how many options each carries. Never the state, never the key. */
export function describeShape(request: JevRequest): string {
  return Object.entries(request.questions)
    .map(([key, question]) => {
      const count = question.type === "choice"
        ? Object.keys(question.criteria).length
        : question.criteria.length;
      return `${key}(${count})`;
    })
    .join(" ");
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && (error.name === "AbortError" || error.name === "TimeoutError");
}

function asResponse(value: unknown): JevResponse {
  const body = value && typeof value === "object" ? value as Record<string, unknown> : undefined;
  const answers = body?.answers;
  if (!answers || typeof answers !== "object" || Array.isArray(answers)) {
    throw new DcuError("agent_response", "The Jev reply carried no answers object");
  }
  const usage = body?.usage && typeof body.usage === "object"
    ? body.usage as Record<string, unknown>
    : {};
  return {
    model: typeof body?.model === "string" ? body.model : "",
    answers: answers as JevAnswers,
    usage: {
      input_tokens: typeof usage.input_tokens === "number" ? usage.input_tokens : 0,
      output_tokens: typeof usage.output_tokens === "number" ? usage.output_tokens : 0
    }
  };
}

export class FetchJevTransport implements JevTransport {
  readonly config: JevConfig;
  private readonly fetchImpl: FetchLike;
  private readonly log: (line: string) => void;
  private readonly sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  /** How many HTTP attempts the last `ask` spent; read by tests and by `--pretty`. */
  attempts = 0;

  constructor(options: FetchJevTransportOptions = {}) {
    this.config = options.config ?? readJevConfig();
    this.fetchImpl = options.fetchImpl ?? ((input, init) => fetch(input, init));
    this.log = options.log ?? ((line: string) => process.stderr.write(`${line}\n`));
    this.sleep = options.sleep ?? defaultSleep;
  }

  async ask(request: JevRequest, signal: AbortSignal): Promise<JevResponse> {
    const apiKey = this.config.apiKey;
    if (!apiKey) throw missingKeyError();
    const url = `${this.config.baseUrl}/systemone`;
    const body = JSON.stringify(request);
    this.attempts = 0;
    let rateLimitAttempts = 0;
    let timeoutRetried = false;

    for (;;) {
      this.attempts += 1;
      const attemptSignal = AbortSignal.any([signal, AbortSignal.timeout(this.config.timeoutMs)]);
      let response: Response;
      try {
        response = await this.fetchImpl(url, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            authorization: `Bearer ${apiKey}`
          },
          body,
          signal: attemptSignal
        });
      } catch (error) {
        if (signal.aborted) {
          throw new DcuError("agent_cancelled", "The Jev request was cancelled");
        }
        if (isAbort(error)) {
          // One retry: a single slow request is common, two in a row is the service.
          if (timeoutRetried) {
            throw new DcuError(
              "agent_timeout",
              `The Jev request did not answer within ${this.config.timeoutMs} ms on two attempts`
            );
          }
          timeoutRetried = true;
          continue;
        }
        throw new DcuError("agent_transport", `Could not reach the Jev endpoint: ${(error as Error).message}`);
      }

      if (response.status === 401) throw rejectedKeyError();
      if (response.status === 422) {
        // Our request is the malformed one. The state carries the visible text of
        // the user's window, so only the shape of the questions may be logged.
        this.log(`jev 422 rejected the request; questions: ${describeShape(request)}`);
        throw new DcuError("agent_request", "The Jev endpoint rejected the request as malformed (422)");
      }
      if (response.status === 429 || response.status === 529) {
        rateLimitAttempts += 1;
        if (rateLimitAttempts >= MAX_RATE_LIMIT_ATTEMPTS) {
          throw new DcuError(
            "agent_rate_limited",
            `The Jev endpoint answered ${response.status} on ${rateLimitAttempts} attempts`
          );
        }
        const advised = retryAfterMs(response.headers.get("retry-after"));
        await this.sleep(advised ?? backoffMs(rateLimitAttempts), signal);
        continue;
      }
      if (!response.ok) {
        throw new DcuError("agent_http", `The Jev endpoint answered ${response.status}`);
      }
      return asResponse(await response.json());
    }
  }
}

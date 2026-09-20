import { randomUUID } from "node:crypto";
import { DcuClient, isTransportFailure } from "../client.js";
import { DcuError, asError } from "../errors.js";
import { MAX_STATE_TOKENS } from "./decide.js";
import { goalTokens, readObservation, serializeCandidate } from "./explain.js";
import type { ExplainCandidate } from "./explain.js";
import { buildCandidates, normalizeLabel } from "./filter.js";
import {
  FetchJevTransport,
  missingKeyError,
  readJevConfig,
  type JevConfig,
  type JevTransport,
  type JevUsage
} from "./jev.js";
import { buildQuestions, parseAnswers } from "./questions.js";
import type { Decision, GateFailure, GateFailureReason, GateSettings } from "./questions.js";
import { estimateTokens } from "./state.js";
import type { DecisionOperation, HistoryStep, StateCandidate, StateWindow } from "./state.js";
import type { Observation, RegionSummary } from "./types.js";
import {
  NoProgressDetector,
  actionKey,
  candidatesHash,
  describeAction,
  exhaustedBudget,
  findDestructive,
  redactCandidates,
  resolveBudgets,
  resolveDestructiveMode,
  resolveGate,
  type Budgets,
  type DestructiveFinding,
  type DestructiveGate,
  type DestructiveMode
} from "./guards.js";
import {
  PATTERN_UNAVAILABLE,
  TextResolver,
  WAIT_MS,
  observeCall,
  planAction,
  runPlan,
  traceableParams,
  type ActionContext,
  type CallOutcome,
  type NativeCall,
  type PlanRun,
  type ResolvedText
} from "./executor.js";
import { nullTrace, type Trace } from "./trace.js";
import type { CallOptions, JsonObject } from "../types.js";

/**
 * The loop. Every step is observe, decide, guard, act, and compare the two
 * observations. The daemon never reports that an action worked — it reports that
 * the input was delivered — so the only verification that exists is the
 * difference between the observation before and the observation after, and that
 * difference is computed here in code.
 */

export type AgentStatus = "succeeded" | "blocked" | "aborted" | "budget_exhausted" | "failed";

/** An element index belongs to one observation; these codes mean it no longer does. */
const INDEX_CODES: ReadonlySet<string> = new Set([
  "element_not_found",
  "element_required",
  "element_not_actionable"
]);

/** Twice in a row is a pattern; once is a window that moved while it was being read. */
const REPEAT_LIMIT = 2;
const BUSY_BACKOFF_MS: readonly number[] = [250, 500, 1000];
const FOCUS_RETRY_MS = 500;
const MAX_CHANGE_LABELS = 4;
/** Below this, an empty candidate list means no accessible UI rather than a quiet screen. */
const BARE_TREE_ELEMENTS = 5;

/**
 * An uncertain answer usually means the screen was still rendering when it was
 * read, so those two get one more look. The other three say the reply did not
 * line up with the questions we asked, and asking the same thing again cannot
 * mend our own bookkeeping.
 */
const RETRYABLE_GATE_REASONS: ReadonlySet<GateFailureReason> = new Set(["low_confidence", "low_margin"]);
/** One second look per step, and no more: twice uncertain is the screen, not the timing. */
const GATE_RETRY_LIMIT = 1;

export interface DecideStepInput {
  goal: string;
  window: StateWindow;
  candidates: readonly StateCandidate[];
  disabledShortlist: readonly StateCandidate[];
  regions: readonly RegionSummary[];
  history: readonly HistoryStep[];
  notes: readonly string[];
  gate: GateSettings;
}

export interface DecideStepResult {
  model: string;
  latencyMs: number;
  usage: JevUsage;
  estimatedStateTokens: number;
  questionKeys: string[];
  decision?: Decision;
  gateFailure?: GateFailure;
}

export type DecideStep = (input: DecideStepInput) => Promise<DecideStepResult>;

/**
 * The half of `decide()` that runs once an observation already exists. The loop
 * must never let the decision layer observe for itself: the index it decides on
 * has to be the index the executor acts against, and a second observation would
 * mint a different one.
 */
export function createJevDecideStep(
  options: { transport?: JevTransport; config?: JevConfig } = {}
): DecideStep {
  const config = options.config ?? readJevConfig();
  if (!options.transport && !config.apiKey) throw missingKeyError();
  const transport = options.transport ?? new FetchJevTransport({ config });
  return async (input: DecideStepInput): Promise<DecideStepResult> => {
    const plan = buildQuestions({
      goal: input.goal,
      window: input.window,
      candidates: input.candidates,
      disabledShortlist: input.disabledShortlist,
      regions: input.regions,
      history: input.history,
      notes: input.notes,
      gate: input.gate
    });
    const estimatedStateTokens = estimateTokens(plan.state);
    if (estimatedStateTokens > MAX_STATE_TOKENS) {
      throw new DcuError(
        "agent_state_too_large",
        `The state is an estimated ${estimatedStateTokens} tokens, over the ${MAX_STATE_TOKENS} ceiling; filter harder`
      );
    }
    const startedAt = performance.now();
    const response = await transport.ask(
      { state: plan.state, model: config.model, questions: plan.questions },
      new AbortController().signal
    );
    const latencyMs = Math.round(performance.now() - startedAt);
    const parsed = parseAnswers(plan, response.answers);
    const result: DecideStepResult = {
      model: response.model || config.model,
      latencyMs,
      usage: response.usage,
      estimatedStateTokens,
      questionKeys: Object.keys(plan.questions)
    };
    if (parsed.ok) result.decision = parsed;
    else result.gateFailure = parsed;
    return result;
  };
}

export interface AgentDeps {
  client: DcuClient;
  decide?: DecideStep;
  trace: Trace;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

export interface AgentConfig {
  app?: string;
  windowId?: string;
  runId?: string;
  sessionId?: string;
  maxSteps?: number;
  maxDurationMs?: number;
  maxInputTokens?: number;
  minConfidence?: number;
  minMargin?: number;
  maxCandidates?: number;
  timeoutMs?: number;
  onDestructive?: DestructiveMode;
  isTty?: boolean;
  texts?: Record<string, string>;
  noValues?: boolean;
  startSession?: boolean;
  resumeSession?: boolean;
  expectElementName?: string;
  stream?: (event: AgentEvent) => void;
}

export interface AgentStepEvent {
  type: "step";
  runId: string;
  step: number;
  decision: number;
  operation: DecisionOperation;
  action: string;
  confidence: number;
  margin: number;
  outcome: string;
  observationId?: string;
  usage: JevUsage;
}

export type AgentEvent = AgentStepEvent;

export interface AgentResult {
  runId: string;
  status: AgentStatus;
  /** Machine readable; the exit code and the summary line both come from it. */
  reason: string;
  steps: number;
  decisions: number;
  usage: JevUsage;
  durationMs: number;
  window: { id?: string; app?: string; title?: string };
  lastObservationId?: string;
  tracePath: string;
  history: HistoryStep[];
  expectation?: "met" | "unmet";
  sessionId?: string;
  budgets: Budgets;
  detail?: Record<string, unknown>;
}

interface Snapshot {
  observationId: string;
  observation: Observation;
  elementCount: number;
  title?: string;
}

interface Stop {
  status: AgentStatus;
  reason: string;
  detail?: Record<string, unknown>;
}

interface WindowRef {
  id?: string;
  app?: string;
  title?: string;
}

interface Prepared {
  sessionId: string;
  windowId: string;
  window: WindowRef;
  snapshot: Snapshot;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function usageZero(): JevUsage {
  return { input_tokens: 0, output_tokens: 0 };
}

function errorOutcome(code: string, message: string, elapsedMs = 0): CallOutcome {
  return { kind: "error", code, message, elapsedMs };
}

function outcomeDetail(outcome: CallOutcome): Record<string, unknown> {
  if (outcome.kind !== "error") return {};
  return {
    code: outcome.code,
    message: outcome.message,
    ...(outcome.requestSent === undefined ? {} : { requestSent: outcome.requestSent })
  };
}

function toSnapshot(raw: Record<string, unknown>): Snapshot | undefined {
  try {
    const observation = readObservation(raw);
    return {
      observationId: typeof raw.observationId === "string" ? raw.observationId : "",
      observation,
      elementCount: observation.accessibility.elementCount ?? observation.accessibility.elements.length,
      ...(observation.window.title === undefined ? {} : { title: observation.window.title })
    };
  } catch {
    return undefined;
  }
}

/**
 * Every native call in this layer goes through here, so that `busy` is retried in
 * exactly one place and every other failure keeps the shape the error matrix
 * reads.
 */
export class NativeCaller {
  constructor(
    private readonly client: DcuClient,
    private readonly now: () => number,
    private readonly sleep: (ms: number) => Promise<void>,
    private readonly options: CallOptions = {}
  ) {}

  async invoke(call: NativeCall): Promise<CallOutcome> {
    const started = this.now();
    try {
      const response = await this.client.request(call.method, call.params, this.options);
      const elapsedMs = this.now() - started;
      if (response.ok) return { kind: "ok", result: asRecord(response.result), elapsedMs };
      return {
        kind: "error",
        code: response.error?.code ?? "native_error",
        message: response.error?.message ?? "The native daemon refused the request",
        elapsedMs
      };
    } catch (error) {
      const elapsedMs = this.now() - started;
      if (isTransportFailure(error)) {
        return {
          kind: "error",
          code: error.code || "transport_error",
          message: error.message,
          elapsedMs,
          requestSent: error.requestSent
        };
      }
      if (error instanceof DcuError) {
        return { kind: "error", code: error.code, message: error.message, elapsedMs };
      }
      return { kind: "error", code: "client_error", message: asError(error).message, elapsedMs };
    }
  }

  /**
   * `busy` is thrown by `try_to_lock` before the backend runs, so nothing
   * happened and the identical call may be sent again. No other code may be.
   */
  async invokeResilient(call: NativeCall): Promise<CallOutcome> {
    let outcome = await this.invoke(call);
    for (const delay of BUSY_BACKOFF_MS) {
      if (outcome.kind !== "error" || outcome.code !== "busy") return outcome;
      await this.sleep(delay);
      outcome = await this.invoke(call);
    }
    return outcome;
  }
}

/** The foreground window first, then one that is at least not minimized. */
export function pickWindow(
  windows: readonly Record<string, unknown>[],
  windowId?: string
): Record<string, unknown> | undefined {
  const usable = windows.filter(window => typeof window.id === "string" && window.id.length > 0);
  if (windowId !== undefined) return usable.find(window => window.id === windowId);
  return (
    usable.find(window => window.isForeground === true) ??
    usable.find(window => window.isMinimized !== true) ??
    usable[0]
  );
}

/**
 * The daemon matches `app` against the executable name alone, so a packaged app
 * is "ApplicationFrameHost.exe" and a user asking for it by the name on its
 * title bar gets nothing back. This is the fallback, and only the fallback: the
 * executable name is still tried first.
 */
export function windowsByTitle(
  windows: readonly Record<string, unknown>[],
  wanted: string
): Record<string, unknown>[] {
  const needle = normalizeLabel(wanted);
  if (!needle) return [];
  const titles = windows.map(window => ({
    window,
    title: normalizeLabel(typeof window.title === "string" ? window.title : "")
  }));
  const exact = titles.filter(entry => entry.title === needle);
  const matches = exact.length > 0 ? exact : titles.filter(entry => entry.title.includes(needle));
  return matches.map(entry => entry.window);
}

function readWindows(outcome: CallOutcome): Record<string, unknown>[] {
  if (outcome.kind !== "ok") return [];
  return Array.isArray(outcome.result.windows)
    ? (outcome.result.windows as unknown[]).map(asRecord)
    : [];
}

async function observeOnce(
  caller: NativeCaller,
  sessionId: string,
  windowId: string
): Promise<{ snapshot?: Snapshot; outcome: CallOutcome }> {
  const outcome = await caller.invokeResilient(observeCall({ sessionId, windowId }));
  if (outcome.kind !== "ok") return { outcome };
  const snapshot = toSnapshot(outcome.result);
  if (!snapshot) {
    return {
      outcome: errorOutcome(
        "invalid_observation",
        "The observation carried no accessibility elements",
        outcome.elapsedMs
      )
    };
  }
  return { snapshot, outcome };
}

/**
 * Session, then window, then the first observation. The window id is frozen here
 * and never resolved again: `require_observation` compares the hwnd, and
 * resolving by app on every call can silently move to another window.
 */
export async function prepareTarget(
  caller: NativeCaller,
  config: AgentConfig
): Promise<{ ok: true; prepared: Prepared } | { ok: false; stop: Stop }> {
  const status = await caller.invokeResilient({ method: "session.status", params: {} });
  if (status.kind !== "ok") {
    return { ok: false, stop: { status: "failed", reason: status.code, detail: outcomeDetail(status) } };
  }
  let sessionId = typeof status.result.sessionId === "string" ? status.result.sessionId : "";
  if (status.result.active !== true) {
    if (!config.startSession) {
      return {
        ok: false,
        stop: {
          status: "failed",
          reason: "session_required",
          detail: { hint: "Run `dcu session start`, or pass --start-session" }
        }
      };
    }
    const started = await caller.invokeResilient({ method: "session.start", params: {} });
    if (started.kind !== "ok") {
      return { ok: false, stop: { status: "failed", reason: started.code, detail: outcomeDetail(started) } };
    }
    sessionId = typeof started.result.sessionId === "string" ? started.result.sessionId : "";
  }
  if (!sessionId) sessionId = config.sessionId ?? "";
  if (!sessionId) return { ok: false, stop: { status: "failed", reason: "session_required" } };

  const params: JsonObject = { sessionId };
  if (config.app !== undefined) params.app = config.app;
  const listed = await caller.invokeResilient({ method: "list-windows", params });
  if (listed.kind !== "ok") {
    return { ok: false, stop: { status: "failed", reason: listed.code, detail: outcomeDetail(listed) } };
  }
  let windows = readWindows(listed);
  let picked = pickWindow(windows, config.windowId);
  if (!picked && config.windowId === undefined && config.app !== undefined) {
    const all = await caller.invokeResilient({ method: "list-windows", params: { sessionId } });
    if (all.kind === "ok") {
      const everything = readWindows(all);
      const byTitle = windowsByTitle(everything, config.app);
      const chosen = pickWindow(byTitle);
      if (chosen) {
        windows = everything;
        picked = chosen;
      }
    }
  }
  if (!picked) {
    return {
      ok: false,
      stop: {
        status: "failed",
        reason: "window_not_found",
        detail: {
          ...(config.app === undefined ? {} : { app: config.app }),
          ...(config.windowId === undefined ? {} : { windowId: config.windowId }),
          offered: windows.length
        }
      }
    };
  }
  const windowId = String(picked.id);
  const window: WindowRef = {
    id: windowId,
    ...(typeof picked.app === "string" ? { app: picked.app } : {}),
    ...(typeof picked.title === "string" ? { title: picked.title } : {})
  };

  const observed = await observeOnce(caller, sessionId, windowId);
  if (!observed.snapshot) {
    return {
      ok: false,
      stop: { status: "failed", reason: "observation_failed", detail: outcomeDetail(observed.outcome) }
    };
  }
  if (observed.snapshot.title !== undefined) window.title = observed.snapshot.title;
  return { ok: true, prepared: { sessionId, windowId, window, snapshot: observed.snapshot } };
}

function quoteLabels(labels: readonly string[]): string {
  const shown = labels.slice(0, MAX_CHANGE_LABELS).map(label => `"${label.replace(/"/gu, "'")}"`);
  const rest = labels.length - shown.length;
  const listed = shown.join(", ");
  return rest > 0 ? `${listed} and ${rest} more` : listed;
}

function joinClauses(parts: readonly string[]): string {
  if (parts.length <= 1) return parts.join("");
  return `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}

function focusLabel(snapshot: Snapshot, candidates: readonly ExplainCandidate[]): string | undefined {
  const focused = snapshot.observation.accessibility.elements.find(element => element.focused);
  if (!focused) return undefined;
  const candidate = candidates.find(item => item.elementIndex === focused.index);
  if (candidate) return candidate.label;
  const name = focused.name.trim();
  return name.length > 0 ? name : undefined;
}

/**
 * What changed, in words, from the two candidate lists. The model is never asked
 * this: it chose the action, so it is the last thing that should be asked whether
 * the action worked.
 */
export function describeChange(
  before: readonly ExplainCandidate[],
  after: readonly ExplainCandidate[],
  beforeTitle: string | undefined,
  afterTitle: string | undefined,
  beforeFocus: string | undefined,
  afterFocus: string | undefined
): string {
  const beforeLabels = new Set(before.map(candidate => candidate.label));
  const afterLabels = new Set(after.map(candidate => candidate.label));
  const added = [...afterLabels].filter(label => !beforeLabels.has(label));
  const removed = [...beforeLabels].filter(label => !afterLabels.has(label));
  const parts: string[] = [];
  if (added.length > 0) parts.push(`${quoteLabels(added)} appeared`);
  if (removed.length > 0) parts.push(`${quoteLabels(removed)} went away`);
  if (afterTitle && afterTitle !== beforeTitle) parts.push(`the window title became "${afterTitle}"`);
  if (afterFocus && afterFocus !== beforeFocus) parts.push(`the focus moved to "${afterFocus}"`);
  if (parts.length === 0) return "Nothing on screen changed afterwards.";
  return `Afterwards ${joinClauses(parts)}.`;
}

export class Agent {
  private readonly caller: NativeCaller;
  private readonly decideStep: DecideStep;
  private readonly trace: Trace;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly config: AgentConfig;
  private readonly budgets: Budgets;
  private readonly gate: GateSettings;
  private readonly destructive: DestructiveGate;
  private readonly expect: RegExp | undefined;
  private readonly runId: string;

  private sessionId = "";
  private windowId = "";
  private window: WindowRef = {};
  private tokens: readonly string[] = [];
  private steps = 0;
  private decisions = 0;
  private startedAt = 0;
  private readonly usage: JevUsage = usageZero();
  private readonly history: HistoryStep[] = [];
  private readonly progress = new NoProgressDetector();
  private snapshot: Snapshot | undefined;
  private lastObservationId: string | undefined;
  private staleStreak = 0;
  private desyncStreak = 0;
  private uncertainStreak = 0;
  private gateRetries = 0;
  private sessionRestarted = false;
  private pendingRestart = false;
  private expectation: "met" | "unmet" | undefined;

  constructor(deps: AgentDeps, config: AgentConfig) {
    this.trace = deps.trace ?? nullTrace();
    this.now = deps.now ?? (() => Date.now());
    this.sleep = deps.sleep ?? ((ms: number) => new Promise(resolve => setTimeout(resolve, ms)));
    this.config = config;
    this.runId = config.runId ?? randomUUID();
    this.budgets = resolveBudgets(config);
    this.gate = resolveGate(config);
    this.destructive = resolveDestructiveMode(config.onDestructive ?? "stop", config.isTty ?? false);
    this.expect = config.expectElementName === undefined
      ? undefined
      : compileExpectation(config.expectElementName);
    this.caller = new NativeCaller(
      deps.client,
      this.now,
      this.sleep,
      config.timeoutMs === undefined ? {} : { timeoutMs: config.timeoutMs }
    );
    // Built last: with no key this throws, and that is worth saying before a
    // session has been started rather than once the desktop has been taken over.
    this.decideStep = deps.decide ?? createJevDecideStep();
    if (config.sessionId) this.sessionId = config.sessionId;
  }

  /**
   * Nothing escapes: a throw from inside the loop would leave the trace without
   * its summary, and summing the trace is how the run's cost is read back.
   */
  async run(goal: string): Promise<AgentResult> {
    this.startedAt = this.now();
    try {
      return await this.loop(goal);
    } catch (error) {
      const code = error instanceof DcuError ? error.code : "agent_error";
      return this.finish({ status: "failed", reason: code, detail: { message: asError(error).message } });
    }
  }

  private async loop(goal: string): Promise<AgentResult> {
    const trimmed = goal?.trim();
    if (!trimmed) return this.finish({ status: "failed", reason: "invalid_argument" });
    this.tokens = goalTokens(trimmed);
    const texts = new TextResolver(trimmed, this.config.texts ?? {});

    const prepared = await prepareTarget(this.caller, this.config);
    if (!prepared.ok) return this.finish(prepared.stop);
    this.sessionId = prepared.prepared.sessionId;
    this.windowId = prepared.prepared.windowId;
    this.window = prepared.prepared.window;
    this.adopt(prepared.prepared.snapshot);

    await this.trace.write({
      type: "run",
      at: new Date().toISOString(),
      runId: this.runId,
      goal: trimmed,
      window: { ...this.window },
      budgets: { ...this.budgets },
      gate: { ...this.gate },
      destructive: {
        requested: this.destructive.requested,
        effective: this.destructive.effective,
        ...(this.destructive.degradedReason === undefined
          ? {}
          : { degradedReason: this.destructive.degradedReason })
      }
    });

    for (;;) {
      const exhausted = exhaustedBudget(this.budgets, {
        steps: this.steps,
        decisions: this.decisions,
        elapsedMs: this.now() - this.startedAt,
        inputTokens: this.usage.input_tokens
      });
      if (exhausted) return this.finish({ status: "budget_exhausted", reason: exhausted });

      if (!this.snapshot) {
        const observed = await this.observeFresh();
        if (!observed.snapshot) {
          const stop = this.classifyObserveFailure(observed.outcome);
          if (stop) return this.finish(stop);
          continue;
        }
        this.adopt(observed.snapshot);
      }
      const snapshot = this.snapshot as Snapshot;

      const filtered = buildCandidates(snapshot.observation, this.filterOptions());
      const candidates = this.redact(filtered.candidates.map(serializeCandidate));
      const disabled = this.redact(filtered.disabledShortlist.map(serializeCandidate));

      // Jev takes text and nothing else, so a window that exposes no accessible
      // controls has nothing to fall back to.
      if (candidates.length === 0 && snapshot.elementCount < BARE_TREE_ELEMENTS) {
        return this.finish({
          status: "blocked",
          reason: "no_accessible_ui",
          detail: { elementCount: snapshot.elementCount }
        });
      }

      this.decisions += 1;
      let step: DecideStepResult;
      try {
        step = await this.decideStep({
          goal: trimmed,
          window: {
            ...(snapshot.observation.window.app === undefined ? {} : { app: snapshot.observation.window.app }),
            ...(snapshot.observation.window.title === undefined
              ? {}
              : { title: snapshot.observation.window.title })
          },
          candidates,
          disabledShortlist: disabled,
          regions: filtered.report.regions,
          history: this.history,
          notes: [],
          gate: this.gate
        });
      } catch (error) {
        const code = error instanceof DcuError ? error.code : "agent_error";
        return this.finish({ status: "failed", reason: code, detail: { message: asError(error).message } });
      }
      this.usage.input_tokens += step.usage.input_tokens;
      this.usage.output_tokens += step.usage.output_tokens;
      await this.traceDecide(step, filtered.report.rawCount, candidates.length);

      if (!step.decision) {
        const failure = step.gateFailure as GateFailure;
        // The commonest cause of an uncertain answer is a screen that was still
        // rendering when it was read, one round trip after the work was already
        // done. Let it settle, read it again, and ask once more before giving up.
        if (RETRYABLE_GATE_REASONS.has(failure.reason) && this.gateRetries < GATE_RETRY_LIMIT) {
          this.gateRetries += 1;
          await this.note(this.steps + 1, "gate_retry", {
            reason: failure.reason,
            question: failure.question,
            ...(failure.confidence === undefined ? {} : { confidence: failure.confidence }),
            ...(failure.margin === undefined ? {} : { margin: failure.margin }),
            settleMs: WAIT_MS
          });
          await this.sleep(WAIT_MS);
          // The second decision must be made against a second observation: an
          // element index belongs to the observation it was minted from.
          this.snapshot = undefined;
          continue;
        }
        return this.finish({
          status: "blocked",
          reason: failure.reason,
          detail: {
            question: failure.question,
            message: failure.message,
            // "Uncertain twice, half a second apart" is a different report from
            // "uncertain once", so the reader is told which one this was.
            retried: this.gateRetries > 0,
            ...(failure.confidence === undefined ? {} : { confidence: failure.confidence }),
            ...(failure.margin === undefined ? {} : { margin: failure.margin })
          }
        });
      }
      const decision = step.decision;

      // A decision that stops carries an operation the gate never checked, because
      // nothing is about to be performed. The `done` branch therefore ignores it,
      // and the `blocked` branch reports it as what the model leaned towards
      // rather than as an action that cleared the floors.
      if (decision.status === "done") return this.finish({ status: "succeeded", reason: "goal_reached" });
      if (decision.status === "blocked") {
        return this.finish({
          status: "blocked",
          reason: "model_reported_blocked",
          detail: { wouldHaveDone: describeAction(decision), operationGated: false }
        });
      }

      if (this.progress.record(candidatesHash(candidates), actionKey(decision))) {
        return this.finish({
          status: "aborted",
          reason: "no_progress",
          detail: { repeatedAction: describeAction(decision) }
        });
      }

      const finding = findDestructive(decision);
      if (finding && this.destructive.effective !== "allow") {
        return this.finish({
          status: "blocked",
          reason: "destructive_action",
          detail: {
            ...finding,
            requestedMode: this.destructive.requested,
            ...(this.destructive.degradedReason === undefined
              ? {}
              : { degradedReason: this.destructive.degradedReason }),
            rerunWith: "--on-destructive allow"
          }
        });
      }

      let text: ResolvedText | undefined;
      if (decision.operation === "enter_text") {
        const label = decision.target?.label ?? "";
        text = texts.resolve(label);
        if (!text) {
          return this.finish({
            status: "blocked",
            reason: "text_unavailable",
            detail: { field: label, action: describeAction(decision) }
          });
        }
      }

      const chosen = decision.target
        ? filtered.candidates.find(candidate => candidate.optionId === decision.target?.optionId)
        : undefined;
      const context: ActionContext = {
        sessionId: this.sessionId,
        windowId: this.windowId,
        observationId: snapshot.observationId,
        observation: snapshot.observation,
        ...(chosen === undefined ? {} : { targetIndexes: [chosen.elementIndex, ...chosen.mergedIndexes] }),
        ...(text === undefined ? {} : { text })
      };

      const stop = await this.act(decision, context, candidates, snapshot, step);
      if (stop) return this.finish(stop);
    }
  }

  private filterOptions(): { goalTokens: readonly string[]; maxCandidates?: number } {
    return {
      goalTokens: this.tokens,
      ...(this.config.maxCandidates === undefined ? {} : { maxCandidates: this.config.maxCandidates })
    };
  }

  private redact(candidates: readonly ExplainCandidate[]): ExplainCandidate[] {
    return redactCandidates(candidates, { noValues: this.config.noValues === true }).candidates;
  }

  /** One action, the observation it carries back, and the history entry comparing the two. */
  private async act(
    decision: Decision,
    context: ActionContext,
    before: readonly ExplainCandidate[],
    beforeSnapshot: Snapshot,
    step: DecideStepResult
  ): Promise<Stop | undefined> {
    const plan = planAction(decision, context);
    const stepNumber = this.steps + 1;
    const invoke = (call: NativeCall): Promise<CallOutcome> => this.caller.invokeResilient(call);
    let run = await runPlan(plan, invoke, this.sleep);
    await this.traceRun(run, stepNumber, decision, plan.text);

    if (run.last.kind === "error" && run.last.code === "focus_denied" && run.last.requestSent !== true) {
      // `activate` refuses before any input is synthesized, so nothing was delivered.
      await this.sleep(FOCUS_RETRY_MS);
      run = await runPlan(plan, invoke, this.sleep);
      await this.traceRun(run, stepNumber, decision, plan.text);
    }

    const outcome = run.last;
    if (outcome.kind === "error") return this.classifyActionFailure(outcome, decision, stepNumber, step);

    // The input reached the window, so the step is spent whatever the observation does.
    this.steps = stepNumber;
    this.staleStreak = 0;
    this.desyncStreak = 0;
    this.uncertainStreak = 0;
    this.gateRetries = 0;

    const attached = plan.observeVia === "result" ? outcome.result : asRecord(outcome.result.observation);
    let next = toSnapshot(attached);
    if (!next) {
      // Delivered input must never become a retryable failure: read the window again.
      await this.note(stepNumber, "observation_retaken", {
        reported: asRecord(outcome.result.observationError).message ?? null
      });
      const fresh = await this.observeFresh();
      if (!fresh.snapshot) {
        return { status: "failed", reason: "observation_failed", detail: outcomeDetail(fresh.outcome) };
      }
      next = fresh.snapshot;
    }

    const beforeFocus = focusLabel(beforeSnapshot, before);
    this.adopt(next);
    const after = this.redact(
      buildCandidates(next.observation, this.filterOptions()).candidates.map(serializeCandidate)
    );
    const change = describeChange(
      before,
      after,
      beforeSnapshot.title,
      next.title,
      beforeFocus,
      focusLabel(next, after)
    );
    this.record(decision, change);
    this.emit(decision, stepNumber, step.usage, change, next.observationId);
    return undefined;
  }

  private classifyActionFailure(
    outcome: CallOutcome,
    decision: Decision,
    stepNumber: number,
    step: DecideStepResult
  ): Stop | undefined {
    if (outcome.kind !== "error") return undefined;
    // `protocol.md`: once the request bytes are written, the mutation is terminal.
    // It may well have happened, so the only honest move is to look again and say so.
    if (outcome.requestSent === true) {
      this.uncertainStreak += 1;
      if (this.uncertainStreak >= REPEAT_LIMIT) {
        return { status: "aborted", reason: "transport_unstable", detail: outcomeDetail(outcome) };
      }
      this.steps = stepNumber;
      this.snapshot = undefined;
      this.progress.reset();
      const note = "It is not known whether that reached the window, so the screen was read again.";
      this.record(decision, note);
      this.emit(decision, stepNumber, step.usage, note);
      return undefined;
    }
    if (outcome.code === "busy") {
      // Every backoff was spent and the daemon is still busy: look again and re-decide.
      this.snapshot = undefined;
      return undefined;
    }
    if (outcome.code === PATTERN_UNAVAILABLE) {
      // The field cannot have its contents replaced, and typing into it instead
      // would insert at the caret and leave a half-merged string behind. Stopping
      // is the only honest move: nothing was written.
      return {
        status: "blocked",
        reason: "field_not_replaceable",
        detail: {
          field: decision.target?.label ?? "",
          action: describeAction(decision),
          ...outcomeDetail(outcome)
        }
      };
    }
    if (outcome.code === "stale_observation") {
      this.staleStreak += 1;
      if (this.staleStreak >= REPEAT_LIMIT) {
        return { status: "aborted", reason: "unstable_window", detail: outcomeDetail(outcome) };
      }
      // Never replay the index against a fresh observation: there it names another control.
      this.snapshot = undefined;
      return undefined;
    }
    if (INDEX_CODES.has(outcome.code)) {
      this.desyncStreak += 1;
      if (this.desyncStreak >= REPEAT_LIMIT) {
        return { status: "aborted", reason: "index_desync", detail: outcomeDetail(outcome) };
      }
      this.snapshot = undefined;
      return undefined;
    }
    if (outcome.code === "focus_denied") {
      return { status: "failed", reason: "focus_denied", detail: outcomeDetail(outcome) };
    }
    if (outcome.code === "session_required") {
      const resumed = this.arrangeSessionRestart();
      return resumed ? undefined : { status: "aborted", reason: "session_lost", detail: outcomeDetail(outcome) };
    }
    return { status: "failed", reason: outcome.code, detail: outcomeDetail(outcome) };
  }

  /**
   * The idle lease is 120 seconds and this loop never idles that long, so the
   * realistic cause is the user pressing Esc. Restarting by default would defeat
   * the one emergency stop they have.
   */
  private arrangeSessionRestart(): boolean {
    if (!this.config.resumeSession || this.sessionRestarted) return false;
    this.sessionRestarted = true;
    this.pendingRestart = true;
    this.snapshot = undefined;
    return true;
  }

  private async observeFresh(): Promise<{ snapshot?: Snapshot; outcome: CallOutcome }> {
    if (this.pendingRestart) {
      this.pendingRestart = false;
      const started = await this.caller.invokeResilient({ method: "session.start", params: {} });
      if (started.kind !== "ok") return { outcome: started };
      const id = started.result.sessionId;
      if (typeof id !== "string" || !id) {
        return { outcome: errorOutcome("session_start_failed", "session.start returned no sessionId") };
      }
      this.sessionId = id;
    }
    return observeOnce(this.caller, this.sessionId, this.windowId);
  }

  /** Undefined means a session restart was arranged and the loop should try again. */
  private classifyObserveFailure(outcome: CallOutcome): Stop | undefined {
    if (outcome.kind !== "error") return { status: "failed", reason: "observation_failed" };
    if (outcome.code === "session_required") {
      if (this.arrangeSessionRestart()) return undefined;
      return { status: "aborted", reason: "session_lost", detail: outcomeDetail(outcome) };
    }
    return { status: "failed", reason: "observation_failed", detail: outcomeDetail(outcome) };
  }

  private adopt(snapshot: Snapshot): void {
    this.snapshot = snapshot;
    this.lastObservationId = snapshot.observationId || this.lastObservationId;
    if (snapshot.title !== undefined) this.window.title = snapshot.title;
  }

  private record(decision: Decision, outcome: string): void {
    const step: HistoryStep = { operation: decision.operation };
    const label = decision.target?.label ?? decision.region?.label;
    if (label !== undefined) step.targetLabel = label;
    if (decision.key !== undefined) step.key = decision.key;
    if (decision.chord !== undefined) step.chord = decision.chord;
    if (decision.direction !== undefined) step.direction = decision.direction;
    step.outcome = outcome;
    this.history.push(step);
  }

  private emit(
    decision: Decision,
    step: number,
    usage: JevUsage,
    outcome: string,
    observationId?: string
  ): void {
    if (!this.config.stream) return;
    this.config.stream({
      type: "step",
      runId: this.runId,
      step,
      decision: this.decisions,
      operation: decision.operation,
      action: describeAction(decision),
      confidence: decision.confidence,
      margin: decision.margin,
      outcome,
      ...(observationId === undefined ? {} : { observationId }),
      usage
    });
  }

  private async traceDecide(
    step: DecideStepResult,
    rawElementCount: number,
    candidateCount: number
  ): Promise<void> {
    const answers = step.decision?.answers ?? step.gateFailure?.answers ?? {};
    const traced: Record<string, { choice?: string; confidence?: number; probabilities?: Record<string, number> }> = {};
    for (const [key, value] of Object.entries(answers)) {
      const record = asRecord(value);
      traced[key] = {
        ...(typeof record.choice === "string" ? { choice: record.choice } : {}),
        ...(typeof record.confidence === "number" ? { confidence: record.confidence } : {}),
        ...(record.probabilities && typeof record.probabilities === "object"
          ? { probabilities: record.probabilities as Record<string, number> }
          : {})
      };
    }
    await this.trace.write({
      type: "decide",
      at: new Date().toISOString(),
      step: this.steps + 1,
      decision: this.decisions,
      // Which look at this step this was: 1 the first, 2 the one after a gate
      // failure sent the loop back to the screen.
      attempt: this.gateRetries + 1,
      model: step.model,
      latencyMs: step.latencyMs,
      usage: step.usage,
      candidateCount,
      rawElementCount,
      estimatedStateTokens: step.estimatedStateTokens,
      answers: traced,
      gate: {
        passed: Boolean(step.decision),
        minConfidence: this.gate.minConfidence,
        minMargin: this.gate.minMargin,
        ...(step.gateFailure === undefined
          ? {}
          : { reason: step.gateFailure.reason, question: step.gateFailure.question }),
        ...(step.decision === undefined ? {} : { readings: step.decision.gate.readings })
      },
      ...(step.decision === undefined
        ? {}
        : {
          status: step.decision.status,
          operation: step.decision.operation,
          ...(step.decision.target === undefined ? {} : { targetLabel: step.decision.target.label })
        })
    });
  }

  private async traceRun(
    run: PlanRun,
    step: number,
    decision: Decision,
    text: ResolvedText | undefined
  ): Promise<void> {
    for (const { call, outcome } of run.attempted) {
      const nativeMs = outcome.kind === "ok" ? nativeTotalMs(outcome.result) : undefined;
      await this.trace.write({
        type: "act",
        at: new Date().toISOString(),
        step,
        method: call.method,
        params: traceableParams(call),
        ...(decision.target === undefined ? {} : { targetLabel: decision.target.label }),
        outcome: outcome.kind === "ok" ? "ok" : "error",
        ...(outcome.kind === "error" ? { code: outcome.code, message: outcome.message } : {}),
        elapsedMs: outcome.elapsedMs,
        ...(nativeMs === undefined ? {} : { nativeMs }),
        ...(text === undefined ? {} : { textSource: text.source, textChars: text.text.length })
      });
    }
  }

  private async note(step: number, event: string, detail: Record<string, unknown>): Promise<void> {
    await this.trace.write({ type: "note", at: new Date().toISOString(), step, event, detail });
  }

  /**
   * The objective success check. It observes again rather than trusting the last
   * observation, because whatever the loop did last may still have been settling.
   */
  private async checkExpectation(): Promise<void> {
    if (!this.expect || !this.sessionId || !this.windowId) return;
    this.pendingRestart = false;
    const observed = await this.observeFresh();
    if (!observed.snapshot) {
      this.expectation = "unmet";
      return;
    }
    this.adopt(observed.snapshot);
    const expect = this.expect;
    this.expectation = observed.snapshot.observation.accessibility.elements
      .some(element => expect.test(element.name))
      ? "met"
      : "unmet";
  }

  private async finish(stop: Stop): Promise<AgentResult> {
    await this.checkExpectation();
    let status = stop.status;
    let reason = stop.reason;
    // An unmet objective check is the whole point of supplying one: the run must
    // not exit zero because the model announced it had finished.
    if (status === "succeeded" && this.expectation === "unmet") {
      status = "failed";
      reason = "expectation_unmet";
    }
    const result: AgentResult = {
      runId: this.runId,
      status,
      reason,
      steps: this.steps,
      decisions: this.decisions,
      usage: { ...this.usage },
      durationMs: Math.max(0, Math.round(this.now() - this.startedAt)),
      window: { ...this.window },
      tracePath: this.trace.path,
      history: [...this.history],
      budgets: { ...this.budgets },
      ...(this.lastObservationId === undefined ? {} : { lastObservationId: this.lastObservationId }),
      ...(this.expectation === undefined ? {} : { expectation: this.expectation }),
      ...(this.sessionId ? { sessionId: this.sessionId } : {}),
      ...(stop.detail === undefined ? {} : { detail: stop.detail })
    };
    await this.trace.write({
      type: "summary",
      at: new Date().toISOString(),
      runId: this.runId,
      status: result.status,
      reason: result.reason,
      steps: result.steps,
      decisions: result.decisions,
      usage: result.usage,
      durationMs: result.durationMs,
      ...(result.expectation === undefined ? {} : { expectation: result.expectation }),
      ...(result.lastObservationId === undefined ? {} : { lastObservationId: result.lastObservationId })
    });
    await this.trace.close();
    return result;
  }
}

function nativeTotalMs(result: Record<string, unknown>): number | undefined {
  const fromAttached = asRecord(asRecord(result.observation).timings).totalMs;
  if (typeof fromAttached === "number") return fromAttached;
  const direct = asRecord(result.timings).totalMs;
  return typeof direct === "number" ? direct : undefined;
}

function compileExpectation(source: string): RegExp {
  try {
    return new RegExp(source);
  } catch (error) {
    throw new DcuError(
      "invalid_argument",
      `--expect-element-name is not a valid regular expression: ${asError(error).message}`
    );
  }
}

export interface DryRunResult {
  runId: string;
  goal: string;
  window: WindowRef;
  observationId?: string;
  rawElementCount: number;
  candidateCount: number;
  candidates: ExplainCandidate[];
  disabledShortlist: ExplainCandidate[];
  regions: RegionSummary[];
  redactedValues: number;
  estimatedStateTokens?: number;
  model?: string;
  latencyMs?: number;
  usage?: JevUsage;
  decision?: Decision;
  gateFailure?: GateFailure;
  destructive: DestructiveGate;
  destructiveFinding?: DestructiveFinding;
  text?: ResolvedText;
  textUnavailableField?: string;
  plannedCalls?: { method: string; params: Record<string, unknown> }[];
  /** Always false. A dry run observes once, decides once, and acts zero times. */
  performedInput: false;
}

/**
 * Deliberately not the loop with a no-op executor: with nothing on screen ever
 * changing, the no-progress detector would fire on the third step and the output
 * would describe a run that could not happen.
 */
export async function dryRun(deps: AgentDeps, config: AgentConfig, goal: string): Promise<DryRunResult> {
  const trimmed = goal?.trim();
  if (!trimmed) throw new DcuError("invalid_argument", "agent run requires --goal");
  const now = deps.now ?? (() => Date.now());
  const sleep = deps.sleep ?? ((ms: number) => new Promise(resolve => setTimeout(resolve, ms)));
  const caller = new NativeCaller(
    deps.client,
    now,
    sleep,
    config.timeoutMs === undefined ? {} : { timeoutMs: config.timeoutMs }
  );
  const decideStep = deps.decide ?? createJevDecideStep();
  const prepared = await prepareTarget(caller, config);
  if (!prepared.ok) {
    throw new DcuError(
      prepared.stop.reason,
      `The dry run could not reach a window: ${prepared.stop.reason}`,
      prepared.stop.detail
    );
  }
  const { sessionId, windowId, window, snapshot } = prepared.prepared;
  const gate = resolveGate(config);
  const filtered = buildCandidates(snapshot.observation, {
    goalTokens: goalTokens(trimmed),
    ...(config.maxCandidates === undefined ? {} : { maxCandidates: config.maxCandidates })
  });
  const redaction = { noValues: config.noValues === true };
  const candidates = redactCandidates(filtered.candidates.map(serializeCandidate), redaction);
  const disabled = redactCandidates(filtered.disabledShortlist.map(serializeCandidate), redaction);

  const result: DryRunResult = {
    runId: config.runId ?? randomUUID(),
    goal: trimmed,
    window: { ...window },
    rawElementCount: filtered.report.rawCount,
    candidateCount: candidates.candidates.length,
    candidates: candidates.candidates,
    disabledShortlist: disabled.candidates,
    regions: filtered.report.regions,
    redactedValues: candidates.redacted + disabled.redacted,
    destructive: resolveDestructiveMode(config.onDestructive ?? "stop", config.isTty ?? false),
    performedInput: false,
    ...(snapshot.observationId ? { observationId: snapshot.observationId } : {})
  };

  const step = await decideStep({
    goal: trimmed,
    window: {
      ...(snapshot.observation.window.app === undefined ? {} : { app: snapshot.observation.window.app }),
      ...(snapshot.observation.window.title === undefined ? {} : { title: snapshot.observation.window.title })
    },
    candidates: candidates.candidates,
    disabledShortlist: disabled.candidates,
    regions: filtered.report.regions,
    history: [],
    notes: [],
    gate
  });
  result.model = step.model;
  result.latencyMs = step.latencyMs;
  result.usage = step.usage;
  result.estimatedStateTokens = step.estimatedStateTokens;
  if (step.gateFailure) result.gateFailure = step.gateFailure;
  if (!step.decision) return result;

  const decision = step.decision;
  result.decision = decision;
  const finding = findDestructive(decision);
  if (finding) result.destructiveFinding = finding;
  if (decision.status !== "continue") return result;

  let text: ResolvedText | undefined;
  if (decision.operation === "enter_text") {
    text = new TextResolver(trimmed, config.texts ?? {}).resolve(decision.target?.label ?? "");
    if (!text) {
      result.textUnavailableField = decision.target?.label ?? "";
      return result;
    }
    result.text = text;
  }
  const chosen = decision.target
    ? filtered.candidates.find(candidate => candidate.optionId === decision.target?.optionId)
    : undefined;
  const plan = planAction(decision, {
    sessionId,
    windowId,
    observationId: snapshot.observationId,
    observation: snapshot.observation,
    ...(chosen === undefined ? {} : { targetIndexes: [chosen.elementIndex, ...chosen.mergedIndexes] }),
    ...(text === undefined ? {} : { text })
  });
  result.plannedCalls = plan.calls.map(call => {
    const params: Record<string, unknown> = { ...call.params };
    Reflect.deleteProperty(params, "sessionId");
    return { method: call.method, params };
  });
  return result;
}

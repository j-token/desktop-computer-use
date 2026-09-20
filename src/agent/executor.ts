import { DcuError } from "../errors.js";
import { normalizeLabel } from "./filter.js";
import { center } from "./regions.js";
import type { Decision, DecisionRegion, DecisionTarget } from "./questions.js";
import type { DecisionOperation } from "./state.js";
import type { Observation } from "./types.js";
import type { JsonObject } from "../types.js";

/**
 * One decision becomes one native call, and that call carries `observe: "text"`
 * so the action and the observation that has to verify it cost a single round
 * trip. Nothing here decides anything; it only maps a decision onto the verbs the
 * daemon accepts.
 */

/** Longer than this goes through the clipboard: synthesized keystrokes drop characters. */
export const PASTE_LENGTH_THRESHOLD = 120;

/** `scroll` sends three wheel notches, the same default the CLI uses. */
export const SCROLL_AMOUNT = 3;

/** `wait` looks again after this long rather than touching anything. */
export const WAIT_MS = 600;

export interface NativeCall {
  method: string;
  params: JsonObject;
}

export type TextSourceName = "goal_quote" | "preset";

export interface ResolvedText {
  text: string;
  source: TextSourceName;
  /** The preset key that matched, when the text came from one. */
  key?: string;
}

export interface ActionContext {
  sessionId: string;
  /** Frozen for the whole run: resolving by app on every call can move windows. */
  windowId: string;
  observationId: string;
  observation: Observation;
  /** The target's element index plus every index merged into it, for the focus test. */
  targetIndexes?: readonly number[];
  text?: ResolvedText;
}

/**
 * How an `enter_text` decision is carried out. The model is never asked this:
 * both mechanisms put the goal's text into the field, so the distinction is
 * ours to make from what the field affords.
 */
export type TextMechanism = "set_value" | "type_text";

export interface ActionPlan {
  operation: DecisionOperation;
  /** Only `wait` sets this; every other operation acts at once. */
  delayMs: number;
  calls: NativeCall[];
  /** Only an `enter_text` plan carries one. */
  mechanism?: TextMechanism;
  /** `attached` reads `result.observation`; `result` means the call *is* the observation. */
  observeVia: "attached" | "result";
  text?: ResolvedText;
}

/** The daemon reports a missing ValuePattern with this code. */
export const PATTERN_UNAVAILABLE = "pattern_unavailable";

function windowParams(context: ActionContext): JsonObject {
  return { sessionId: context.sessionId, windowId: context.windowId };
}

export function observeCall(context: { sessionId: string; windowId: string }): NativeCall {
  return {
    method: "get-app-state",
    params: {
      sessionId: context.sessionId,
      windowId: context.windowId,
      includeText: true,
      includeScreenshot: false
    }
  };
}

/** Keystroke synthesis is unreliable above the threshold and for anything outside ASCII. */
export function needsPaste(text: string): boolean {
  return text.length > PASTE_LENGTH_THRESHOLD || /[^\x20-\x7e]/u.test(text);
}

function focusedIndexes(observation: Observation): Set<number> {
  const indexes = new Set<number>();
  for (const element of observation.accessibility.elements) {
    if (element.focused) indexes.add(element.index);
  }
  return indexes;
}

export function isTargetFocused(target: DecisionTarget, context: ActionContext): boolean {
  const focused = focusedIndexes(context.observation);
  if (focused.size === 0) return false;
  if (focused.has(target.elementIndex)) return true;
  for (const index of context.targetIndexes ?? []) {
    if (focused.has(index)) return true;
  }
  return false;
}

function clampCoordinate(value: number, limit: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(Math.max(0, Math.round(limit) - 1), Math.round(value)));
}

/**
 * The region's centre, computed here rather than asked of the model. A region
 * that names an element is measured from that element's rectangle; the
 * whole-window region has no element and falls back to the middle of the window.
 */
export function regionPoint(region: DecisionRegion, observation: Observation): { x: number; y: number } {
  const width = Math.max(1, observation.window.width);
  const height = Math.max(1, observation.window.height);
  if (region.elementIndex !== undefined) {
    const element = observation.accessibility.elements.find(item => item.index === region.elementIndex);
    if (element && element.bounds.width > 0 && element.bounds.height > 0) {
      const point = center(element.bounds);
      return { x: clampCoordinate(point.x, width), y: clampCoordinate(point.y, height) };
    }
  }
  return { x: clampCoordinate(width / 2, width), y: clampCoordinate(height / 2, height) };
}

function requireTarget(decision: Decision): DecisionTarget {
  if (!decision.target) {
    throw new DcuError("agent_internal", `A ${decision.operation} decision arrived without a target`);
  }
  return decision.target;
}

function requireText(context: ActionContext, operation: DecisionOperation): ResolvedText {
  if (!context.text) {
    throw new DcuError("agent_internal", `A ${operation} decision arrived without resolved text`);
  }
  return context.text;
}

function typeCall(context: ActionContext, text: string): NativeCall {
  return {
    method: needsPaste(text) ? "paste-text" : "type-text",
    params: { ...windowParams(context), text, observe: "text" }
  };
}

function focusCall(context: ActionContext, target: DecisionTarget): NativeCall {
  // No `observe` here: the observation that matters is the one after the typing.
  return {
    method: "click",
    params: { ...windowParams(context), elementIndex: target.elementIndex, observationId: context.observationId }
  };
}

/**
 * `drag` is deliberately absent. It needs a press, a paced path and a release
 * against a target the model cannot see the geometry of, and this stage has no
 * way to verify any of the three.
 */
export function planAction(decision: Decision, context: ActionContext): ActionPlan {
  const base: Pick<ActionPlan, "operation" | "delayMs" | "observeVia"> = {
    operation: decision.operation,
    delayMs: 0,
    observeVia: "attached"
  };
  switch (decision.operation) {
    case "click": {
      const target = requireTarget(decision);
      return {
        ...base,
        calls: [{
          method: "click",
          params: {
            ...windowParams(context),
            elementIndex: target.elementIndex,
            observationId: context.observationId,
            observe: "text"
          }
        }]
      };
    }
    case "enter_text": {
      const target = requireTarget(decision);
      const text = requireText(context, decision.operation);
      // A field with a value pattern is written outright, which is the only
      // mechanism that really replaces what is there. Everything else is focused
      // and typed into, and the goal says what the field should end up holding.
      if (target.ops.includes("set_value")) {
        return {
          ...base,
          mechanism: "set_value",
          calls: [{
            method: "set-value",
            params: {
              ...windowParams(context),
              elementIndex: target.elementIndex,
              observationId: context.observationId,
              value: text.text,
              observe: "text"
            }
          }],
          text
        };
      }
      const calls: NativeCall[] = [];
      if (!isTargetFocused(target, context)) calls.push(focusCall(context, target));
      calls.push(typeCall(context, text.text));
      return { ...base, mechanism: "type_text", calls, text };
    }
    case "press_key": {
      if (!decision.key) throw new DcuError("agent_internal", "A press_key decision arrived without a key");
      return {
        ...base,
        calls: [{ method: "press-key", params: { ...windowParams(context), key: decision.key, observe: "text" } }]
      };
    }
    case "hotkey": {
      if (!decision.chord) throw new DcuError("agent_internal", "A hotkey decision arrived without a chord");
      return {
        ...base,
        calls: [{ method: "hotkey", params: { ...windowParams(context), key: decision.chord, observe: "text" } }]
      };
    }
    case "scroll": {
      if (!decision.region) throw new DcuError("agent_internal", "A scroll decision arrived without an area");
      const point = regionPoint(decision.region, context.observation);
      return {
        ...base,
        calls: [{
          method: "scroll",
          params: {
            ...windowParams(context),
            x: point.x,
            y: point.y,
            direction: decision.direction ?? "down",
            amount: SCROLL_AMOUNT,
            observationId: context.observationId,
            observe: "text"
          }
        }]
      };
    }
    default:
      return { ...base, delayMs: WAIT_MS, observeVia: "result", calls: [observeCall(context)] };
  }
}

export type CallOutcome =
  | { kind: "ok"; result: Record<string, unknown>; elapsedMs: number }
  | { kind: "error"; code: string; message: string; elapsedMs: number; requestSent?: boolean };

export type Invoke = (call: NativeCall) => Promise<CallOutcome>;

export interface PlanRun {
  attempted: { call: NativeCall; outcome: CallOutcome }[];
  last: CallOutcome;
}

/**
 * Runs the calls in order and stops at the first error, so a failed focus click
 * never types into whatever had the focus instead. A `set-value` that comes back
 * `pattern_unavailable` stops here too, and the loop refuses the step: typing
 * into the field instead inserts at the caret rather than replacing, which
 * leaves a half-merged string in someone's real document. Selecting all first was
 * considered and rejected — in an app that does not scope select-all to the
 * focused field it would select the whole document.
 */
export async function runPlan(
  plan: ActionPlan,
  invoke: Invoke,
  sleep: (ms: number) => Promise<void>
): Promise<PlanRun> {
  if (plan.delayMs > 0) await sleep(plan.delayMs);
  const attempted: PlanRun["attempted"] = [];
  let last: CallOutcome | undefined;
  for (const call of plan.calls) {
    const outcome = await invoke(call);
    attempted.push({ call, outcome });
    last = outcome;
    if (outcome.kind === "error") break;
  }
  if (!last) throw new DcuError("agent_internal", "An action plan carried no calls");
  return { attempted, last };
}

/** A quoted run, in any of the four pairs a goal is realistically written with. */
const QUOTED_RUN = /"([^"\n]+)"|'([^'\n]+)'|“([^”\n]+)”|「([^」\n]+)」/gu;

export function goalQuotes(goal: string): string[] {
  const runs: string[] = [];
  for (const match of goal.matchAll(QUOTED_RUN)) {
    const value = match[1] ?? match[2] ?? match[3] ?? match[4];
    if (value && value.trim().length > 0) runs.push(value);
  }
  return runs;
}

/**
 * Jev answers with a choice, never with a string, so a typing decision arrives
 * with a field and no text. The text comes from the goal or from a preset the
 * caller supplied, and from nowhere else: inventing it, or offering the model a
 * list of guesses to ratify, would put our words into someone's real document.
 */
export class TextResolver {
  private readonly quotes: string[];
  private readonly presets: ReadonlyMap<string, { key: string; text: string }>;
  private cursor = 0;

  constructor(goal: string, presets: Readonly<Record<string, string>> = {}) {
    this.quotes = goalQuotes(goal);
    const map = new Map<string, { key: string; text: string }>();
    for (const [key, text] of Object.entries(presets)) {
      const normalized = normalizeLabel(key);
      if (normalized) map.set(normalized, { key, text });
    }
    this.presets = map;
  }

  /** How many quoted runs are left, for the dry-run report. */
  get remainingQuotes(): number {
    return Math.max(0, this.quotes.length - this.cursor);
  }

  private preset(label: string): { key: string; text: string } | undefined {
    const normalized = normalizeLabel(label);
    const exact = this.presets.get(normalized);
    if (exact) return exact;
    // One containing key resolves; two is an ambiguity we refuse rather than guess.
    const contained = [...this.presets.values()].filter(entry => normalized.includes(normalizeLabel(entry.key)));
    return contained.length === 1 ? contained[0] : undefined;
  }

  /** Undefined means the run must stop: this field has no text we are allowed to supply. */
  resolve(label: string): ResolvedText | undefined {
    if (this.cursor < this.quotes.length) {
      const text = this.quotes[this.cursor];
      this.cursor += 1;
      return { text, source: "goal_quote" };
    }
    const preset = this.preset(label);
    if (preset) return { text: preset.text, source: "preset", key: preset.key };
    return undefined;
  }
}

/** Every key a trace record must not carry out of this process. */
const UNTRACEABLE_PARAMS: ReadonlySet<string> = new Set(["sessionId", "text", "value"]);

/**
 * The params as the trace may record them. The session id is a credential and
 * the typed string may be a preset the caller treats as a secret, so the text is
 * reduced to its length.
 */
export function traceableParams(call: NativeCall): Record<string, unknown> {
  const params: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(call.params)) {
    if (!UNTRACEABLE_PARAMS.has(key)) params[key] = value;
  }
  const text = call.params.text ?? call.params.value;
  if (typeof text === "string") params.textChars = text.length;
  return params;
}

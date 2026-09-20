import { createHash } from "node:crypto";
import { DEFAULT_MIN_CONFIDENCE, DEFAULT_MIN_MARGIN } from "./questions.js";
import type { Decision, GateSettings } from "./questions.js";

/**
 * The guards this layer puts between a 87.5%-per-step decision and a real
 * desktop. Every one of them answers in code: none of them is ever asked of the
 * model, because a model that chose the action is the wrong judge of whether the
 * action should be taken.
 */

/**
 * Placeholders, not tuned values. We have not yet observed how Jev's confidence
 * is distributed on this task, so these two numbers are a starting point to be
 * replaced once real answers have been scored. Both are configurable per run,
 * and the full probability vector stays on the decision so a caller can judge
 * for itself.
 */
export function resolveGate(options: { minConfidence?: number; minMargin?: number } = {}): GateSettings {
  return {
    minConfidence: options.minConfidence ?? DEFAULT_MIN_CONFIDENCE,
    minMargin: options.minMargin ?? DEFAULT_MIN_MARGIN
  };
}

/**
 * Labels that name an action which cannot be undone by pressing the same button
 * again. "Remove filter" matches and is a false positive; stopping to ask is the
 * correct direction for a tool driving someone's real desktop, so it stays.
 */
export const DESTRUCTIVE_LABEL_PATTERNS: readonly RegExp[] = [
  /delete|remove|erase|format|uninstall|reset|factory|shut\s?down|restart|sign\s?out|log\s?out|discard|overwrite|permanently/i,
  /삭제|제거|지우기|포맷|초기화|영구|덮어쓰|로그아웃|휴지통|복구할 수 없/
];

/** A single key that destroys what is selected. */
export const DESTRUCTIVE_KEYS: ReadonlySet<string> = new Set(["delete"]);

/** Chords that close the work rather than change it. */
export const DESTRUCTIVE_CHORDS: ReadonlySet<string> = new Set(["alt+f4", "ctrl+w"]);

export type DestructiveMode = "stop" | "confirm" | "allow";

export interface DestructiveFinding {
  matched: "label" | "key" | "chord";
  /** The part of the label, or the whole key, that tripped the guard. */
  trigger: string;
  /** The label, key or chord the trigger was found in. */
  text: string;
  /** The exact action in words, so a rerun with --on-destructive allow is one flag. */
  action: string;
}

export interface DestructiveGate {
  requested: DestructiveMode;
  /** `confirm` is never resolved to `allow`; with no way to ask, it stops. */
  effective: "stop" | "allow";
  degradedFrom?: DestructiveMode;
  degradedReason?: string;
}

/** The action in words, target and all, so the report names what would have happened. */
export function describeAction(decision: Decision): string {
  const target = decision.target;
  const region = decision.region;
  switch (decision.operation) {
    case "click":
      return target ? `press the ${target.role} "${target.label}" — ${target.place}` : "press a control";
    case "enter_text":
      return target
        ? `put text into the ${target.role} "${target.label}" — ${target.place}`
        : "put text into a field";
    case "press_key":
      return `send the ${decision.key ?? "unknown"} key`;
    case "hotkey":
      return `send the ${decision.chord ?? "unknown"} shortcut`;
    case "scroll":
      return region
        ? `scroll ${region.label} ${decision.direction ?? "down"}`
        : `scroll ${decision.direction ?? "down"}`;
    default:
      return "wait for the window to settle and look again";
  }
}

/**
 * Judged on the chosen action alone. Screening the candidate list instead would
 * stop every run that merely has a Delete button on screen.
 */
export function findDestructive(decision: Decision): DestructiveFinding | undefined {
  const label = decision.target?.label;
  if (label) {
    for (const pattern of DESTRUCTIVE_LABEL_PATTERNS) {
      const match = pattern.exec(label);
      if (match) {
        return { matched: "label", trigger: match[0], text: label, action: describeAction(decision) };
      }
    }
  }
  const key = decision.key?.trim().toLowerCase();
  if (decision.operation === "press_key" && key && DESTRUCTIVE_KEYS.has(key)) {
    return { matched: "key", trigger: key, text: key, action: describeAction(decision) };
  }
  const chord = decision.chord?.trim().toLowerCase();
  if (decision.operation === "hotkey" && chord && DESTRUCTIVE_CHORDS.has(chord)) {
    return { matched: "chord", trigger: chord, text: chord, action: describeAction(decision) };
  }
  return undefined;
}

/**
 * There is no console to ask on: under MCP there is no TTY at all, and the loop
 * that owns the desktop cannot read stdin while it is driving it. `confirm`
 * therefore resolves to `stop` and says why, so the operator reruns with `allow`
 * rather than finding the action was taken anyway.
 */
export function resolveDestructiveMode(requested: DestructiveMode = "stop", isTty = false): DestructiveGate {
  if (requested === "allow") return { requested, effective: "allow" };
  if (requested === "stop") return { requested, effective: "stop" };
  return {
    requested,
    effective: "stop",
    degradedFrom: "confirm",
    degradedReason: isTty ? "no_confirmation_channel" : "no_tty"
  };
}

/** The shape of a candidate this module needs; the geometry stays out of it. */
export interface HashableCandidate {
  role: string;
  label: string;
  value?: string;
  stateWord?: string;
}

/**
 * What the screen offers, independent of the order the elements arrived in. Two
 * observations of the same unchanged window hash the same even when UIA
 * renumbers the tree, which is exactly the case this has to catch.
 */
export function candidatesHash(candidates: readonly HashableCandidate[]): string {
  const rows = candidates
    .map(candidate => `${candidate.role}|${candidate.label}|${candidate.value ?? ""}|${candidate.stateWord ?? ""}`)
    .sort();
  return createHash("sha256").update(rows.join("\n"), "utf8").digest("hex").slice(0, 16);
}

/** The action by what it acts on, never by option id: `e12` names a different control next step. */
export function actionKey(decision: Decision): string {
  const parts: string[] = [decision.operation];
  if (decision.target) parts.push(decision.target.role, decision.target.label);
  if (decision.region) parts.push(decision.region.label);
  if (decision.key) parts.push(decision.key);
  if (decision.chord) parts.push(decision.chord);
  if (decision.direction) parts.push(decision.direction);
  return parts.join("|");
}

/** Three identical screens in a row while repeating the same action is a loop, not patience. */
export const NO_PROGRESS_REPEATS = 3;

export class NoProgressDetector {
  private hash: string | undefined;
  private action: string | undefined;
  private repeats = 0;

  /** True when this step should be refused. Call once per action-bearing decision. */
  record(hash: string, action: string): boolean {
    this.repeats = hash === this.hash ? this.repeats + 1 : 1;
    const repeatedAction = action === this.action;
    this.hash = hash;
    this.action = action;
    return this.repeats >= NO_PROGRESS_REPEATS && repeatedAction;
  }

  /** Something moved, so the count starts again. */
  reset(): void {
    this.hash = undefined;
    this.action = undefined;
    this.repeats = 0;
  }
}

export const DEFAULT_MAX_STEPS = 25;
export const MAX_STEPS_CEILING = 100;
/** Stale observations and busy retries buy new decisions without buying new steps. */
export const DECISION_HEADROOM = 10;
export const DEFAULT_MAX_DURATION_MS = 180_000;
export const DEFAULT_MAX_INPUT_TOKENS = 400_000;

export interface Budgets {
  maxSteps: number;
  maxDecisions: number;
  maxDurationMs: number;
  maxInputTokens: number;
}

export interface BudgetOptions {
  maxSteps?: number;
  maxDurationMs?: number;
  maxInputTokens?: number;
}

export function resolveBudgets(options: BudgetOptions = {}): Budgets {
  const requested = options.maxSteps ?? DEFAULT_MAX_STEPS;
  const maxSteps = Math.max(1, Math.min(MAX_STEPS_CEILING, Math.floor(requested)));
  return {
    maxSteps,
    maxDecisions: maxSteps + DECISION_HEADROOM,
    maxDurationMs: options.maxDurationMs ?? DEFAULT_MAX_DURATION_MS,
    maxInputTokens: options.maxInputTokens ?? DEFAULT_MAX_INPUT_TOKENS
  };
}

export interface BudgetCounters {
  steps: number;
  decisions: number;
  elapsedMs: number;
  inputTokens: number;
}

export type BudgetReason = "max_steps" | "max_decisions" | "max_duration" | "max_input_tokens";

export function exhaustedBudget(budgets: Budgets, counters: BudgetCounters): BudgetReason | undefined {
  if (counters.steps >= budgets.maxSteps) return "max_steps";
  if (counters.decisions >= budgets.maxDecisions) return "max_decisions";
  if (counters.elapsedMs >= budgets.maxDurationMs) return "max_duration";
  if (counters.inputTokens >= budgets.maxInputTokens) return "max_input_tokens";
  return undefined;
}

/**
 * The native side blanks the *values* of password-like fields but not their
 * names, and this state leaves the machine. This is the second pass: it works on
 * the label as well, because "Enter your PIN" is a name and the six digits next
 * to it are not.
 */
export const SENSITIVE_LABEL = /password|passwd|pin|otp|secret|token|card|비밀번호|인증번호|카드번호/iu;

/** Six digits or more is a code, an account or a card number often enough to be worth losing. */
export const SENSITIVE_VALUE = /\d{6,}/u;

export interface RedactionOptions {
  /** Strip every value, not only the ones that look sensitive. */
  noValues?: boolean;
}

export interface RedactionResult<T> {
  candidates: T[];
  redacted: number;
}

export function shouldRedactValue(label: string, value: string, options: RedactionOptions = {}): boolean {
  if (options.noValues) return true;
  return SENSITIVE_LABEL.test(label) || SENSITIVE_VALUE.test(value);
}

/**
 * Returns copies: the caller's filter result is left alone so that the geometry
 * the executor still needs is never quietly rewritten.
 */
export function redactCandidates<T extends { label: string; value?: string }>(
  candidates: readonly T[],
  options: RedactionOptions = {}
): RedactionResult<T> {
  let redacted = 0;
  const result = candidates.map(candidate => {
    if (candidate.value === undefined || candidate.value === "") return { ...candidate };
    if (!shouldRedactValue(candidate.label, candidate.value, options)) return { ...candidate };
    redacted += 1;
    const copy: T = { ...candidate };
    // The key goes, not just its contents: an empty string still serializes.
    Reflect.deleteProperty(copy, "value");
    return copy;
  });
  return { candidates: result, redacted };
}

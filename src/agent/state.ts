import type { Candidate, RegionSummary } from "./types.js";

/**
 * The six actions the decision layer may choose between. `Operation` in
 * `types.ts` is the filter's vocabulary — what a control affords — and stops at
 * four; a decision may also send a key, send a chord, or do nothing at all.
 * Typing into a field and replacing its contents are one action here: they are
 * the same outcome reached by two mechanisms, and the mechanism is the
 * executor's to pick.
 */
export type DecisionOperation =
  | "click" | "enter_text" | "press_key" | "hotkey" | "scroll" | "wait";

export const DECISION_OPERATIONS: readonly DecisionOperation[] = [
  "click", "enter_text", "press_key", "hotkey", "scroll", "wait"
];

/** A candidate as the state sees it: the filter's code-only geometry is not part of it. */
export type StateCandidate = Omit<Candidate, "center" | "area" | "mergedIndexes">;

/**
 * One past step, in words. It carries a label and never an option id: `e12` names
 * a different element in the next observation, so "clicked e12" would be actively
 * misleading rather than merely useless.
 */
export interface HistoryStep {
  operation: DecisionOperation;
  targetLabel?: string;
  key?: string;
  chord?: string;
  direction?: string;
  outcome?: string;
}

export interface StateWindow {
  app?: string;
  title?: string;
}

export interface StateInputs {
  goal: string;
  window: StateWindow;
  candidates: readonly StateCandidate[];
  disabledShortlist?: readonly StateCandidate[];
  regions?: readonly RegionSummary[];
  history?: readonly HistoryStep[];
  notes?: readonly string[];
  /** Read by `buildQuestions`, never by `serializeState`. */
  gate?: { minConfidence?: number; minMargin?: number };
}

/** Only the last few steps go in: older ones are the part of the state that rots. */
const HISTORY_DEPTH = 5;
const OPTION_COLUMN = 6;

function pad(text: string, width: number): string {
  return text.length >= width ? text : text + " ".repeat(width - text.length);
}

function quote(text: string): string {
  return `"${text.replace(/"/gu, "'")}"`;
}

/** `e3   toggle "Bluetooth" — currently on — top-right`. Words only, never a rectangle. */
export function describeControl(candidate: StateCandidate): string {
  const parts = [`${candidate.role} ${quote(candidate.label)}`];
  if (candidate.altLabel) parts.push(`also called ${quote(candidate.altLabel)}`);
  if (candidate.stateWord) parts.push(candidate.stateWord);
  if (candidate.value) parts.push(`showing ${quote(candidate.value)}`);
  parts.push(candidate.place);
  return parts.join(" — ");
}

function controlLine(candidate: StateCandidate): string {
  return `${pad(candidate.optionId, OPTION_COLUMN)}${describeControl(candidate)}`;
}

/** The whole-window region names itself twice; say it once. */
export function describeRegion(region: RegionSummary): string {
  return region.label === region.place ? region.label : `${region.label} — ${region.place}`;
}

export function describeStep(step: HistoryStep): string {
  const label = step.targetLabel ? quote(step.targetLabel) : "a control";
  let sentence: string;
  switch (step.operation) {
    case "click":
      sentence = `Pressed ${label}.`;
      break;
    case "enter_text":
      sentence = `Put text into ${label}.`;
      break;
    case "press_key":
      sentence = `Sent the ${step.key ?? "unknown"} key.`;
      break;
    case "hotkey":
      sentence = `Sent the ${step.chord ?? "unknown"} shortcut.`;
      break;
    case "scroll":
      sentence = `Scrolled ${label} ${step.direction ?? "further along"}.`;
      break;
    default:
      sentence = "Waited for the window to settle.";
      break;
  }
  return step.outcome ? `${sentence} ${step.outcome}` : sentence;
}

function windowSection(window: StateWindow): string {
  const title = window.title?.trim();
  const app = window.app?.trim();
  if (title && app) return `Window: ${title}, shown by ${app}.`;
  if (title) return `Window: ${title}.`;
  if (app) return `Window: a window of ${app}.`;
  return "Window: the window in front, which reports no title.";
}

function historySection(history: readonly HistoryStep[]): string {
  if (history.length === 0) return "What has happened so far: nothing yet — this is the first step.";
  const recent = history.slice(-HISTORY_DEPTH);
  const lines = recent.map((step, position) => `${position + 1}. ${describeStep(step)}`);
  return ["What has happened so far, oldest first:", ...lines].join("\n");
}

function controlsSection(candidates: readonly StateCandidate[]): string {
  if (candidates.length === 0) {
    return "The controls on screen now: none of the controls in this window can be acted on.";
  }
  return ["The controls on screen now:", ...candidates.map(controlLine)].join("\n");
}

function unavailableSection(disabled: readonly StateCandidate[]): string {
  if (disabled.length === 0) return "What is unavailable right now: nothing — every control listed above can be used.";
  const lines = disabled.map(candidate => `${pad("-", OPTION_COLUMN)}${describeControl(candidate)}`);
  return ["What is unavailable right now — these are greyed out and cannot be used:", ...lines].join("\n");
}

function notesSection(inputs: StateInputs): string {
  const notes: string[] = [];
  const regions = inputs.regions ?? [];
  if (regions.length > 0) {
    const named = regions.map(region => `${region.regionId} — ${describeRegion(region)}`);
    notes.push(`The areas that can be scrolled: ${named.join("; ")}.`);
  }
  notes.push("The e-numbers name the controls of this screen only; they change when the screen changes.");
  for (const note of inputs.notes ?? []) {
    const trimmed = note.trim();
    if (trimmed) notes.push(trimmed);
  }
  return ["Notes:", ...notes.map(note => `- ${note}`)].join("\n");
}

/**
 * The state Jev is asked to read, as one section per array entry. Accuracy falls
 * as the state grows with detail the model cannot use, so coordinates, sizes and
 * handles stay in code and only words go out.
 */
export function serializeState(inputs: StateInputs): string[] {
  return [
    `Goal: ${inputs.goal.trim()}`,
    windowSection(inputs.window),
    historySection(inputs.history ?? []),
    controlsSection(inputs.candidates),
    unavailableSection(inputs.disabledShortlist ?? []),
    notesSection(inputs)
  ];
}

/**
 * A chars/4 estimate, not a count. It exists so a caller can refuse an oversized
 * state before spending a request, and so the real `usage.input_tokens` from a
 * reply can calibrate it later. Always label it as an estimate where it is shown.
 */
export function estimateTokens(state: readonly string[]): number {
  let characters = 0;
  for (const section of state) characters += section.length + 1;
  return Math.ceil(characters / 4);
}

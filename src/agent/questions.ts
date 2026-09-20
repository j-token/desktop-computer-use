import { DcuError } from "../errors.js";
import type { JevAnswers, JevChoiceQuestion, JevQuestion } from "./jev.js";
import { DECISION_OPERATIONS, describeControl, describeRegion, serializeState } from "./state.js";
import type { DecisionOperation, StateCandidate, StateInputs } from "./state.js";
import type { CandidateRole, Operation, RegionSummary } from "./types.js";

/** The Jev choice question accepts at most this many options. */
export const MAX_CHOICE_OPTIONS = 255;

/**
 * Placeholders, not tuned values. We have not yet observed how Jev's confidence
 * is distributed on this task, so these two numbers are a starting point to be
 * replaced once real answers have been scored. Both are configurable, and the
 * full probability vector stays on the decision so a caller can judge for itself.
 */
export const DEFAULT_MIN_CONFIDENCE = 0.55;
export const DEFAULT_MIN_MARGIN = 0.15;

export interface GateSettings {
  minConfidence: number;
  minMargin: number;
}

/** A single key sent to the window. Nine keys, fixed, so the answer is always executable. */
export const PRESS_KEYS: Readonly<Record<string, string>> = {
  enter: "Confirm what is in front, or run the line that has been entered.",
  escape: "Back out of what is in front and leave it unchanged.",
  tab: "Move the focus on to the next control.",
  down: "Move the selection or the caret one step down.",
  up: "Move the selection or the caret one step up.",
  backspace: "Delete the one character or item before the caret.",
  delete: "Delete the one character or item after the caret.",
  f2: "Rename whatever is selected.",
  f5: "Refresh what is on screen."
};

/** An allowlist of eight chords. Anything outside it is not a decision this layer can make. */
export const HOTKEY_CHORDS: Readonly<Record<string, string>> = {
  "ctrl+s": "Save the work that is open.",
  "ctrl+a": "Select everything in the area that has the focus.",
  "ctrl+c": "Copy what is selected.",
  "ctrl+v": "Paste what was copied.",
  "ctrl+z": "Undo the last change.",
  "ctrl+f": "Open the find box to search within what is open.",
  "ctrl+n": "Open a new window or a new document.",
  "ctrl+w": "Close the tab or the window that is in front."
};

export const SCROLL_DIRECTIONS: Readonly<Record<string, string>> = {
  down: "Bring what lies below the visible part into view.",
  up: "Bring what lies above the visible part into view.",
  left: "Bring what lies to the left of the visible part into view.",
  right: "Bring what lies to the right of the visible part into view."
};

/**
 * One text-entry option, not two. `type_text` and `set_value` named the same
 * outcome by two mechanisms, and asking the model to tell them apart split its
 * probability mass across a distinction that changes nothing on screen: on the
 * run that prompted this, `set_value` 48%, `type_text` 31% and `click` 20% left
 * the answer at 0.40 confidence and stopped a run the model had otherwise
 * decided correctly. The model chooses what; the executor chooses how.
 */
const OPERATION_DESCRIPTIONS: Readonly<Record<DecisionOperation, string>> = {
  click: "Press one of the controls listed on screen.",
  enter_text:
    "Put the text the goal calls for into one of the text fields listed on screen, " +
    "replacing whatever that field holds now.",
  press_key: "Send one single key to the window, with no modifier held down.",
  hotkey: "Send one keyboard shortcut, with a modifier held down.",
  scroll: "Scroll one of the areas listed on screen to bring more of it into view.",
  wait: "Let the window finish what it is doing and look again, without touching anything."
};

/**
 * Both `progress` and `obstacle` are two-option choices rather than a yes/no
 * judgement. A declarative statement with a yes/no answer drove one model to
 * answer "true" on 18 of 18 items (38.9% accurate) where the same judgement as a
 * two-option descriptive choice reached 94.4%. The two descriptions are kept to
 * deliberately similar length and detail: an asymmetry is itself a bias.
 */
const PROGRESS_CRITERIA: Record<string, string> = {
  goal_reached:
    "The goal is already satisfied by what this window shows now, so no further control has to be used.",
  work_remains:
    "The goal is not yet satisfied by what this window shows now, so at least one more control has to be used."
};

const OBSTACLE_CRITERIA: Record<string, string> = {
  path_clear:
    "The controls listed on screen are enough to carry the goal forward, and nothing here is holding the work up.",
  blocked:
    "The controls listed on screen are not enough to carry the goal forward, and something here is holding the work up."
};

const TARGET_SUFFIX = "Answer this even if you chose a different action.";

export interface QuestionPlan {
  state: string[];
  questions: Record<string, JevQuestion>;
  /** Option id back to the candidate it was minted from, so an answer resolves without a re-search. */
  candidatesByOption: ReadonlyMap<string, StateCandidate>;
  regionsByOption: ReadonlyMap<string, RegionSummary>;
  gate: GateSettings;
}

export interface DecisionTarget {
  optionId: string;
  elementIndex: number;
  role: CandidateRole;
  label: string;
  place: string;
  /**
   * What the filter judged this control affords. The model never sees it; it is
   * here so the executor can pick the mechanism an `enter_text` decision is
   * carried out by without re-reading the observation.
   */
  ops: readonly Operation[];
}

export interface DecisionRegion {
  regionId: string;
  label: string;
  place: string;
  elementIndex?: number;
}

export interface GateReading {
  question: string;
  choice: string;
  confidence: number;
  margin: number;
  /** Whether the floors were actually applied to this answer, so a reader tells "passed" from "not checked". */
  gated: boolean;
}

export interface Decision {
  ok: true;
  status: "continue" | "done" | "blocked";
  operation: DecisionOperation;
  target?: DecisionTarget;
  region?: DecisionRegion;
  key?: string;
  chord?: string;
  direction?: string;
  /** The operation answer's confidence and margin; every reading is in `gate`. */
  confidence: number;
  margin: number;
  gate: GateSettings & { readings: GateReading[] };
  answers: JevAnswers;
}

export type GateFailureReason =
  | "missing_answer"
  | "malformed_answer"
  | "unknown_choice"
  | "low_confidence"
  | "low_margin";

export interface GateFailure {
  ok: false;
  reason: GateFailureReason;
  question: string;
  message: string;
  choice?: string;
  offeredCount?: number;
  confidence?: number;
  margin?: number;
  gate: GateSettings;
  answers: JevAnswers;
}

function assertSize(key: string, size: number): void {
  if (size > MAX_CHOICE_OPTIONS) {
    throw new DcuError(
      "agent_internal",
      `Question ${key} offers ${size} options; the Jev choice limit is ${MAX_CHOICE_OPTIONS}`
    );
  }
}

function choiceQuestion(key: string, instructions: string, criteria: Record<string, string>): JevChoiceQuestion {
  assertSize(key, Object.keys(criteria).length);
  return { type: "choice", instructions, criteria };
}

function criteriaFrom(candidates: readonly StateCandidate[]): Record<string, string> {
  const criteria: Record<string, string> = {};
  for (const candidate of candidates) criteria[candidate.optionId] = describeControl(candidate);
  return criteria;
}

function regionCriteria(regions: readonly RegionSummary[]): Record<string, string> {
  const criteria: Record<string, string> = {};
  for (const region of regions) criteria[region.regionId] = describeRegion(region);
  return criteria;
}

/** Which answer names the thing the chosen operation would act on. */
export function targetQuestionFor(operation: DecisionOperation): string | undefined {
  switch (operation) {
    case "click": return "click_target";
    case "enter_text": return "text_target";
    case "press_key": return "press_key_choice";
    case "hotkey": return "hotkey_choice";
    case "scroll": return "scroll_target";
    default: return undefined;
  }
}

/**
 * Every question goes out in one request. Extra questions add almost no latency
 * because they are evaluated in parallel, so the targets are asked speculatively
 * and the ones the chosen operation does not need are discarded on arrival.
 */
export function buildQuestions(inputs: StateInputs): QuestionPlan {
  const candidates = inputs.candidates;
  const regions = inputs.regions ?? [];
  const clickable = candidates.filter(candidate => candidate.ops.includes("click"));
  // One pool, whichever mechanism the field affords: a field that can only be
  // typed into and a field whose value can be replaced are the same choice here.
  const textFields = candidates.filter(
    candidate => candidate.ops.includes("type_text") || candidate.ops.includes("set_value")
  );

  const questions: Record<string, JevQuestion> = {};
  const candidatesByOption = new Map<string, StateCandidate>();
  const regionsByOption = new Map<string, RegionSummary>();
  const offered: DecisionOperation[] = [];

  const addTarget = (
    key: string,
    instructions: string,
    pool: readonly StateCandidate[],
    operation: DecisionOperation
  ): void => {
    // A question with an empty criteria map asks nothing; the operation that
    // would have needed it leaves the menu with it.
    if (pool.length === 0) return;
    questions[key] = choiceQuestion(key, `${instructions} ${TARGET_SUFFIX}`, criteriaFrom(pool));
    for (const candidate of pool) candidatesByOption.set(candidate.optionId, candidate);
    offered.push(operation);
  };

  addTarget("click_target", "Which control should be pressed next to move the goal forward?", clickable, "click");
  addTarget(
    "text_target",
    "Which field should the text the goal calls for be put into?",
    textFields,
    "enter_text"
  );

  if (regions.length > 0) {
    questions.scroll_target = choiceQuestion(
      "scroll_target",
      `Which area on screen should be scrolled? ${TARGET_SUFFIX}`,
      regionCriteria(regions)
    );
    questions.scroll_direction = choiceQuestion(
      "scroll_direction",
      `Which way should that area be scrolled? ${TARGET_SUFFIX}`,
      { ...SCROLL_DIRECTIONS }
    );
    for (const region of regions) regionsByOption.set(region.regionId, region);
    offered.push("scroll");
  }

  // These three need nothing from the screen, so they are always on the menu.
  questions.press_key_choice = choiceQuestion(
    "press_key_choice",
    `Which single key should be sent to the window? ${TARGET_SUFFIX}`,
    { ...PRESS_KEYS }
  );
  questions.hotkey_choice = choiceQuestion(
    "hotkey_choice",
    `Which keyboard shortcut should be sent to the window? ${TARGET_SUFFIX}`,
    { ...HOTKEY_CHORDS }
  );
  offered.push("press_key", "hotkey", "wait");

  const menu = new Set(offered);
  const operationCriteria: Record<string, string> = {};
  for (const operation of DECISION_OPERATIONS) {
    if (menu.has(operation)) operationCriteria[operation] = OPERATION_DESCRIPTIONS[operation];
  }

  const ordered: Record<string, JevQuestion> = {
    progress: choiceQuestion(
      "progress",
      "Compare the goal with this window as it stands right now, and choose the description that fits.",
      { ...PROGRESS_CRITERIA }
    ),
    obstacle: choiceQuestion(
      "obstacle",
      "Compare the goal with this window as it stands right now, and choose the description that fits.",
      { ...OBSTACLE_CRITERIA }
    ),
    operation: choiceQuestion(
      "operation",
      "Pick the single next action that moves the goal forward from this window as it stands right now.",
      operationCriteria
    ),
    ...questions
  };

  return {
    state: serializeState(inputs),
    questions: ordered,
    candidatesByOption,
    regionsByOption,
    gate: {
      minConfidence: inputs.gate?.minConfidence ?? DEFAULT_MIN_CONFIDENCE,
      minMargin: inputs.gate?.minMargin ?? DEFAULT_MIN_MARGIN
    }
  };
}

/** Top probability minus the second. Counting and subtraction stay in code. */
export function marginOf(probabilities: Record<string, number>): number {
  const values = Object.values(probabilities)
    .filter(value => typeof value === "number" && Number.isFinite(value))
    .sort((left, right) => right - left);
  if (values.length === 0) return 0;
  if (values.length === 1) return values[0];
  return values[0] - values[1];
}

interface ChoiceReading {
  choice: string;
  confidence: number;
  margin: number;
}

function failure(
  reason: GateFailureReason,
  question: string,
  message: string,
  gate: GateSettings,
  answers: JevAnswers,
  extra: Partial<GateFailure> = {}
): GateFailure {
  return { ok: false, reason, question, message, gate, answers, ...extra };
}

function offeredKeys(plan: QuestionPlan, key: string): string[] {
  const question = plan.questions[key];
  if (!question || question.type !== "choice") return [];
  return Object.keys(question.criteria);
}

/**
 * Reads one choice answer, or says exactly why it cannot. A value we never
 * offered is rejected rather than coerced onto the nearest option we did.
 */
function readChoice(
  plan: QuestionPlan,
  answers: JevAnswers,
  key: string
): ChoiceReading | GateFailure {
  const gate = plan.gate;
  const raw = answers[key];
  if (raw === undefined || raw === null) {
    return failure("missing_answer", key, `The Jev reply carried no answer for ${key}`, gate, answers);
  }
  const record = typeof raw === "object" && !Array.isArray(raw) ? raw as Record<string, unknown> : undefined;
  const choice = record?.choice;
  const confidence = record?.confidence;
  const probabilities = record?.probabilities;
  if (
    typeof choice !== "string" ||
    typeof confidence !== "number" ||
    !Number.isFinite(confidence) ||
    !probabilities ||
    typeof probabilities !== "object" ||
    Array.isArray(probabilities)
  ) {
    return failure(
      "malformed_answer",
      key,
      `The answer for ${key} is not a choice with a probability vector and a confidence`,
      gate,
      answers
    );
  }
  const keys = offeredKeys(plan, key);
  if (!keys.includes(choice)) {
    return failure("unknown_choice", key, `The answer for ${key} names an option we never offered`, gate, answers, {
      choice,
      offeredCount: keys.length
    });
  }
  return { choice, confidence, margin: marginOf(probabilities as Record<string, number>) };
}

function gateCheck(
  plan: QuestionPlan,
  answers: JevAnswers,
  key: string,
  reading: ChoiceReading
): GateFailure | undefined {
  const gate = plan.gate;
  if (reading.confidence < gate.minConfidence) {
    return failure(
      "low_confidence",
      key,
      `The answer for ${key} is below the confidence floor of ${gate.minConfidence}`,
      gate,
      answers,
      { choice: reading.choice, confidence: reading.confidence, margin: reading.margin }
    );
  }
  if (reading.margin < gate.minMargin) {
    return failure(
      "low_margin",
      key,
      `The answer for ${key} is below the margin floor of ${gate.minMargin}`,
      gate,
      answers,
      { choice: reading.choice, confidence: reading.confidence, margin: reading.margin }
    );
  }
  return undefined;
}

function isFailure(value: ChoiceReading | GateFailure): value is GateFailure {
  return "ok" in value && value.ok === false;
}

/**
 * Gate what is acted on. Stopping is decided by `progress` and `obstacle`, so
 * those two have to clear the floors to end a run; acting is decided by
 * `operation` and its target, so those two have to clear the floors to touch the
 * desktop. Uncertainty anywhere else is a reason to keep working rather than to
 * fail: a run that has finished must not be reported as blocked because of the
 * confidence of an action that was never going to be performed. The budgets and
 * the no-progress detector already bound a run that keeps going.
 */
export function parseAnswers(plan: QuestionPlan, answers: JevAnswers): Decision | GateFailure {
  const progress = readChoice(plan, answers, "progress");
  if (isFailure(progress)) return progress;
  const obstacle = readChoice(plan, answers, "obstacle");
  if (isFailure(obstacle)) return obstacle;

  // A `goal_reached` or `blocked` that misses the floors does not fail the
  // decision; it is simply not confident enough to stop on, so the run continues
  // and the action gate below protects the step.
  const wantsDone = progress.choice === "goal_reached";
  const done = wantsDone && gateCheck(plan, answers, "progress", progress) === undefined;
  const wantsBlocked = !done && obstacle.choice === "blocked";
  const blocked = wantsBlocked && gateCheck(plan, answers, "obstacle", obstacle) === undefined;
  const status: Decision["status"] = done ? "done" : blocked ? "blocked" : "continue";
  const acting = status === "continue";

  const operation = readChoice(plan, answers, "operation");
  if (isFailure(operation)) return operation;
  if (acting) {
    const operationFailure = gateCheck(plan, answers, "operation", operation);
    if (operationFailure) return operationFailure;
  }

  const readings: GateReading[] = [
    { question: "progress", ...progress, gated: wantsDone },
    { question: "obstacle", ...obstacle, gated: wantsBlocked },
    { question: "operation", ...operation, gated: acting }
  ];

  const chosen = operation.choice as DecisionOperation;

  const decision: Decision = {
    ok: true,
    status,
    operation: chosen,
    confidence: operation.confidence,
    margin: operation.margin,
    gate: { ...plan.gate, readings },
    answers
  };

  const targetKey = targetQuestionFor(chosen);
  if (targetKey) {
    // Still resolved when the status is `done` or `blocked`: a target we never
    // offered means our own bookkeeping is wrong, whatever the run does next.
    const target = readChoice(plan, answers, targetKey);
    if (isFailure(target)) return target;
    if (acting) {
      const targetFailure = gateCheck(plan, answers, targetKey, target);
      if (targetFailure) return targetFailure;
    }
    readings.push({ question: targetKey, ...target, gated: acting });

    if (targetKey === "press_key_choice") {
      decision.key = target.choice;
    } else if (targetKey === "hotkey_choice") {
      decision.chord = target.choice;
    } else if (targetKey === "scroll_target") {
      const region = plan.regionsByOption.get(target.choice);
      if (!region) {
        return failure("unknown_choice", targetKey, `The answer for ${targetKey} names an area we never offered`,
          plan.gate, answers, { choice: target.choice, offeredCount: plan.regionsByOption.size });
      }
      decision.region = {
        regionId: region.regionId,
        label: region.label,
        place: region.place,
        ...(region.elementIndex === undefined ? {} : { elementIndex: region.elementIndex })
      };
      const direction = readChoice(plan, answers, "scroll_direction");
      if (isFailure(direction)) return direction;
      readings.push({ question: "scroll_direction", ...direction, gated: false });
      decision.direction = direction.choice;
    } else {
      const candidate = plan.candidatesByOption.get(target.choice);
      if (!candidate) {
        return failure("unknown_choice", targetKey, `The answer for ${targetKey} names a control we never offered`,
          plan.gate, answers, { choice: target.choice, offeredCount: plan.candidatesByOption.size });
      }
      decision.target = {
        optionId: candidate.optionId,
        elementIndex: candidate.elementIndex,
        role: candidate.role,
        label: candidate.label,
        place: candidate.place,
        ops: [...candidate.ops]
      };
    }
  }

  return decision;
}

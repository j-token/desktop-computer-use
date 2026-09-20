import test from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { buildCandidates } from "../dist/agent/filter.js";
import { readObservation } from "../dist/agent/explain.js";
import { estimateTokens, serializeState } from "../dist/agent/state.js";
import {
  HOTKEY_CHORDS,
  MAX_CHOICE_OPTIONS,
  PRESS_KEYS,
  SCROLL_DIRECTIONS,
  buildQuestions,
  parseAnswers
} from "../dist/agent/questions.js";

const FIXTURE_DIRECTORY = join(process.cwd(), "tests", "fixtures", "uia");
const FIXTURE_NAME = /^app-\d+-(asfound|foreground)\.json$/;
/** A recording of a keypad: a grid of small labelled buttons and no text field. */
const KEYPAD = "app-02-foreground.json";
/** The one shape the state must never carry: numbers the model cannot use. */
const GEOMETRY = /\brect=|\b\d+x\d+\b|\bpx\b/;

async function fixtureNames() {
  const entries = await readdir(FIXTURE_DIRECTORY);
  return entries.filter(name => FIXTURE_NAME.test(name)).sort();
}

async function loadFixture(name) {
  return readObservation(JSON.parse(await readFile(join(FIXTURE_DIRECTORY, name), "utf8")));
}

async function inputsFor(name, goal = "다음 단계를 진행한다") {
  const observation = await loadFixture(name);
  const filtered = buildCandidates(observation);
  return {
    goal,
    window: observation.window,
    candidates: filtered.candidates,
    disabledShortlist: filtered.disabledShortlist,
    regions: filtered.report.regions
  };
}

function candidate(index, ops, role = "button") {
  return {
    optionId: `e${index}`,
    elementIndex: index,
    role,
    label: `Control ${index}`,
    place: "middle-center",
    regionId: "r_main",
    ops
  };
}

function inputsWith(candidates, extra = {}) {
  return {
    goal: "테스트 목표",
    window: { app: "synthetic.exe", title: "테스트 창" },
    candidates,
    regions: [{ regionId: "r_main", label: "main window area", place: "main window area", candidateCount: candidates.length }],
    ...extra
  };
}

function choice(value, probabilities, confidence) {
  return { type: "choice", choice: value, probabilities, confidence };
}

/** A reply that passes every gate, so a test only has to spoil the one part it is about. */
function healthyAnswers(overrides = {}) {
  return {
    progress: choice("work_remains", { work_remains: 0.9, goal_reached: 0.1 }, 0.9),
    obstacle: choice("path_clear", { path_clear: 0.88, blocked: 0.12 }, 0.88),
    operation: choice("click", { click: 0.8, wait: 0.1, press_key: 0.05, hotkey: 0.03, scroll: 0.02 }, 0.82),
    click_target: choice("e1", { e1: 0.7, e2: 0.2, e3: 0.1 }, 0.74),
    ...overrides
  };
}

test("every criteria key resolves back to a candidate or an allowlisted key", async () => {
  const allowlisted = new Set([
    "goal_reached", "work_remains", "path_clear", "blocked",
    "click", "enter_text", "press_key", "hotkey", "scroll", "wait",
    ...Object.keys(PRESS_KEYS),
    ...Object.keys(HOTKEY_CHORDS),
    ...Object.keys(SCROLL_DIRECTIONS)
  ]);
  for (const name of await fixtureNames()) {
    const inputs = await inputsFor(name);
    const plan = buildQuestions(inputs);
    const regionIds = new Set(inputs.regions.map(region => region.regionId));
    for (const [key, question] of Object.entries(plan.questions)) {
      for (const option of Object.keys(question.criteria)) {
        const resolved =
          plan.candidatesByOption.has(option) ||
          regionIds.has(option) ||
          allowlisted.has(option);
        assert.ok(resolved, `${name} ${key} offered ${option}, which resolves to nothing`);
        const candidate = plan.candidatesByOption.get(option);
        if (candidate) assert.equal(Number(option.slice(1)), candidate.elementIndex, `${name} ${option}`);
      }
    }
  }
});

test("no question ever goes out with an empty criteria map", async () => {
  for (const name of await fixtureNames()) {
    const plan = buildQuestions(await inputsFor(name));
    for (const [key, question] of Object.entries(plan.questions)) {
      const size = Array.isArray(question.criteria)
        ? question.criteria.length
        : Object.keys(question.criteria).length;
      assert.ok(size > 0, `${name} ${key} would have asked nothing`);
    }
  }
  // A window with nothing to act on still asks the three questions that need no control.
  const bare = buildQuestions(inputsWith([], { regions: [] }));
  assert.deepEqual(Object.keys(bare.questions), ["progress", "obstacle", "operation", "press_key_choice", "hotkey_choice"]);
  assert.deepEqual(Object.keys(bare.questions.operation.criteria), ["press_key", "hotkey", "wait"]);
});

test("enter_text and text_target both vanish when the window has no text field", () => {
  const withoutText = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  assert.equal(withoutText.questions.text_target, undefined);
  assert.ok(!("enter_text" in withoutText.questions.operation.criteria));

  const withText = buildQuestions(inputsWith([
    candidate(1, ["click"]),
    candidate(2, ["set_value", "type_text", "click"], "textfield")
  ]));
  assert.ok(withText.questions.text_target);
  assert.deepEqual(Object.keys(withText.questions.text_target.criteria), ["e2"]);
  assert.ok("enter_text" in withText.questions.operation.criteria);
});

/**
 * The defect this merge exists for: two options describing the same outcome by
 * different mechanisms split the operation answer's probability mass and dropped
 * it under the floor, on a screen where the model was certain about the field.
 */
test("the operation menu offers exactly one way to put text into a field", () => {
  const settable = buildQuestions(inputsWith([
    candidate(1, ["click"]),
    candidate(2, ["set_value", "type_text", "click"], "textfield")
  ]));
  const typableOnly = buildQuestions(inputsWith([
    candidate(1, ["click"]),
    candidate(2, ["type_text", "click"], "textfield")
  ]));
  for (const plan of [settable, typableOnly]) {
    const operations = Object.keys(plan.questions.operation.criteria);
    assert.deepEqual(operations.filter(name => name.includes("text")), ["enter_text"]);
    assert.ok(!operations.includes("type_text"));
    assert.ok(!operations.includes("set_value"));
    // One target question for the one operation, whichever mechanism the field affords.
    assert.deepEqual(Object.keys(plan.questions.text_target.criteria), ["e2"]);
    assert.equal(plan.questions.type_target, undefined);
    assert.equal(plan.questions.set_value_target, undefined);
  }
});

test("a choice question over the option limit trips the internal guard", () => {
  const under = Array.from({ length: MAX_CHOICE_OPTIONS }, (_, index) => candidate(index, ["click"]));
  assert.ok(buildQuestions(inputsWith(under)).questions.click_target);

  const over = Array.from({ length: MAX_CHOICE_OPTIONS + 1 }, (_, index) => candidate(index, ["click"]));
  assert.throws(() => buildQuestions(inputsWith(over)), error => {
    assert.equal(error.name, "DcuError");
    assert.equal(error.code, "agent_internal");
    assert.match(error.message, /click_target offers 256 options/);
    return true;
  });
});

test("the serialized state carries no rectangle, size or pixel count", async () => {
  for (const name of await fixtureNames()) {
    const inputs = await inputsFor(name);
    const state = serializeState(inputs);
    assert.ok(Array.isArray(state) && state.length === 6, name);
    assert.doesNotMatch(state.join("\n"), GEOMETRY, name);
    assert.doesNotMatch(JSON.stringify(buildQuestions(inputs).questions), GEOMETRY, name);
  }
});

test("a full 120-row control table stays well under six thousand estimated tokens", () => {
  const elements = [];
  for (let index = 0; index < 400; index += 1) {
    elements.push({
      index,
      name: `설정 항목 ${index}`,
      controlType: "Button",
      automationId: `settingsActionButton${index}`,
      className: "Synthetic",
      enabled: true,
      offscreen: false,
      focused: false,
      bounds: { x: (index % 20) * 90, y: Math.floor(index / 20) * 40, width: 86, height: 36 },
      patterns: ["invoke"]
    });
  }
  const filtered = buildCandidates({
    window: { width: 1920, height: 1032, app: "synthetic.exe" },
    accessibility: { elements }
  });
  assert.equal(filtered.candidates.length, 120);
  const state = serializeState({
    goal: "설정에서 항목 하나를 켠다",
    window: { app: "synthetic.exe", title: "설정" },
    candidates: filtered.candidates,
    disabledShortlist: filtered.disabledShortlist,
    regions: filtered.report.regions
  });
  const estimated = estimateTokens(state);
  assert.ok(estimated < 6000, `120 rows estimated at ${estimated} tokens`);
});

test("parseAnswers accepts a well-formed reply and resolves the target to a native index", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"]), candidate(3, ["click"])]));
  const decision = parseAnswers(plan, healthyAnswers());
  assert.equal(decision.ok, true);
  assert.equal(decision.status, "continue");
  assert.equal(decision.operation, "click");
  assert.equal(decision.target.elementIndex, 1);
  assert.equal(decision.target.optionId, "e1");
  // What the field affords rides along, so the executor picks its own mechanism.
  assert.deepEqual(decision.target.ops, ["click"]);
  assert.equal(Math.round(decision.margin * 100), 70);
  assert.equal(decision.confidence, 0.82);
});

test("parseAnswers rejects a choice value we never offered", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const failure = parseAnswers(plan, healthyAnswers({
    click_target: choice("e99", { e1: 0.2, e2: 0.1, e99: 0.7 }, 0.8)
  }));
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "unknown_choice");
  assert.equal(failure.question, "click_target");
  assert.equal(failure.choice, "e99");
});

test("parseAnswers rejects a reply that is missing an expected answer key", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"])]));
  const answers = healthyAnswers();
  delete answers.click_target;
  const failure = parseAnswers(plan, answers);
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "missing_answer");
  assert.equal(failure.question, "click_target");

  const withoutOperation = healthyAnswers();
  delete withoutOperation.operation;
  const second = parseAnswers(plan, withoutOperation);
  assert.equal(second.reason, "missing_answer");
  assert.equal(second.question, "operation");
});

test("parseAnswers refuses an answer below the confidence floor", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const failure = parseAnswers(plan, healthyAnswers({
    operation: choice("click", { click: 0.52, wait: 0.2, press_key: 0.18, hotkey: 0.1 }, 0.4)
  }));
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "low_confidence");
  assert.equal(failure.question, "operation");
  assert.equal(failure.confidence, 0.4);
  assert.equal(failure.gate.minConfidence, 0.55);
});

test("parseAnswers refuses an answer whose top two options are too close", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const failure = parseAnswers(plan, healthyAnswers({
    click_target: choice("e1", { e1: 0.44, e2: 0.42, e3: 0.14 }, 0.9)
  }));
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "low_margin");
  assert.equal(failure.question, "click_target");
  assert.equal(Math.round(failure.margin * 100), 2);
  assert.equal(failure.gate.minMargin, 0.15);
});

test("the gate floors are configurable, not baked in", () => {
  const candidates = [candidate(1, ["click"]), candidate(2, ["click"])];
  const strict = buildQuestions(inputsWith(candidates, { gate: { minConfidence: 0.95, minMargin: 0.9 } }));
  assert.equal(parseAnswers(strict, healthyAnswers()).reason, "low_confidence");
  const loose = buildQuestions(inputsWith(candidates, { gate: { minConfidence: 0.1, minMargin: 0.01 } }));
  assert.equal(parseAnswers(loose, healthyAnswers()).ok, true);
});

function readingOf(decision, question) {
  return decision.gate.readings.find(reading => reading.question === question);
}

test("a finished run is not reported as blocked over the confidence of an action it will never take", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const decision = parseAnswers(plan, healthyAnswers({
    progress: choice("goal_reached", { goal_reached: 0.98, work_remains: 0.02 }, 0.98),
    // The exact reading that misreported a successful Calculator run: just under the floor.
    operation: choice("click", { click: 0.5, wait: 0.3, press_key: 0.12, hotkey: 0.08 }, 0.5)
  }));
  assert.equal(decision.ok, true);
  assert.equal(decision.status, "done");
  assert.equal(decision.reason, undefined);
  assert.equal(readingOf(decision, "progress").gated, true);
  assert.equal(readingOf(decision, "obstacle").gated, false);
  assert.equal(readingOf(decision, "operation").gated, false);
  assert.equal(readingOf(decision, "click_target").gated, false);
});

test("a goal_reached below the floor keeps working, and the action gate still applies", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const answers = healthyAnswers({
    progress: choice("goal_reached", { goal_reached: 0.5, work_remains: 0.5 }, 0.5)
  });
  const decision = parseAnswers(plan, answers);
  assert.equal(decision.ok, true);
  assert.equal(decision.status, "continue");
  assert.equal(decision.operation, "click");
  assert.equal(readingOf(decision, "progress").gated, true);
  assert.equal(readingOf(decision, "operation").gated, true);
  assert.equal(readingOf(decision, "click_target").gated, true);

  const failure = parseAnswers(plan, {
    ...answers,
    operation: choice("click", { click: 0.5, wait: 0.3, press_key: 0.2 }, 0.5)
  });
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "low_confidence");
  assert.equal(failure.question, "operation");
});

test("blocked stops on its own confidence and not on the action's", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const decision = parseAnswers(plan, healthyAnswers({
    obstacle: choice("blocked", { blocked: 0.95, path_clear: 0.05 }, 0.95),
    operation: choice("click", { click: 0.5, wait: 0.3, press_key: 0.2 }, 0.5)
  }));
  assert.equal(decision.ok, true);
  assert.equal(decision.status, "blocked");
  assert.equal(readingOf(decision, "progress").gated, false);
  assert.equal(readingOf(decision, "obstacle").gated, true);
  assert.equal(readingOf(decision, "operation").gated, false);

  const unsure = parseAnswers(plan, healthyAnswers({
    obstacle: choice("blocked", { blocked: 0.5, path_clear: 0.5 }, 0.5)
  }));
  assert.equal(unsure.ok, true);
  assert.equal(unsure.status, "continue");
  assert.equal(readingOf(unsure, "obstacle").gated, true);
});

test("a low-margin target still fails while the run is still acting", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const failure = parseAnswers(plan, healthyAnswers({
    click_target: choice("e1", { e1: 0.44, e2: 0.42, e3: 0.14 }, 0.9)
  }));
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "low_margin");
  assert.equal(failure.question, "click_target");

  // The same reading no longer fails once nothing is going to be pressed.
  const decision = parseAnswers(plan, healthyAnswers({
    progress: choice("goal_reached", { goal_reached: 0.98, work_remains: 0.02 }, 0.98),
    click_target: choice("e1", { e1: 0.44, e2: 0.42, e3: 0.14 }, 0.9)
  }));
  assert.equal(decision.ok, true);
  assert.equal(decision.status, "done");
  assert.equal(decision.target.elementIndex, 1);
});

test("a target we never offered is a hard failure even once the goal is reached", () => {
  const plan = buildQuestions(inputsWith([candidate(1, ["click"]), candidate(2, ["click"])]));
  const failure = parseAnswers(plan, healthyAnswers({
    progress: choice("goal_reached", { goal_reached: 0.98, work_remains: 0.02 }, 0.98),
    click_target: choice("e99", { e1: 0.2, e2: 0.1, e99: 0.7 }, 0.8)
  }));
  assert.equal(failure.ok, false);
  assert.equal(failure.reason, "unknown_choice");
  assert.equal(failure.question, "click_target");
  assert.equal(failure.choice, "e99");
});

test("the keypad fixture asks about pressing a control and never about typing", async () => {
  const plan = buildQuestions(await inputsFor(KEYPAD, "7 더하기 8을 계산한다"));
  assert.deepEqual(Object.keys(plan.questions), [
    "progress", "obstacle", "operation", "click_target", "scroll_target", "scroll_direction",
    "press_key_choice", "hotkey_choice"
  ]);
  assert.equal(plan.questions.text_target, undefined);
  assert.ok(!("enter_text" in plan.questions.operation.criteria));
  // The label element 44 carries in app-02-foreground.json.
  assert.equal(plan.candidatesByOption.get("e44").label, "6");
  assert.ok(Object.keys(plan.questions.click_target.criteria).length <= MAX_CHOICE_OPTIONS);
});

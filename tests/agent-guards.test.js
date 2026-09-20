import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_MAX_DURATION_MS,
  DEFAULT_MAX_INPUT_TOKENS,
  DEFAULT_MAX_STEPS,
  MAX_STEPS_CEILING,
  NoProgressDetector,
  actionKey,
  candidatesHash,
  describeAction,
  exhaustedBudget,
  findDestructive,
  redactCandidates,
  resolveBudgets,
  resolveDestructiveMode,
  resolveGate
} from "../dist/agent/guards.js";
import { DEFAULT_MIN_CONFIDENCE, DEFAULT_MIN_MARGIN } from "../dist/agent/questions.js";
import { TextResolver, goalQuotes, needsPaste, planAction, regionPoint } from "../dist/agent/executor.js";

function target(label, role = "button", elementIndex = 3, ops = ["click"]) {
  return { optionId: `e${elementIndex}`, elementIndex, role, label, place: "middle-center", ops };
}

function decision(overrides = {}) {
  return {
    ok: true,
    status: "continue",
    operation: "click",
    confidence: 0.99,
    margin: 0.9,
    gate: { minConfidence: DEFAULT_MIN_CONFIDENCE, minMargin: DEFAULT_MIN_MARGIN, readings: [] },
    answers: {},
    ...overrides
  };
}

test("the destructive guard reads the chosen action's label in both languages", () => {
  const english = [
    "Delete account", "Remove device", "Erase all content", "Format disk", "Uninstall app",
    "Reset settings", "Factory reset", "Shut down", "Shutdown", "Restart now", "Sign out",
    "Log out", "Discard changes", "Overwrite file", "Delete permanently"
  ];
  for (const label of english) {
    const finding = findDestructive(decision({ target: target(label) }));
    assert.ok(finding, `expected "${label}" to trip the guard`);
    assert.equal(finding.matched, "label");
  }
  const korean = ["삭제", "항목 제거", "지우기", "포맷", "초기화", "영구 삭제", "덮어쓰기", "로그아웃", "휴지통 비우기", "복구할 수 없습니다"];
  for (const label of korean) {
    const finding = findDestructive(decision({ target: target(label) }));
    assert.ok(finding, `expected "${label}" to trip the guard`);
    assert.equal(finding.matched, "label");
  }
});

test("the destructive guard ignores letter case and reports the exact action", () => {
  const upper = findDestructive(decision({ target: target("DELETE ACCOUNT") }));
  assert.ok(upper);
  assert.equal(upper.matched, "label");
  assert.equal(upper.action, 'press the button "DELETE ACCOUNT" — middle-center');

  const mixed = findDestructive(decision({ target: target("Permanently Discard", "menuitem", 9) }));
  assert.ok(mixed);
  assert.equal(mixed.action, 'press the menuitem "Permanently Discard" — middle-center');
});

test("the destructive guard covers both key cases", () => {
  const pressed = findDestructive(decision({ operation: "press_key", key: "delete" }));
  assert.ok(pressed);
  assert.equal(pressed.matched, "key");
  assert.equal(pressed.action, "send the delete key");
  assert.ok(findDestructive(decision({ operation: "press_key", key: "DELETE" })));

  for (const chord of ["alt+f4", "ctrl+w", "ALT+F4", "Ctrl+W"]) {
    const finding = findDestructive(decision({ operation: "hotkey", chord }));
    assert.ok(finding, `expected ${chord} to trip the guard`);
    assert.equal(finding.matched, "chord");
  }
  assert.equal(findDestructive(decision({ operation: "press_key", key: "enter" })), undefined);
  assert.equal(findDestructive(decision({ operation: "hotkey", chord: "ctrl+s" })), undefined);
});

test("the destructive guard judges the chosen action and not the candidate list", () => {
  // A Delete button on screen is not a reason to stop; choosing it is.
  assert.equal(findDestructive(decision({ target: target("Save") })), undefined);
  // Scrolling names no control, so a region called "Deleted items" is not an action.
  const scrolled = decision({
    operation: "scroll",
    region: { regionId: "r4", label: "Deleted items", place: "in the left navigation list" },
    direction: "down"
  });
  assert.equal(findDestructive(scrolled), undefined);
  assert.equal(describeAction(scrolled), "scroll Deleted items down");
});

test("confirm degrades to stop with no TTY and never to allow", () => {
  const headless = resolveDestructiveMode("confirm", false);
  assert.equal(headless.effective, "stop");
  assert.equal(headless.requested, "confirm");
  assert.equal(headless.degradedFrom, "confirm");
  assert.equal(headless.degradedReason, "no_tty");

  // Even with a console attached there is no channel to ask on while the loop
  // owns the desktop, so the answer is still stop.
  const attached = resolveDestructiveMode("confirm", true);
  assert.equal(attached.effective, "stop");
  assert.equal(attached.degradedReason, "no_confirmation_channel");

  assert.equal(resolveDestructiveMode("stop", true).effective, "stop");
  assert.equal(resolveDestructiveMode("allow", false).effective, "allow");
  assert.equal(resolveDestructiveMode(undefined, false).effective, "stop");
});

test("the second redaction pass blanks a value the native side leaves alone", () => {
  const candidates = [
    { optionId: "e1", elementIndex: 1, role: "textfield", label: "Password", value: "hunter2", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e2", elementIndex: 2, role: "textfield", label: "비밀번호", value: "abc", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e3", elementIndex: 3, role: "textfield", label: "인증번호", value: "abc", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e4", elementIndex: 4, role: "textfield", label: "Card number", value: "abcd", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e5", elementIndex: 5, role: "textfield", label: "Confirmation", value: "123456", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e6", elementIndex: 6, role: "textfield", label: "Name", value: "Kim", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e7", elementIndex: 7, role: "textfield", label: "Quantity", value: "12345", place: "p", regionId: "r_main", ops: [] }
  ];
  const { candidates: redacted, redacted: count } = redactCandidates(candidates);
  assert.equal(count, 5);
  for (const optionId of ["e1", "e2", "e3", "e4", "e5"]) {
    const found = redacted.find(candidate => candidate.optionId === optionId);
    assert.ok(!("value" in found), `${optionId} should have lost its value`);
  }
  assert.equal(redacted.find(candidate => candidate.optionId === "e6").value, "Kim");
  assert.equal(redacted.find(candidate => candidate.optionId === "e7").value, "12345");
  // The caller's own list is left intact: the executor still needs it.
  assert.equal(candidates[0].value, "hunter2");
});

test("--no-values strips every value and leaves every label", () => {
  const candidates = [
    { optionId: "e1", elementIndex: 1, role: "textfield", label: "Name", value: "Kim", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e2", elementIndex: 2, role: "textfield", label: "City", value: "Seoul", place: "p", regionId: "r_main", ops: [] },
    { optionId: "e3", elementIndex: 3, role: "button", label: "Save", place: "p", regionId: "r_main", ops: ["click"] }
  ];
  const { candidates: stripped, redacted } = redactCandidates(candidates, { noValues: true });
  assert.equal(redacted, 2);
  assert.ok(stripped.every(candidate => candidate.value === undefined));
  assert.deepEqual(stripped.map(candidate => candidate.label), ["Name", "City", "Save"]);
});

test("the no-progress detector needs three identical screens and a repeated action", () => {
  const detector = new NoProgressDetector();
  assert.equal(detector.record("h1", "click|button|7"), false);
  assert.equal(detector.record("h1", "click|button|7"), false);
  assert.equal(detector.record("h1", "click|button|7"), true);

  const moving = new NoProgressDetector();
  assert.equal(moving.record("h1", "click|button|7"), false);
  assert.equal(moving.record("h2", "click|button|7"), false);
  assert.equal(moving.record("h2", "click|button|7"), false);

  const varied = new NoProgressDetector();
  assert.equal(varied.record("h1", "click|button|7"), false);
  assert.equal(varied.record("h1", "click|button|8"), false);
  assert.equal(varied.record("h1", "click|button|7"), false);
});

test("the candidate hash ignores order and follows label, value and state", () => {
  const first = [
    { role: "button", label: "7" },
    { role: "button", label: "8" }
  ];
  const reordered = [
    { role: "button", label: "8" },
    { role: "button", label: "7" }
  ];
  assert.equal(candidatesHash(first), candidatesHash(reordered));
  assert.notEqual(candidatesHash(first), candidatesHash([{ role: "button", label: "7" }]));
  assert.notEqual(
    candidatesHash([{ role: "textfield", label: "Name", value: "a" }]),
    candidatesHash([{ role: "textfield", label: "Name", value: "b" }])
  );
  assert.notEqual(
    candidatesHash([{ role: "toggle", label: "Wi-Fi", stateWord: "currently on" }]),
    candidatesHash([{ role: "toggle", label: "Wi-Fi" }])
  );
});

test("the action key names what was acted on and never an option id", () => {
  const key = actionKey(decision({ target: target("Save", "button", 12) }));
  assert.equal(key, "click|button|Save");
  assert.ok(!key.includes("e12"));
});

test("budgets keep their documented defaults and their ceiling", () => {
  const defaults = resolveBudgets({});
  assert.equal(defaults.maxSteps, DEFAULT_MAX_STEPS);
  assert.equal(defaults.maxSteps, 25);
  assert.equal(defaults.maxDecisions, 35);
  assert.equal(defaults.maxDurationMs, DEFAULT_MAX_DURATION_MS);
  assert.equal(defaults.maxInputTokens, DEFAULT_MAX_INPUT_TOKENS);

  const capped = resolveBudgets({ maxSteps: 500 });
  assert.equal(capped.maxSteps, MAX_STEPS_CEILING);
  assert.equal(capped.maxDecisions, 110);
  assert.equal(resolveBudgets({ maxSteps: 0 }).maxSteps, 1);

  const budgets = resolveBudgets({ maxSteps: 3 });
  const idle = { steps: 0, decisions: 0, elapsedMs: 0, inputTokens: 0 };
  assert.equal(exhaustedBudget(budgets, idle), undefined);
  assert.equal(exhaustedBudget(budgets, { ...idle, steps: 3 }), "max_steps");
  assert.equal(exhaustedBudget(budgets, { ...idle, decisions: 13 }), "max_decisions");
  assert.equal(exhaustedBudget(budgets, { ...idle, elapsedMs: 180_000 }), "max_duration");
  assert.equal(exhaustedBudget(budgets, { ...idle, inputTokens: 400_000 }), "max_input_tokens");
});

test("the confidence gate keeps the untuned defaults and stays configurable per run", () => {
  assert.deepEqual(resolveGate(), { minConfidence: DEFAULT_MIN_CONFIDENCE, minMargin: DEFAULT_MIN_MARGIN });
  assert.deepEqual(resolveGate({ minConfidence: 0.9, minMargin: 0.4 }), { minConfidence: 0.9, minMargin: 0.4 });
  assert.deepEqual(resolveGate({ minConfidence: 0.9 }), { minConfidence: 0.9, minMargin: DEFAULT_MIN_MARGIN });
});

test("free text comes from the goal's quoted runs, in order, one per typing step", () => {
  assert.deepEqual(goalQuotes('type "first" then \'second\' then “third” then 「네번째」'), [
    "first", "second", "third", "네번째"
  ]);
  const resolver = new TextResolver('search for "cats" then "dogs"', { Name: "unused" });
  assert.deepEqual(resolver.resolve("Search box"), { text: "cats", source: "goal_quote" });
  assert.deepEqual(resolver.resolve("Search box"), { text: "dogs", source: "goal_quote" });
  assert.equal(resolver.resolve("Search box"), undefined);
});

test("a preset resolves against the normalized field label, and an unknown field stops the run", () => {
  const resolver = new TextResolver("이름을 입력하세요", { "이름": "홍길동", "Search": "kittens" });
  assert.deepEqual(resolver.resolve("이름"), { text: "홍길동", source: "preset", key: "이름" });
  assert.deepEqual(resolver.resolve("Search..."), { text: "kittens", source: "preset", key: "Search" });
  assert.deepEqual(resolver.resolve("Search box"), { text: "kittens", source: "preset", key: "Search" });
  assert.equal(resolver.resolve("Telephone"), undefined);
});

test("long or non-ASCII text goes through the clipboard rather than synthesized keys", () => {
  assert.equal(needsPaste("hello"), false);
  assert.equal(needsPaste("x".repeat(120)), false);
  assert.equal(needsPaste("x".repeat(121)), true);
  assert.equal(needsPaste("안녕하세요"), true);
  assert.equal(needsPaste("line\nbreak"), true);
});

function fieldObservation() {
  return {
    window: { width: 400, height: 300 },
    accessibility: {
      elements: [
        { index: 0, name: "Name", controlType: "Edit", automationId: "", className: "", enabled: true, offscreen: false, focused: false, bounds: { x: 10, y: 40, width: 120, height: 24 }, patterns: ["value"] }
      ]
    }
  };
}

test("a field with no value pattern is focused and typed into, and only when it lacks the focus", () => {
  const observation = fieldObservation();
  observation.accessibility.elements[0].patterns = [];
  const context = {
    sessionId: "s",
    windowId: "w",
    observationId: "obs-1",
    observation,
    text: { text: "hello", source: "goal_quote" }
  };
  const decisionToType = decision({
    operation: "enter_text",
    target: target("Name", "textfield", 0, ["type_text", "click"])
  });
  const unfocused = planAction(decisionToType, context);
  assert.equal(unfocused.mechanism, "type_text");
  assert.deepEqual(unfocused.calls.map(call => call.method), ["click", "type-text"]);
  assert.equal(unfocused.calls[0].params.observationId, "obs-1");
  assert.equal(unfocused.calls[0].params.observe, undefined);
  assert.equal(unfocused.calls[1].params.observe, "text");

  observation.accessibility.elements[0].focused = true;
  const focused = planAction(decisionToType, context);
  assert.deepEqual(focused.calls.map(call => call.method), ["type-text"]);
});

/** The model answers `enter_text` and nothing else; which mechanism carries it is ours. */
test("a field that carries the value pattern has its contents replaced outright", () => {
  const observation = fieldObservation();
  const context = {
    sessionId: "s",
    windowId: "w",
    observationId: "obs-1",
    observation,
    text: { text: "Kim", source: "preset" }
  };
  const plan = planAction(
    decision({ operation: "enter_text", target: target("Name", "textfield", 0, ["set_value", "type_text", "click"]) }),
    context
  );
  assert.equal(plan.mechanism, "set_value");
  assert.deepEqual(plan.calls.map(call => call.method), ["set-value"]);
  assert.equal(plan.calls[0].params.value, "Kim");
  assert.equal(plan.calls[0].params.observe, "text");
  // The fallback that used to insert at the caret is gone: nothing follows the refusal.
  assert.equal(plan.fallback, undefined);
});

test("long or non-ASCII text still goes through the clipboard on the typing path", () => {
  const observation = fieldObservation();
  observation.accessibility.elements[0].patterns = [];
  observation.accessibility.elements[0].focused = true;
  const field = target("Name", "textfield", 0, ["type_text", "click"]);
  const planFor = text => planAction(
    decision({ operation: "enter_text", target: field }),
    { sessionId: "s", windowId: "w", observationId: "obs-1", observation, text: { text, source: "goal_quote" } }
  );
  assert.equal(planFor("x".repeat(120)).calls[0].method, "type-text");
  assert.equal(planFor("x".repeat(121)).calls[0].method, "paste-text");
  assert.equal(planFor("안녕하세요").calls[0].method, "paste-text");
});

test("scroll aims at the region's centre", () => {
  const observation = {
    window: { width: 400, height: 300 },
    accessibility: {
      elements: [
        { index: 0, name: "Name", controlType: "Edit", automationId: "", className: "", enabled: true, offscreen: false, focused: false, bounds: { x: 10, y: 40, width: 120, height: 24 }, patterns: ["value"] },
        { index: 1, name: "List", controlType: "List", automationId: "", className: "", enabled: true, offscreen: false, focused: false, bounds: { x: 0, y: 100, width: 200, height: 160 }, patterns: [] }
      ]
    }
  };
  const context = { sessionId: "s", windowId: "w", observationId: "obs-1", observation, text: { text: "Kim", source: "preset" } };

  assert.deepEqual(regionPoint({ regionId: "r1", label: "List", place: "p", elementIndex: 1 }, observation), { x: 100, y: 180 });
  assert.deepEqual(regionPoint({ regionId: "r_main", label: "main window area", place: "p" }, observation), { x: 200, y: 150 });

  const scrolled = planAction(
    decision({ operation: "scroll", region: { regionId: "r1", label: "List", place: "p", elementIndex: 1 }, direction: "down" }),
    context
  );
  assert.equal(scrolled.calls[0].method, "scroll");
  assert.equal(scrolled.calls[0].params.amount, 3);
  assert.equal(scrolled.calls[0].params.x, 100);
  assert.equal(scrolled.calls[0].params.y, 180);
  assert.equal(scrolled.calls[0].params.observe, "text");
});

import test from "node:test";
import assert from "node:assert/strict";
import { buildCandidates } from "../dist/agent/filter.js";
import { goalTokens } from "../dist/agent/explain.js";

/**
 * The live failure was a Naver Mail window of 371 elements overflowing the cap of
 * 120, so the shape that matters is "more elements than the cap, and the one the
 * goal names is not the most salient on its own". A synthetic window reproduces
 * that shape without putting a recording of this user's mailbox in the repository.
 */
function overflowObservation(targetName) {
  const elements = [];
  for (let index = 0; index < 200; index += 1) {
    elements.push({
      index,
      // Filler that shares no two-character run with any goal used below, so the
      // boost it never earns cannot be mistaken for one.
      name: `row ${index}`,
      controlType: "Button",
      automationId: "",
      className: "Synthetic",
      enabled: true,
      offscreen: false,
      focused: false,
      bounds: { x: 200 + (index % 20) * 60, y: Math.floor(index / 20) * 24, width: 56, height: 20 },
      patterns: ["invoke"]
    });
  }
  // A link scores below a button, so only the goal boost can carry it over the cap.
  elements.push({
    index: 200,
    name: targetName,
    controlType: "Hyperlink",
    automationId: "",
    className: "Synthetic",
    enabled: true,
    offscreen: false,
    focused: false,
    bounds: { x: 20, y: 400, width: 140, height: 24 },
    patterns: ["invoke"]
  });
  return { window: { width: 1920, height: 1032, app: "chrome.exe" }, accessibility: { elements } };
}

function survives(observation, tokens, label) {
  const result = buildCandidates(observation, { goalTokens: tokens });
  assert.equal(result.report.overflow, true, "the window must overflow the cap for this to mean anything");
  return result.candidates.some(candidate => candidate.label === label);
}

test("the goal the run actually failed on keeps the link it names", () => {
  const observation = overflowObservation("스팸메일함");
  const goal = "왼쪽 메뉴에서 스팸메일함을 연다";

  // What the old whitespace split produced: the word carries the particle, so it
  // never equalled the label and the element the user named was thrown away.
  const whitespaceSplit = goal.toLowerCase().split(/\s+/u);
  assert.ok(!whitespaceSplit.includes("스팸메일함"));
  assert.ok(goalTokens(goal).includes("스팸메일함"));

  assert.ok(survives(observation, goalTokens(goal), "스팸메일함"));
  // The boost is what saves it: an unrelated goal still loses the link.
  assert.ok(!survives(observation, goalTokens("새 창을 하나 띄운다"), "스팸메일함"));
  assert.ok(!survives(observation, [], "스팸메일함"));
});

test("a goal word keeps both its stem and the particle it carries", () => {
  assert.deepEqual(goalTokens("스팸메일함을"), ["스팸메일함을", "스팸메일함"]);
  const stems = [
    ["메뉴에서", "메뉴"],
    ["폴더를", "폴더"],
    ["계정이", "계정"],
    ["보관함까지", "보관함"],
    ["친구에게", "친구"],
    ["받은메일함에서", "받은메일함"],
    ["제목도", "제목"],
    ["휴지통부터", "휴지통"]
  ];
  for (const [word, stem] of stems) {
    assert.deepEqual(goalTokens(word), [word, stem], word);
  }
});

test("the longest particle is stripped first", () => {
  // "에서" before "서", "으로" before "로", "라도" before "도", "이나" before "나".
  assert.deepEqual(goalTokens("메뉴에서"), ["메뉴에서", "메뉴"]);
  assert.deepEqual(goalTokens("설정으로"), ["설정으로", "설정"]);
  assert.deepEqual(goalTokens("복사라도"), ["복사라도", "복사"]);
  assert.deepEqual(goalTokens("메일이나"), ["메일이나", "메일"]);
});

test("a stem shorter than two characters is not worth keeping", () => {
  // "함을" would leave "함", which matches every label that contains the syllable.
  assert.deepEqual(goalTokens("함을"), ["함을"]);
  assert.deepEqual(goalTokens("이나"), ["이나"]);
});

test("a one-character goal token boosts nothing", () => {
  const observation = overflowObservation("스팸메일함");
  const ids = tokens => buildCandidates(observation, { goalTokens: tokens }).candidates.map(c => c.optionId);
  // "함" sits inside the link label, "row" inside every button: neither may decide the cap.
  assert.deepEqual(ids(["함"]), ids([]));
  assert.deepEqual(goalTokens("함 을 가 7"), []);
});

test("containment still matches an English label that says more than the goal", () => {
  const observation = overflowObservation("Bluetooth settings");
  assert.ok(survives(observation, goalTokens("bluetooth"), "Bluetooth settings"));
  assert.ok(survives(observation, goalTokens("Open Bluetooth"), "Bluetooth settings"));
  assert.ok(!survives(observation, goalTokens("open the printer queue"), "Bluetooth settings"));
});

test("an empty goal yields no tokens and throws nothing", () => {
  assert.deepEqual(goalTokens(undefined), []);
  assert.deepEqual(goalTokens(""), []);
  assert.deepEqual(goalTokens("   \t\n  "), []);
  assert.deepEqual(goalTokens("!!! ??? --- ..."), []);
  const observation = overflowObservation("스팸메일함");
  assert.doesNotThrow(() => buildCandidates(observation, { goalTokens: goalTokens("   ") }));
});

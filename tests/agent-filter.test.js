import test from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { buildCandidates } from "../dist/agent/filter.js";
import { readObservation, serializeCandidate } from "../dist/agent/explain.js";
import { DROP_REASONS } from "../dist/agent/types.js";

const FIXTURE_DIRECTORY = join(process.cwd(), "tests", "fixtures", "uia");
/** Recorded observations are named `app-<n>-<pass>.json`; nothing else here is one. */
const FIXTURE_NAME = /^app-\d+-(asfound|foreground)\.json$/;

async function fixtureNames() {
  const entries = await readdir(FIXTURE_DIRECTORY);
  return entries.filter(name => FIXTURE_NAME.test(name)).sort();
}

async function loadFixture(name) {
  return readObservation(JSON.parse(await readFile(join(FIXTURE_DIRECTORY, name), "utf8")));
}

function droppedTotal(report) {
  return Object.values(report.dropped).reduce((total, count) => total + count, 0);
}

function stable(result) {
  return {
    candidates: result.candidates,
    disabledShortlist: result.disabledShortlist,
    report: { ...result.report, elapsedMs: 0 }
  };
}

function syntheticObservation(count) {
  const elements = [];
  for (let index = 0; index < count; index += 1) {
    const column = index % 40;
    const row = Math.floor(index / 40) % 50;
    elements.push({
      index,
      name: `Item ${index % 400}`,
      controlType: index % 7 === 0 ? "Text" : "Button",
      automationId: index % 5 === 0 ? `itemActionButton${index % 400}` : "",
      className: "Synthetic",
      enabled: index % 23 !== 0,
      offscreen: false,
      focused: index === 11,
      bounds: { x: column * 48, y: row * 20, width: 44 + (index % 3), height: 18 + (index % 3) },
      patterns: index % 3 === 0 ? ["invoke"] : ["invoke", "value"]
    });
  }
  return { window: { width: 1920, height: 1032, app: "synthetic.exe" }, accessibility: { elements } };
}

test("every recorded observation produces candidates without throwing", async () => {
  const names = await fixtureNames();
  assert.ok(names.length >= 20, `expected the recorded observations, found ${names.length}`);
  for (const name of names) {
    const result = buildCandidates(await loadFixture(name));
    assert.ok(Array.isArray(result.candidates), name);
    assert.ok(result.report.elapsedMs >= 0, name);
    for (const candidate of result.candidates) {
      assert.ok(candidate.label.length > 0 && candidate.label.length <= 80, `${name} ${candidate.optionId}`);
      assert.ok(candidate.place.length > 0, `${name} ${candidate.optionId}`);
      assert.ok(candidate.regionId.length > 0, `${name} ${candidate.optionId}`);
      if (candidate.value !== undefined) assert.ok(candidate.value.length <= 60, `${name} ${candidate.optionId}`);
    }
  }
});

test("kept plus every drop count equals the raw element count", async () => {
  for (const name of await fixtureNames()) {
    const { report } = buildCandidates(await loadFixture(name));
    assert.equal(report.kept + droppedTotal(report), report.rawCount, name);
  }
});

test("the candidate list never exceeds the cap", async () => {
  for (const name of await fixtureNames()) {
    const observation = await loadFixture(name);
    assert.ok(buildCandidates(observation).candidates.length <= 120, name);
    const capped = buildCandidates(observation, { maxCandidates: 12 });
    assert.ok(capped.candidates.length <= 12, name);
    assert.equal(capped.report.kept + droppedTotal(capped.report), capped.report.rawCount, name);
  }
});

test("filtering the same observation twice gives the same answer", async () => {
  for (const name of await fixtureNames()) {
    const observation = await loadFixture(name);
    const first = buildCandidates(observation, { goalTokens: ["search", "검색"] });
    const second = buildCandidates(observation, { goalTokens: ["search", "검색"] });
    assert.deepEqual(stable(second), stable(first), name);
  }
});

test("each drop reason is exercised", async () => {
  const totals = Object.fromEntries(DROP_REASONS.map(reason => [reason, 0]));
  const count = report => {
    for (const reason of DROP_REASONS) totals[reason] += report.dropped[reason];
  };
  for (const name of await fixtureNames()) {
    const observation = await loadFixture(name);
    // A small cap is the only way a recording of this size overflows.
    count(buildCandidates(observation).report);
    count(buildCandidates(observation, { maxCandidates: 5 }).report);
  }
  // Every control in the recordings carries a name, an automation id or a value,
  // so only a synthetic window reaches the naming stage empty-handed.
  const nameless = {
    window: { width: 400, height: 300 },
    accessibility: {
      elements: [{
        index: 0,
        name: "",
        controlType: "Button",
        automationId: "",
        className: "",
        enabled: true,
        offscreen: false,
        focused: false,
        bounds: { x: 10, y: 10, width: 40, height: 20 },
        patterns: ["invoke"]
      }]
    }
  };
  const namelessReport = buildCandidates(nameless).report;
  assert.equal(namelessReport.dropped.unnamed, 1);
  assert.equal(namelessReport.kept, 0);
  count(namelessReport);
  // Whether a recording happens to contain a control scrolled fully out of the
  // window is an accident of the desktop at capture time, so a synthetic one
  // carries this reason instead.
  const scrolledOut = {
    window: { width: 400, height: 300 },
    accessibility: {
      elements: [{
        index: 0,
        name: "저장",
        controlType: "Button",
        automationId: "SaveButton",
        className: "",
        enabled: true,
        offscreen: false,
        focused: false,
        bounds: { x: 420, y: 10, width: 40, height: 20 },
        patterns: ["invoke"]
      }]
    }
  };
  const scrolledOutReport = buildCandidates(scrolledOut).report;
  assert.equal(scrolledOutReport.dropped.outside_window, 1);
  assert.equal(scrolledOutReport.kept, 0);
  count(scrolledOutReport);
  const unused = DROP_REASONS.filter(reason => totals[reason] === 0);
  assert.deepEqual(unused, [], `drop reasons never exercised: ${unused.join(", ")}`);
});

test("a twenty thousand element observation stays well clear of a quadratic collapse", () => {
  const observation = syntheticObservation(20_000);
  const startedAt = performance.now();
  const result = buildCandidates(observation);
  const elapsedMs = performance.now() - startedAt;
  assert.equal(result.report.rawCount, 20_000);
  assert.equal(result.report.kept + droppedTotal(result.report), 20_000);
  assert.ok(result.report.overflow);
  assert.ok(elapsedMs < 150, `filtering took ${elapsedMs.toFixed(1)} ms`);
});

test("a single element window yields a sane empty result", async () => {
  // The one recording of a window that exposes a single element and no usable
  // accessibility tree below it: a bare container with nothing to act on.
  const observation = await loadFixture("app-13-foreground.json");
  const result = buildCandidates(observation);
  assert.equal(result.report.rawCount, 1);
  assert.equal(result.candidates.length, 0);
  assert.equal(result.report.overflow, false);
  assert.deepEqual(result.disabledShortlist, []);
  assert.equal(result.report.regions.length, 1);
  assert.equal(result.report.regions[0].regionId, "r_main");
});

test("no candidate carries a raw rectangle out of the module", async () => {
  for (const name of await fixtureNames()) {
    const result = buildCandidates(await loadFixture(name));
    const serialized = JSON.stringify([
      ...result.candidates.map(serializeCandidate),
      ...result.disabledShortlist.map(serializeCandidate)
    ]);
    assert.doesNotMatch(serialized, /rect=|\b\d+x\d+\b/, name);
    assert.doesNotMatch(JSON.stringify(result.report.regions), /rect=|\b\d+x\d+\b/, name);
  }
});

test("every option id parses back to the native element index", async () => {
  for (const name of await fixtureNames()) {
    const result = buildCandidates(await loadFixture(name));
    for (const candidate of [...result.candidates, ...result.disabledShortlist]) {
      assert.match(candidate.optionId, /^e\d+$/, name);
      assert.equal(Number(candidate.optionId.slice(1)), candidate.elementIndex, name);
      assert.ok(Number.isInteger(candidate.elementIndex) && candidate.elementIndex >= 0, name);
    }
  }
});

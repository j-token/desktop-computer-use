import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent } from "../dist/agent/agent.js";
import { createTrace, memoryTrace, nullTrace } from "../dist/agent/trace.js";

const run = promisify(execFile);

/* ------------------------------------------------------------------ fakes */

function button(index, name, x) {
  return {
    index,
    name,
    controlType: "Button",
    automationId: "",
    className: "",
    enabled: true,
    offscreen: false,
    focused: false,
    bounds: { x, y: 120, width: 40, height: 30 },
    patterns: ["invoke"]
  };
}

function observation(id, labels, title = "계산기") {
  const elements = labels.map((name, position) => button(position, name, 10 + position * 60));
  return {
    observationId: id,
    window: { id: "w1", app: "calc.exe", title, x: 0, y: 0, width: 400, height: 600 },
    accessibility: { elementCount: elements.length, elements },
    timings: { totalMs: 7 }
  };
}

function fieldObservation(id, value, patterns = ["value"]) {
  const field = {
    index: 0,
    name: "이름",
    controlType: "Edit",
    automationId: "",
    className: "",
    enabled: true,
    offscreen: false,
    focused: false,
    bounds: { x: 10, y: 120, width: 200, height: 24 },
    patterns
  };
  if (value !== undefined) field.value = value;
  const elements = [field, button(1, "저장", 10)];
  elements[1].bounds = { x: 10, y: 200, width: 60, height: 24 };
  return {
    observationId: id,
    window: { id: "w1", app: "forms.exe", title: "양식", x: 0, y: 0, width: 400, height: 600 },
    accessibility: { elementCount: elements.length, elements },
    timings: { totalMs: 7 }
  };
}

function ok(result) {
  return { id: "req", ok: true, result };
}

function nativeError(code, message = code) {
  return { id: "req", ok: false, error: { code, message } };
}

function transportFailure(requestSent, code = "timeout") {
  const error = new Error(`Native daemon ${code}`);
  error.name = "DcuTransportError";
  error.requestSent = requestSent;
  error.code = code;
  return error;
}

const WINDOW_LIST = ok({
  windows: [{ id: "w1", app: "calc.exe", title: "계산기", isForeground: true, isMinimized: false }]
});

function fakeClient(handlers) {
  const calls = [];
  return {
    calls,
    of(method) {
      return calls.filter(call => call.method === method);
    },
    async request(method, params) {
      calls.push({ method, params });
      const handler = handlers[method];
      if (!handler) throw new Error(`the test scripted no answer for ${method}`);
      const nth = calls.filter(call => call.method === method).length;
      const value = typeof handler === "function" ? handler(params, nth) : handler;
      if (value instanceof Error) throw value;
      return value;
    }
  };
}

function baseHandlers(extra = {}) {
  return {
    "session.status": ok({ active: true, sessionId: "s1", stopping: false, idleTimeoutMs: 120_000 }),
    "list-windows": WINDOW_LIST,
    "get-app-state": ok(observation("obs-1", ["7", "8", "="])),
    ...extra
  };
}

function usage(input = 100) {
  return { input_tokens: input, output_tokens: 4 };
}

function decideEnvelope(decision, tokens) {
  return {
    model: "jev-test",
    latencyMs: 11,
    usage: usage(tokens),
    estimatedStateTokens: 180,
    questionKeys: ["progress", "obstacle", "operation", "click_target"],
    decision
  };
}

function clickCandidate(candidate, tokens) {
  return decideEnvelope({
    ok: true,
    status: "continue",
    operation: "click",
    target: {
      optionId: candidate.optionId,
      elementIndex: candidate.elementIndex,
      role: candidate.role,
      label: candidate.label,
      place: candidate.place,
      ops: candidate.ops
    },
    confidence: 0.99,
    margin: 0.9,
    gate: {
      minConfidence: 0.55,
      minMargin: 0.15,
      readings: [{ question: "operation", choice: "click", confidence: 0.99, margin: 0.9 }]
    },
    answers: {
      progress: { choice: "work_remains", confidence: 0.97, probabilities: { work_remains: 0.97, goal_reached: 0.03 } },
      obstacle: { choice: "path_clear", confidence: 0.95, probabilities: { path_clear: 0.95, blocked: 0.05 } },
      operation: { choice: "click", confidence: 0.99, probabilities: { click: 0.99, wait: 0.01 } },
      click_target: { choice: candidate.optionId, confidence: 0.96, probabilities: { [candidate.optionId]: 0.96, other: 0.04 } }
    }
  }, tokens);
}

function clickOn(label, tokens) {
  return input => {
    const candidate = input.candidates.find(item => item.label === label);
    assert.ok(candidate, `the fake screen carried no candidate labelled ${label}`);
    return clickCandidate(candidate, tokens);
  };
}

function clickFirst(tokens) {
  return input => clickCandidate(input.candidates[0], tokens);
}

function typeInto(label, tokens) {
  return input => {
    const candidate = input.candidates.find(item => item.label === label);
    assert.ok(candidate, `the fake screen carried no candidate labelled ${label}`);
    return decideEnvelope({
      ok: true,
      status: "continue",
      operation: "enter_text",
      target: {
        optionId: candidate.optionId,
        elementIndex: candidate.elementIndex,
        role: candidate.role,
        label: candidate.label,
        place: candidate.place,
        ops: candidate.ops
      },
      confidence: 0.98,
      margin: 0.8,
      gate: {
        minConfidence: 0.55,
        minMargin: 0.15,
        readings: [{ question: "operation", choice: "enter_text", confidence: 0.98, margin: 0.8 }]
      },
      answers: {
        progress: { choice: "work_remains", confidence: 0.96, probabilities: { work_remains: 0.96, goal_reached: 0.04 } },
        obstacle: { choice: "path_clear", confidence: 0.94, probabilities: { path_clear: 0.94, blocked: 0.06 } },
        operation: { choice: "enter_text", confidence: 0.98, probabilities: { enter_text: 0.98, click: 0.02 } },
        text_target: { choice: candidate.optionId, confidence: 0.95, probabilities: { [candidate.optionId]: 0.95, other: 0.05 } }
      }
    }, tokens);
  };
}

function finished(tokens) {
  return () => decideEnvelope({
    ok: true,
    status: "done",
    operation: "wait",
    confidence: 0.99,
    margin: 0.95,
    gate: { minConfidence: 0.55, minMargin: 0.15, readings: [] },
    answers: {
      progress: { choice: "goal_reached", confidence: 0.99, probabilities: { goal_reached: 0.99, work_remains: 0.01 } },
      obstacle: { choice: "path_clear", confidence: 0.98, probabilities: { path_clear: 0.98, blocked: 0.02 } },
      operation: { choice: "wait", confidence: 0.99, probabilities: { wait: 0.99, click: 0.01 } }
    }
  }, tokens);
}

function gateFailed(reason, extra = {}, tokens) {
  return () => ({
    model: "jev-test",
    latencyMs: 9,
    usage: usage(tokens),
    estimatedStateTokens: 100,
    questionKeys: ["progress", "obstacle", "operation"],
    gateFailure: {
      ok: false,
      reason,
      question: "operation",
      message: `The answer for operation did not clear the gate: ${reason}`,
      gate: { minConfidence: 0.55, minMargin: 0.15 },
      answers: {},
      ...extra
    }
  });
}

function decider(scripts) {
  const seen = [];
  const fn = async input => {
    seen.push(input);
    const script = scripts[Math.min(seen.length - 1, scripts.length - 1)];
    return script(input);
  };
  fn.seen = seen;
  return fn;
}

function agentFor(client, decide, config = {}, extras = {}) {
  const sleeps = [];
  const deps = {
    client,
    decide,
    trace: extras.trace ?? nullTrace(),
    sleep: async ms => {
      sleeps.push(ms);
    },
    ...(extras.now === undefined ? {} : { now: extras.now })
  };
  const agent = new Agent(deps, { app: "calc.exe", runId: "run-test", ...config });
  return { agent, sleeps };
}

/* ------------------------------------------------------------------ tests */

test("an action carries its own next observation, so one step is one round trip", async () => {
  const client = fakeClient(baseHandlers({
    click: ok({
      delivered: true,
      verification: { state: "unverified" },
      observation: observation("obs-2", ["15", "8", "="]),
      sessionId: "s1"
    })
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), finished()]));
  const result = await agent.run("7 더하기 8을 계산하세요");

  assert.equal(result.status, "succeeded");
  assert.equal(result.reason, "goal_reached");
  assert.equal(result.steps, 1);
  assert.equal(result.decisions, 2);
  // One observation at setup and none after the click: the action brought it back.
  assert.equal(client.of("get-app-state").length, 1);
  assert.deepEqual(client.of("click")[0].params, {
    sessionId: "s1",
    windowId: "w1",
    elementIndex: 0,
    observationId: "obs-1",
    observe: "text"
  });
  assert.equal(result.lastObservationId, "obs-2");
  assert.equal(result.usage.input_tokens, 200);
  assert.match(result.history[0].outcome, /"15" appeared/);
  assert.match(result.history[0].outcome, /"7" went away/);
  assert.equal(result.history[0].targetLabel, "7");
});

test("a stale observation is never replayed: the loop observes again and decides again", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": (_params, nth) => ok(
      nth === 1 ? observation("obs-1", ["7", "8", "="]) : observation("obs-2", ["=", "8", "7"])
    ),
    click: (_params, nth) => nth === 1
      ? nativeError("stale_observation", "Window geometry changed; observe again")
      : ok({ delivered: true, observation: observation("obs-3", ["15"]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), clickOn("7"), finished()]));
  const result = await agent.run("7을 누르세요");

  const clicks = client.of("click");
  assert.equal(clicks.length, 2);
  assert.equal(clicks[0].params.elementIndex, 0);
  assert.equal(clicks[0].params.observationId, "obs-1");
  // The same control, a different index, and never the old observation's id.
  assert.equal(clicks[1].params.elementIndex, 2);
  assert.equal(clicks[1].params.observationId, "obs-2");
  assert.equal(client.of("get-app-state").length, 2);
  assert.equal(result.steps, 1, "a stale observation consumes a decision, not a step");
  assert.equal(result.decisions, 3);
  assert.equal(result.status, "succeeded");
});

test("two stale observations in a row end the run as an unstable window", async () => {
  const client = fakeClient(baseHandlers({
    click: nativeError("stale_observation", "Window geometry changed; observe again")
  }));
  const { agent } = agentFor(client, decider([clickFirst()]));
  const result = await agent.run("무언가 누르세요");
  assert.equal(result.status, "aborted");
  assert.equal(result.reason, "unstable_window");
  assert.equal(result.steps, 0);
  assert.equal(result.decisions, 2);
});

test("busy backs off and retries the identical call without consuming a step", async () => {
  const client = fakeClient(baseHandlers({
    click: (_params, nth) => nth === 1
      ? nativeError("busy", "Another operation is in progress; observe before retrying an input")
      : ok({ delivered: true, observation: observation("obs-2", ["15"]) })
  }));
  const { agent, sleeps } = agentFor(client, decider([clickOn("7"), finished()]));
  const result = await agent.run("7을 누르세요");

  assert.deepEqual(sleeps, [250]);
  assert.equal(client.of("click").length, 2);
  assert.deepEqual(client.of("click")[0].params, client.of("click")[1].params);
  assert.equal(client.of("get-app-state").length, 1);
  assert.equal(result.steps, 1);
  assert.equal(result.decisions, 2);
});

test("busy through every backoff observes again and decides again, still without a step", async () => {
  const client = fakeClient(baseHandlers({
    click: (_params, nth) => nth <= 4
      ? nativeError("busy", "Another operation is in progress")
      : ok({ delivered: true, observation: observation("obs-3", ["15"]) })
  }));
  const { agent, sleeps } = agentFor(client, decider([clickOn("7"), clickOn("8"), finished()]));
  const result = await agent.run("7을 누르세요");

  assert.deepEqual(sleeps, [250, 500, 1000]);
  assert.equal(client.of("click").length, 5);
  assert.equal(client.of("get-app-state").length, 2);
  assert.equal(result.steps, 1);
  assert.equal(result.decisions, 3);
});

test("a transport failure after the request bytes is never repeated as a mutation", async () => {
  const client = fakeClient(baseHandlers({
    click: (_params, nth) => nth === 1
      ? transportFailure(true)
      : ok({ delivered: true, observation: observation("obs-3", ["15"]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), clickOn("8"), finished()]));
  const result = await agent.run("7과 8을 누르세요");

  const clicks = client.of("click");
  assert.equal(clicks.length, 2, "the uncertain mutation must not be sent again");
  assert.notEqual(clicks[0].params.elementIndex, clicks[1].params.elementIndex);
  assert.equal(result.steps, 2, "an uncertain mutation still spends its step");
  assert.equal(result.decisions, 3);
  assert.equal(client.of("get-app-state").length, 2);
  assert.match(result.history[0].outcome, /not known whether/);
});

test("two uncertain mutations in a row end the run as an unstable transport", async () => {
  const client = fakeClient(baseHandlers({ click: () => transportFailure(true) }));
  const { agent } = agentFor(client, decider([clickOn("7"), clickOn("8")]));
  const result = await agent.run("7과 8을 누르세요");
  assert.equal(result.status, "aborted");
  assert.equal(result.reason, "transport_unstable");
  assert.equal(result.steps, 1);
  assert.equal(client.of("click").length, 2);
});

test("an element that is no longer there is re-observed once and then called a desync", async () => {
  const client = fakeClient(baseHandlers({
    click: nativeError("element_not_found", "Element index is not present in the observation")
  }));
  const { agent } = agentFor(client, decider([clickFirst()]));
  const result = await agent.run("무언가 누르세요");
  assert.equal(result.status, "aborted");
  assert.equal(result.reason, "index_desync");
  assert.equal(result.steps, 0);
  assert.equal(result.decisions, 2);
});

test("focus_denied is retried once after half a second and then stops", async () => {
  const client = fakeClient(baseHandlers({
    click: nativeError("focus_denied", "The target window could not be brought to the foreground")
  }));
  const { agent, sleeps } = agentFor(client, decider([clickFirst()]));
  const result = await agent.run("무언가 누르세요");
  assert.equal(result.status, "failed");
  assert.equal(result.reason, "focus_denied");
  assert.equal(client.of("click").length, 2);
  assert.ok(sleeps.includes(500));
});

test("a delivered action whose observation failed is followed by a standalone observation", async () => {
  const client = fakeClient(baseHandlers({
    click: ok({ delivered: true, observationError: { message: "capture busy" } }),
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, nth === 1 ? ["7", "8"] : ["15", "8"]))
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), finished()]));
  const result = await agent.run("7을 누르세요");
  assert.equal(result.status, "succeeded");
  assert.equal(result.steps, 1);
  assert.equal(client.of("get-app-state").length, 2);
  assert.equal(result.lastObservationId, "obs-2");
});

test("a delivered action with no observation at all and no fallback stops as observation_failed", async () => {
  const client = fakeClient(baseHandlers({
    click: ok({ delivered: true, observationError: { message: "capture busy" } }),
    "get-app-state": (_params, nth) => nth === 1
      ? ok(observation("obs-1", ["7", "8"]))
      : nativeError("provider_error", "UIA is unavailable")
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), finished()]));
  const result = await agent.run("7을 누르세요");
  assert.equal(result.status, "failed");
  assert.equal(result.reason, "observation_failed");
  assert.equal(result.steps, 1);
});

test("session_required stops as session_lost, because the realistic cause is the Esc key", async () => {
  const client = fakeClient(baseHandlers({
    click: nativeError("session_required", "Start a session and provide its sessionId")
  }));
  const { agent } = agentFor(client, decider([clickFirst()]));
  const result = await agent.run("무언가 누르세요");
  assert.equal(result.status, "aborted");
  assert.equal(result.reason, "session_lost");
  assert.equal(client.of("session.start").length, 0, "restarting would defeat the emergency stop");
});

test("--resume-session buys exactly one restart", async () => {
  const client = fakeClient(baseHandlers({
    "session.start": ok({ ready: true, sessionId: "s2" }),
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, ["7", "8"])),
    click: (_params, nth) => nth === 1
      ? nativeError("session_required", "Start a session and provide its sessionId")
      : ok({ delivered: true, observation: observation("obs-9", ["15"]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), clickOn("7"), finished()]), { resumeSession: true });
  const result = await agent.run("7을 누르세요");
  assert.equal(result.status, "succeeded");
  assert.equal(client.of("session.start").length, 1);
  assert.equal(result.sessionId, "s2");
  assert.equal(client.of("click")[1].params.sessionId, "s2");
});

test("--app falls back to the window title when the executable name does not match", async () => {
  const everything = {
    windows: [
      { id: "w0", app: "explorer.exe", title: "Program Manager", isForeground: false, isMinimized: false },
      { id: "w1", app: "ApplicationFrameHost.exe", title: "계산기", isForeground: false, isMinimized: false }
    ]
  };
  const client = fakeClient({
    "session.status": ok({ active: true, sessionId: "s1" }),
    // The daemon compares `app` against the executable name alone, so a packaged
    // app asked for by the name on its title bar comes back empty.
    "list-windows": params => ok(params.app === undefined ? everything : { windows: [] }),
    "get-app-state": ok(observation("obs-1", ["7", "8", "="])),
    click: ok({ delivered: true, observation: observation("obs-2", ["15"]) })
  });
  const { agent } = agentFor(client, decider([clickOn("7"), finished()]), { app: "계산기" });
  const result = await agent.run("7을 누르세요");

  assert.equal(result.status, "succeeded");
  assert.equal(result.window.id, "w1");
  assert.equal(result.window.app, "ApplicationFrameHost.exe");
  assert.equal(client.of("list-windows").length, 2);
  // The frozen id, not the app name, is what every later call carries.
  assert.equal(client.of("click")[0].params.windowId, "w1");
  assert.equal(client.of("click")[0].params.app, undefined);
});

test("the title fallback prefers an exact title and only then a containing one", async () => {
  const { windowsByTitle } = await import("../dist/agent/agent.js");
  const windows = [
    { id: "a", title: "계산기" },
    { id: "b", title: "계산기 - 도움말" },
    { id: "c", title: "메모장" }
  ];
  assert.deepEqual(windowsByTitle(windows, "계산기").map(window => window.id), ["a"]);
  assert.deepEqual(windowsByTitle(windows, "도움말").map(window => window.id), ["b"]);
  assert.deepEqual(windowsByTitle(windows, "Calculator"), []);
  assert.deepEqual(windowsByTitle(windows, "  메모장 ").map(window => window.id), ["c"]);
});

test("the no-progress detector stops at step 3", async () => {
  const client = fakeClient(baseHandlers({
    click: ok({ delivered: true, observation: observation("obs-1", ["7", "8", "="]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("7")]));
  const result = await agent.run("7을 누르세요");

  assert.equal(result.status, "aborted");
  assert.equal(result.reason, "no_progress");
  assert.equal(result.steps, 2, "the third identical action is refused rather than sent");
  assert.equal(result.decisions, 3);
  assert.equal(client.of("click").length, 2);
  assert.match(result.detail.repeatedAction, /press the button "7"/);
});

test("a window with no accessible controls stops rather than guessing", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": ok({
      observationId: "obs-1",
      window: { id: "w1", app: "game.exe", title: "Game", x: 0, y: 0, width: 800, height: 600 },
      accessibility: { elementCount: 1, elements: [] }
    })
  }));
  const { agent } = agentFor(client, decider([clickFirst()]));
  const result = await agent.run("게임을 시작하세요");
  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "no_accessible_ui");
  assert.equal(result.decisions, 0);
});

test("each budget ends the run with its own reason", async () => {
  const changing = () => baseHandlers({
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, [`a${nth}`, `b${nth}`])),
    click: (_params, nth) => ok({ delivered: true, observation: observation(`act-${nth}`, [`a${nth + 9}`, `b${nth + 9}`]) })
  });

  const steps = agentFor(fakeClient(changing()), decider([clickFirst()]), { maxSteps: 2 });
  const byStep = await steps.agent.run("계속 누르세요");
  assert.equal(byStep.status, "budget_exhausted");
  assert.equal(byStep.reason, "max_steps");
  assert.equal(byStep.steps, 2);
  assert.equal(byStep.budgets.maxSteps, 2);

  // A daemon that stays busy spends decisions and never a step. The screen still
  // changes, so this is the budget stopping the run and not the progress detector.
  const decisions = agentFor(
    fakeClient({ ...changing(), click: nativeError("busy", "Another operation is in progress") }),
    decider([clickFirst()]),
    { maxSteps: 1 }
  );
  const byDecision = await decisions.agent.run("계속 누르세요");
  assert.equal(byDecision.status, "budget_exhausted");
  assert.equal(byDecision.reason, "max_decisions");
  assert.equal(byDecision.decisions, 11);
  assert.equal(byDecision.steps, 0);

  let clock = 0;
  const duration = agentFor(
    fakeClient(changing()),
    decider([clickFirst()]),
    { maxSteps: 50, maxDurationMs: 10_000 },
    { now: () => (clock += 1000) }
  );
  const byDuration = await duration.agent.run("계속 누르세요");
  assert.equal(byDuration.status, "budget_exhausted");
  assert.equal(byDuration.reason, "max_duration");

  const tokens = agentFor(
    fakeClient(changing()),
    decider([clickFirst(100)]),
    { maxSteps: 50, maxInputTokens: 150 }
  );
  const byToken = await tokens.agent.run("계속 누르세요");
  assert.equal(byToken.status, "budget_exhausted");
  assert.equal(byToken.reason, "max_input_tokens");
  assert.equal(byToken.steps, 2);
  assert.equal(byToken.usage.input_tokens, 200);
});

test("the destructive guard stops the run and reports the action it would have taken", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": ok(observation("obs-1", ["Delete account", "Cancel"])),
    click: ok({ delivered: true, observation: observation("obs-2", ["Cancel"]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("Delete account")]));
  const result = await agent.run("계정을 정리하세요");
  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "destructive_action");
  assert.equal(client.of("click").length, 0, "nothing may reach the desktop once the guard stops");
  assert.match(result.detail.action, /press the button "Delete account"/);
  assert.equal(result.detail.rerunWith, "--on-destructive allow");

  const allowed = agentFor(client, decider([clickOn("Delete account"), finished()]), { onDestructive: "allow" });
  const second = await allowed.agent.run("계정을 정리하세요");
  assert.equal(second.status, "succeeded");
  assert.equal(client.of("click").length, 1);
});

test("a typing decision with no text to type stops instead of inventing one", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": ok(fieldObservation("obs-1")),
    click: ok({ delivered: true }),
    "set-value": ok({ delivered: true, observation: fieldObservation("obs-2", "x") }),
    "type-text": ok({ delivered: true, observation: fieldObservation("obs-2", "x") })
  }));
  const { agent } = agentFor(client, decider([typeInto("이름")]));
  const result = await agent.run("양식을 채우세요");
  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "text_unavailable");
  assert.equal(result.detail.field, "이름");
  assert.equal(client.of("set-value").length, 0);
  assert.equal(client.of("type-text").length, 0);
  assert.equal(client.of("click").length, 0);
});

test("the executor replaces a field that affords it and types into one that does not", async () => {
  const settable = fakeClient(baseHandlers({
    "get-app-state": ok(fieldObservation("obs-1")),
    "set-value": ok({ delivered: true, observation: fieldObservation("obs-2", "홍길동") })
  }));
  const replaced = agentFor(settable, decider([typeInto("이름"), finished()]), { texts: { "이름": "홍길동" } });
  const first = await replaced.agent.run("이름 칸을 채우세요");
  assert.equal(first.status, "succeeded");
  assert.equal(settable.of("set-value").length, 1);
  assert.equal(settable.of("set-value")[0].params.value, "홍길동");
  assert.equal(settable.of("click").length, 0, "a field written outright is never clicked first");

  // The same decision against a field with no value pattern: focus it and type.
  const typable = fakeClient(baseHandlers({
    "get-app-state": ok(fieldObservation("obs-1", undefined, [])),
    click: ok({ delivered: true }),
    "paste-text": ok({ delivered: true, observation: fieldObservation("obs-2", "홍길동", []) })
  }));
  const typed = agentFor(typable, decider([typeInto("이름"), finished()]), { texts: { "이름": "홍길동" } });
  const second = await typed.agent.run("이름 칸을 채우세요");
  assert.equal(second.status, "succeeded");
  assert.equal(typable.of("set-value").length, 0);
  assert.deepEqual(typable.calls.filter(call => call.method === "click" || call.method === "paste-text")
    .map(call => call.method), ["click", "paste-text"]);
});

/**
 * The fallback this replaces typed into the field after `set-value` was refused,
 * which inserts at the caret and leaves a half-merged string in a real document.
 */
test("a field that cannot be replaced ends the run and is never typed into instead", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": ok(fieldObservation("obs-1", "기존 내용")),
    click: ok({ delivered: true }),
    "type-text": ok({ delivered: true, observation: fieldObservation("obs-2", "x") }),
    "paste-text": ok({ delivered: true, observation: fieldObservation("obs-2", "x") }),
    "set-value": nativeError("pattern_unavailable", "The element exposes no ValuePattern")
  }));
  const { agent } = agentFor(client, decider([typeInto("이름")]), { texts: { "이름": "홍길동" } });
  const result = await agent.run("이름 칸을 채우세요");

  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "field_not_replaceable");
  assert.equal(result.detail.field, "이름");
  assert.equal(result.detail.code, "pattern_unavailable");
  assert.match(result.detail.action, /put text into the textfield "이름"/);
  assert.equal(client.of("set-value").length, 1);
  assert.equal(client.of("type-text").length, 0, "typing here would insert at the caret");
  assert.equal(client.of("paste-text").length, 0);
  assert.equal(result.steps, 0, "nothing was written, so nothing was spent");
});

/**
 * The screen is often still rendering when the answer is read, so the first
 * uncertain answer buys a second look rather than ending the run.
 */
test("an uncertain answer is re-observed once and decided again before the step is taken", async () => {
  const records = [];
  const client = fakeClient(baseHandlers({
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, ["7", "8", "="])),
    click: ok({ delivered: true, observation: observation("obs-act", ["15", "8", "="]) })
  }));
  const { agent, sleeps } = agentFor(
    client,
    decider([gateFailed("low_confidence", { confidence: 0.4, margin: 0.2 }), clickOn("7"), finished()]),
    {},
    { trace: memoryTrace(records) }
  );
  const result = await agent.run("7을 누르세요");

  assert.equal(result.status, "succeeded");
  assert.equal(result.steps, 1, "the second look costs a decision, not a step");
  assert.equal(result.decisions, 3);
  assert.equal(client.of("get-app-state").length, 2, "the retry decides against a fresh observation");
  // Never the stale candidate list: the index acted on comes from the new observation.
  assert.equal(client.of("click")[0].params.observationId, "obs-2");
  assert.deepEqual(sleeps, [600], "the settle went through the injected sleep");

  // Both decisions are in the trace, told apart, and still sum to the exact cost.
  const decides = records.filter(record => record.type === "decide");
  assert.deepEqual(decides.map(record => record.attempt), [1, 2, 1]);
  assert.deepEqual(decides.map(record => record.gate.passed), [false, true, true]);
  assert.equal(
    decides.reduce((total, record) => total + record.usage.input_tokens, 0),
    result.usage.input_tokens
  );
  assert.ok(records.some(record => record.type === "note" && record.event === "gate_retry"));
});

test("a second uncertain answer ends the run, and the detail says it was retried", async () => {
  const client = fakeClient(baseHandlers({ click: ok({ delivered: true }) }));
  const { agent, sleeps } = agentFor(
    client,
    decider([gateFailed("low_confidence", { confidence: 0.4, margin: 0.2 })])
  );
  const result = await agent.run("무언가 하세요");

  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "low_confidence");
  assert.equal(result.detail.confidence, 0.4);
  assert.equal(result.detail.retried, true);
  assert.equal(result.decisions, 2, "one retry, and no more");
  assert.equal(client.of("get-app-state").length, 2);
  assert.deepEqual(sleeps, [600]);
  assert.equal(client.of("click").length, 0);
});

test("a thin margin buys the same second look as a low confidence", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, ["7", "8", "="])),
    click: ok({ delivered: true, observation: observation("obs-act", ["15", "8", "="]) })
  }));
  const { agent, sleeps } = agentFor(
    client,
    decider([gateFailed("low_margin", { confidence: 0.8, margin: 0.04 }), clickOn("7"), finished()])
  );
  const result = await agent.run("7을 누르세요");

  assert.equal(result.status, "succeeded");
  assert.equal(result.steps, 1);
  assert.equal(result.decisions, 3);
  assert.equal(client.of("get-app-state").length, 2);
  assert.deepEqual(sleeps, [600]);
});

/** Asking the same question again cannot mend an answer that never fitted the question. */
test("an answer we never offered stops at once, with no second look", async () => {
  const client = fakeClient(baseHandlers({ click: ok({ delivered: true }) }));
  const { agent, sleeps } = agentFor(
    client,
    decider([gateFailed("unknown_choice", { choice: "option-99", offeredCount: 3 })])
  );
  const result = await agent.run("무언가 하세요");

  assert.equal(result.status, "blocked");
  assert.equal(result.reason, "unknown_choice");
  assert.equal(result.detail.retried, false);
  assert.equal(result.decisions, 1);
  assert.equal(client.of("get-app-state").length, 1, "nothing was worth observing again");
  assert.deepEqual(sleeps, []);
});

test("a step that goes through restores the allowance, so a later gate failure is retried too", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, ["7", "8", "="])),
    click: ok({ delivered: true, observation: observation("obs-act", ["15", "8", "="]) })
  }));
  const uncertain = gateFailed("low_confidence", { confidence: 0.4, margin: 0.2 });
  const { agent, sleeps } = agentFor(client, decider([uncertain, clickOn("7"), uncertain, finished()]));
  const result = await agent.run("7을 누르세요");

  assert.equal(result.status, "succeeded");
  assert.equal(result.reason, "goal_reached");
  assert.equal(result.steps, 1);
  assert.equal(result.decisions, 4, "each of the two gate failures got its own second look");
  assert.deepEqual(sleeps, [600, 600]);
  assert.equal(client.of("get-app-state").length, 3);
});

test("an unmet expectation demotes a run the model called finished", async () => {
  const client = fakeClient(baseHandlers({
    "get-app-state": (_params, nth) => ok(observation(`obs-${nth}`, ["7", "8", "="])),
    click: ok({ delivered: true, observation: observation("obs-act", ["7", "8", "="]) })
  }));
  const { agent } = agentFor(client, decider([finished()]), { expectElementName: "15\\s*$" });
  const unmet = await agent.run("15를 만드세요");
  assert.equal(unmet.status, "failed");
  assert.equal(unmet.reason, "expectation_unmet");
  assert.equal(unmet.expectation, "unmet");

  const met = agentFor(
    fakeClient(baseHandlers({ "get-app-state": ok(observation("obs-1", ["디스플레이 15", "8"])) })),
    decider([finished()]),
    { expectElementName: "15\\s*$" }
  );
  const result = await met.agent.run("15를 만드세요");
  assert.equal(result.status, "succeeded");
  assert.equal(result.expectation, "met");
});

test("the trace carries the documented records, the exact cost, and no state text", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-trace-"));
  try {
    const client = fakeClient({
      "session.status": ok({ active: true, sessionId: "s1" }),
      "list-windows": ok({ windows: [{ id: "w1", app: "forms.exe", title: "양식", isForeground: true }] }),
      "get-app-state": ok(fieldObservation("obs-1")),
      click: ok({ delivered: true }),
      "set-value": ok({ delivered: true, observation: fieldObservation("obs-2", "s3cr3t-preset") })
    });
    const trace = await createTrace({ runId: "run-trace", directory });
    const { agent } = agentFor(
      client,
      decider([typeInto("이름", 120), finished(80)]),
      { app: "forms.exe", texts: { "이름": "s3cr3t-preset" } },
      { trace }
    );
    const result = await agent.run("이름 칸을 채우세요");
    assert.equal(result.status, "succeeded");
    assert.equal(result.tracePath, join(directory, "run-trace.jsonl"));

    const text = await readFile(result.tracePath, "utf8");
    const records = text.trim().split("\n").map(line => JSON.parse(line));
    assert.equal(records[0].type, "run");
    assert.equal(records[0].goal, "이름 칸을 채우세요");
    assert.deepEqual(records[0].budgets, result.budgets);
    assert.equal(records.at(-1).type, "summary");
    assert.equal(records.at(-1).status, "succeeded");

    const decides = records.filter(record => record.type === "decide");
    assert.equal(decides.length, 2);
    assert.equal(decides[0].model, "jev-test");
    assert.equal(decides[0].candidateCount, 2);
    assert.equal(decides[0].rawElementCount, 2);
    assert.equal(decides[0].operation, "enter_text");
    assert.equal(decides[0].targetLabel, "이름");
    assert.equal(decides[0].gate.passed, true);
    assert.deepEqual(decides[0].answers.operation.probabilities, { enter_text: 0.98, click: 0.02 });

    // Summing the trace must give the run's exact cost.
    const summed = decides.reduce((total, record) => total + record.usage.input_tokens, 0);
    assert.equal(summed, records.at(-1).usage.input_tokens);
    assert.equal(summed, result.usage.input_tokens);
    assert.equal(summed, 200);

    const acts = records.filter(record => record.type === "act");
    assert.deepEqual(acts.map(record => record.method), ["set-value"]);
    assert.equal(acts[0].step, 1);
    assert.equal(acts[0].targetLabel, "이름");
    assert.equal(acts[0].outcome, "ok");
    assert.equal(typeof acts[0].elapsedMs, "number");
    assert.equal(acts[0].nativeMs, 7);
    assert.equal(acts[0].textSource, "preset");
    assert.equal(acts[0].textChars, 13);
    for (const act of acts) {
      assert.equal(act.params.sessionId, undefined, "the session id is a credential");
      assert.equal(act.params.text, undefined);
      assert.equal(act.params.value, undefined);
    }

    // Never the key, never the state payload, never a value we were asked to type.
    assert.ok(!text.includes("s3cr3t-preset"));
    assert.ok(!text.includes("The controls on screen now"));
    assert.ok(!text.includes("What has happened so far"));
    for (const record of records) {
      assert.ok(!("state" in record), `${record.type} must not carry the state payload`);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("a memory trace records the same events, so a caller can inspect without a file", async () => {
  const records = [];
  const client = fakeClient(baseHandlers({
    click: ok({ delivered: true, observation: observation("obs-2", ["15"]) })
  }));
  const { agent } = agentFor(client, decider([clickOn("7"), finished()]), {}, { trace: memoryTrace(records) });
  await agent.run("7을 누르세요");
  assert.deepEqual(records.map(record => record.type), ["run", "decide", "act", "decide", "summary"]);
});

/* -------------------------------------------------- the CLI output contract */

function endpointFor(directory, name) {
  if (process.platform === "win32") return `\\\\.\\pipe\\dcu-agent-${process.pid}-${name}`;
  return join(directory, `${name}.sock`);
}

function jevAnswers(request, asks) {
  const targets = Object.keys(request.questions.click_target?.criteria ?? {});
  if (asks === 1 && targets.length > 0) {
    return {
      progress: { choice: "work_remains", confidence: 0.97, probabilities: { work_remains: 0.97, goal_reached: 0.03 } },
      obstacle: { choice: "path_clear", confidence: 0.95, probabilities: { path_clear: 0.95, blocked: 0.05 } },
      operation: { choice: "click", confidence: 0.99, probabilities: { click: 0.99, wait: 0.01 } },
      click_target: { choice: targets[0], confidence: 0.96, probabilities: { [targets[0]]: 0.96, [targets[1] ?? "x"]: 0.04 } }
    };
  }
  return {
    progress: { choice: "goal_reached", confidence: 0.99, probabilities: { goal_reached: 0.99, work_remains: 0.01 } },
    obstacle: { choice: "path_clear", confidence: 0.98, probabilities: { path_clear: 0.98, blocked: 0.02 } },
    operation: { choice: "wait", confidence: 0.99, probabilities: { wait: 0.99, click: 0.01 } }
  };
}

async function withFakeStack(body) {
  const directory = await mkdtemp(join(tmpdir(), "dcu-agent-cli-"));
  const traceDirectory = await mkdtemp(join(tmpdir(), "dcu-agent-trace-"));
  const endpoint = endpointFor(directory, "cli");
  await writeFile(join(directory, "token"), `${"t".repeat(48)}\n`, "utf8");

  const daemon = net.createServer(socket => {
    let body = "";
    socket.on("data", chunk => {
      body += chunk.toString("utf8");
      const newline = body.indexOf("\n");
      if (newline < 0) return;
      const request = JSON.parse(body.slice(0, newline));
      const answer = { session: { active: true, sessionId: "s-cli", stopping: false, idleTimeoutMs: 120000 } };
      let result;
      if (request.method === "session.status") result = answer.session;
      else if (request.method === "list-windows") {
        result = { windows: [{ id: "w1", app: "calc.exe", title: "계산기", isForeground: true, isMinimized: false }] };
      } else if (request.method === "get-app-state") result = observation("obs-1", ["7", "8", "="]);
      else if (request.method === "click") {
        result = { delivered: true, verification: { state: "unverified" }, observation: observation("obs-2", ["15", "8", "="]) };
      }
      socket.end(`${JSON.stringify(result === undefined
        ? { id: request.id, ok: false, error: { code: "unsupported_method", message: request.method } }
        : { id: request.id, ok: true, result })}\n`);
    });
  });
  await new Promise((resolve, reject) => {
    daemon.once("error", reject);
    daemon.listen(endpoint, resolve);
  });

  let asks = 0;
  const jev = http.createServer((request, response) => {
    let payload = "";
    request.on("data", chunk => {
      payload += chunk;
    });
    request.on("end", () => {
      asks += 1;
      const answers = jevAnswers(JSON.parse(payload), asks);
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ model: "jev-test", usage: { input_tokens: 42, output_tokens: 3 }, answers }));
    });
  });
  await new Promise(resolve => jev.listen(0, "127.0.0.1", resolve));

  const testEnvFile = join(directory, "test.env");
  await writeFile(testEnvFile, "", "utf8");
  const inheritedEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
    !/^(DCU_|TYPESAFE_API_KEY$)/i.test(key)
  ));
  const env = {
    ...inheritedEnv,
    DCU_ENV_FILE: testEnvFile,
    DCU_RUNTIME_DIR: directory,
    DCU_ENDPOINT: endpoint,
    DCU_AGENT_TRACE_DIR: traceDirectory,
    DCU_AGENT_BASE_URL: `http://127.0.0.1:${jev.address().port}`,
    DCU_AGENT_MODEL: "jev-test",
    TYPESAFE_API_KEY: "test-key-not-a-real-one"
  };
  try {
    await body({ env, reset: () => { asks = 0; } });
  } finally {
    daemon.close();
    jev.close();
    await rm(directory, { recursive: true, force: true });
    await rm(traceDirectory, { recursive: true, force: true });
  }
}

test("agent run prints exactly one JSON line, and with --stream the last line is still that line", async () => {
  await withFakeStack(async ({ env, reset }) => {
    const cli = join(process.cwd(), "dist", "cli.js");
    const args = ["agent", "run", "--goal", "7을 누르세요", "--app", "calc.exe", "--max-steps", "3"];

    const quiet = await run(process.execPath, [cli, ...args], { env, encoding: "utf8" });
    const quietLines = quiet.stdout.split("\n").filter(line => line.length > 0);
    assert.equal(quietLines.length, 1, `expected one line, got ${quiet.stdout}`);
    const envelope = JSON.parse(quietLines[0]);
    assert.equal(envelope.id, null);
    assert.equal(envelope.ok, true);
    assert.equal(envelope.result.status, "succeeded");
    assert.equal(envelope.result.steps, 1);
    assert.equal(envelope.result.usage.input_tokens, 84);

    reset();
    const streamed = await run(process.execPath, [cli, ...args, "--stream"], { env, encoding: "utf8" });
    const streamLines = streamed.stdout.split("\n").filter(line => line.length > 0);
    assert.ok(streamLines.length >= 2, `expected a step record and a summary, got ${streamed.stdout}`);
    const parsed = streamLines.map(line => JSON.parse(line));
    for (const record of parsed.slice(0, -1)) {
      assert.equal(record.type, "step");
      assert.equal(typeof record.action, "string");
    }
    const last = parsed.at(-1);
    assert.deepEqual(Object.keys(last).sort(), ["id", "ok", "result"]);
    assert.equal(last.result.status, "succeeded");
    assert.equal(last.result.reason, "goal_reached");
  });
});

test("agent run --dry-run decides once and touches nothing", async () => {
  await withFakeStack(async ({ env }) => {
    const cli = join(process.cwd(), "dist", "cli.js");
    const result = await run(
      process.execPath,
      [cli, "agent", "run", "--goal", "7을 누르세요", "--app", "calc.exe", "--dry-run"],
      { env, encoding: "utf8" }
    );
    const lines = result.stdout.split("\n").filter(line => line.length > 0);
    assert.equal(lines.length, 1);
    const envelope = JSON.parse(lines[0]);
    assert.equal(envelope.result.performedInput, false);
    assert.equal(envelope.result.decision.operation, "click");
    assert.equal(envelope.result.plannedCalls[0].method, "click");
    assert.equal(envelope.result.plannedCalls[0].params.observe, "text");
    assert.equal(envelope.result.plannedCalls[0].params.sessionId, undefined);
    assert.equal(envelope.result.candidateCount, 3);
  });
});

import test from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { decide } from "../dist/agent/decide.js";
import { FetchJevTransport, describeShape, readJevConfig, retryAfterMs } from "../dist/agent/jev.js";

/** A recording of a keypad: a grid of small labelled buttons and no text field. */
const KEYPAD = join(process.cwd(), "tests", "fixtures", "uia", "app-02-foreground.json");
const GOAL = "7 더하기 8을 계산한다";
/** A stand-in string, never a credential; no test here reaches a network. */
const FAKE_KEY = "not-a-real-key";
const CONFIG = { apiKey: FAKE_KEY, baseUrl: "https://api.invalid/v1", model: "jev-test", timeoutMs: 50 };

function choice(value, probabilities, confidence) {
  return { type: "choice", choice: value, probabilities, confidence };
}

/** A hand-written reply body, replayed without touching the network. */
function replyBody(overrides = {}) {
  return {
    model: "jev-test",
    answers: {
      progress: choice("work_remains", { work_remains: 0.93, goal_reached: 0.07 }, 0.91),
      obstacle: choice("path_clear", { path_clear: 0.89, blocked: 0.11 }, 0.87),
      operation: choice(
        "click",
        { click: 0.86, press_key: 0.06, hotkey: 0.03, scroll: 0.03, wait: 0.02 },
        0.84
      ),
      click_target: choice("e44", { e44: 0.71, e45: 0.12, e34: 0.09, e35: 0.08 }, 0.76),
      scroll_target: choice("r_main", { r_main: 1 }, 0.99),
      scroll_direction: choice("down", { down: 0.7, up: 0.2, left: 0.05, right: 0.05 }, 0.7),
      press_key_choice: choice("enter", { enter: 0.5, escape: 0.2, tab: 0.1, down: 0.1, up: 0.1 }, 0.5),
      hotkey_choice: choice("ctrl+a", { "ctrl+a": 0.4, "ctrl+c": 0.3, "ctrl+v": 0.3 }, 0.4),
      ...overrides
    },
    usage: { input_tokens: 1180, output_tokens: 64 }
  };
}

/** A transport that replays one body and records what it was asked. */
function fakeTransport(body) {
  const seen = [];
  return {
    seen,
    async ask(request) {
      seen.push(request);
      return body;
    }
  };
}

function jsonResponse(status, body, headers = {}) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });
}

function planShapedRequest() {
  return {
    state: ["Goal: 계좌 비밀번호 창을 연다", "The controls on screen now:\ne1 button \"비밀번호\" — middle-center"],
    model: "jev-test",
    questions: {
      progress: { type: "choice", instructions: "…", criteria: { goal_reached: "a", work_remains: "b" } },
      click_target: { type: "choice", instructions: "…", criteria: { e1: "a", e2: "b", e3: "c" } }
    }
  };
}

test("a well-formed reply becomes a decision that names a native element index", async () => {
  const transport = fakeTransport(replyBody());
  const result = await decide({ goal: GOAL, fixturePath: KEYPAD, transport, config: CONFIG });
  assert.equal(result.performedInput, false);
  assert.equal(result.source, "fixture");
  assert.equal(result.gateFailure, undefined);
  assert.equal(result.decision.status, "continue");
  assert.equal(result.decision.operation, "click");
  assert.equal(result.decision.target.optionId, "e44");
  assert.equal(result.decision.target.elementIndex, 44);
  // The label element 44 carries in app-02-foreground.json.
  assert.equal(result.decision.target.label, "6");
  assert.deepEqual(result.usage, { input_tokens: 1180, output_tokens: 64 });
  assert.ok(result.estimatedStateTokens > 0);
  // One request carried every question; the unused answers were simply discarded.
  assert.equal(transport.seen.length, 1);
  assert.ok(transport.seen[0].questions.hotkey_choice);
});

test("a reply that reports the goal reached comes back as done", async () => {
  const transport = fakeTransport(replyBody({
    progress: choice("goal_reached", { goal_reached: 0.9, work_remains: 0.1 }, 0.9)
  }));
  const result = await decide({ goal: GOAL, fixturePath: KEYPAD, transport, config: CONFIG });
  assert.equal(result.decision.status, "done");
});

test("a choice that was never offered is refused rather than coerced", async () => {
  const transport = fakeTransport(replyBody({
    click_target: choice("e9999", { e9999: 0.8, e44: 0.2 }, 0.9)
  }));
  const result = await decide({ goal: GOAL, fixturePath: KEYPAD, transport, config: CONFIG });
  assert.equal(result.decision, undefined);
  assert.equal(result.gateFailure.reason, "unknown_choice");
  assert.equal(result.gateFailure.question, "click_target");
  assert.equal(result.gateFailure.choice, "e9999");
});

test("a reply missing a question key is refused rather than guessed", async () => {
  const body = replyBody();
  delete body.answers.click_target;
  const result = await decide({ goal: GOAL, fixturePath: KEYPAD, transport: fakeTransport(body), config: CONFIG });
  assert.equal(result.decision, undefined);
  assert.equal(result.gateFailure.reason, "missing_answer");
  assert.equal(result.gateFailure.question, "click_target");
});

test("429 honours Retry-After, retries, and gives up after three attempts", async () => {
  const calls = [];
  const slept = [];
  const transport = new FetchJevTransport({
    config: CONFIG,
    fetchImpl: async () => {
      calls.push(Date.now());
      return jsonResponse(429, { error: "slow down" }, { "retry-after": "2" });
    },
    sleep: async ms => {
      slept.push(ms);
    },
    log: () => {}
  });
  await assert.rejects(
    transport.ask(planShapedRequest(), new AbortController().signal),
    error => {
      assert.equal(error.name, "DcuError");
      assert.equal(error.code, "agent_rate_limited");
      return true;
    }
  );
  assert.equal(calls.length, 3, "expected three attempts and no more");
  assert.equal(transport.attempts, 3);
  assert.deepEqual(slept, [2000, 2000], "Retry-After was not honoured");
});

test("a 429 that clears on the second attempt returns the reply", async () => {
  let call = 0;
  const transport = new FetchJevTransport({
    config: CONFIG,
    fetchImpl: async () => {
      call += 1;
      return call === 1 ? jsonResponse(429, {}, { "retry-after": "1" }) : jsonResponse(200, replyBody());
    },
    sleep: async () => {},
    log: () => {}
  });
  const response = await transport.ask(planShapedRequest(), new AbortController().signal);
  assert.equal(transport.attempts, 2);
  assert.equal(response.usage.input_tokens, 1180);
});

test("a 422 logs the question shape and never the text of the user's window", async () => {
  const lines = [];
  const request = planShapedRequest();
  const transport = new FetchJevTransport({
    config: CONFIG,
    fetchImpl: async () => jsonResponse(422, { error: "unprocessable" }),
    sleep: async () => {},
    log: line => lines.push(line)
  });
  await assert.rejects(transport.ask(request, new AbortController().signal), error => {
    assert.equal(error.code, "agent_request");
    return true;
  });
  assert.equal(lines.length, 1);
  assert.match(lines[0], /progress\(2\)/);
  assert.match(lines[0], /click_target\(3\)/);
  for (const section of request.state) {
    assert.ok(!lines[0].includes(section), "the state reached the log");
  }
  assert.ok(!lines[0].includes("비밀번호"), "window text reached the log");
  assert.ok(!lines[0].includes(FAKE_KEY), "the api key reached the log");
  assert.equal(transport.attempts, 1);
});

test("401 aborts at once, names the variable, and never retries", async () => {
  let calls = 0;
  const transport = new FetchJevTransport({
    config: CONFIG,
    fetchImpl: async () => {
      calls += 1;
      return jsonResponse(401, { error: "unauthorized" });
    },
    sleep: async () => {
      throw new Error("a failed auth must never back off and retry");
    },
    log: () => {}
  });
  await assert.rejects(transport.ask(planShapedRequest(), new AbortController().signal), error => {
    assert.equal(error.name, "DcuError");
    assert.equal(error.code, "agent_config");
    assert.match(error.message, /TYPESAFE_API_KEY/);
    assert.ok(!error.message.includes(FAKE_KEY));
    return true;
  });
  assert.equal(calls, 1, "a 401 was retried");
});

test("a timeout is retried once and then reported as a typed failure", async () => {
  let calls = 0;
  const transport = new FetchJevTransport({
    config: CONFIG,
    fetchImpl: async () => {
      calls += 1;
      throw Object.assign(new Error("The operation was aborted due to timeout"), { name: "TimeoutError" });
    },
    sleep: async () => {},
    log: () => {}
  });
  await assert.rejects(transport.ask(planShapedRequest(), new AbortController().signal), error => {
    assert.equal(error.code, "agent_timeout");
    return true;
  });
  assert.equal(calls, 2);
});

test("a missing TYPESAFE_API_KEY is a typed config error that names the variable", async () => {
  const transport = new FetchJevTransport({
    config: { baseUrl: CONFIG.baseUrl, model: CONFIG.model, timeoutMs: 50 },
    fetchImpl: async () => {
      throw new Error("nothing should be sent without a key");
    },
    log: () => {}
  });
  await assert.rejects(transport.ask(planShapedRequest(), new AbortController().signal), error => {
    assert.equal(error.code, "agent_config");
    assert.match(error.message, /TYPESAFE_API_KEY/);
    return true;
  });
  await assert.rejects(
    decide({ goal: GOAL, fixturePath: KEYPAD, config: { baseUrl: CONFIG.baseUrl, model: "jev-test", timeoutMs: 50 } }),
    error => {
      assert.equal(error.code, "agent_config");
      assert.match(error.message, /TYPESAFE_API_KEY/);
      return true;
    }
  );
});

test("the config comes from the environment and carries documented defaults", () => {
  const bare = readJevConfig({});
  assert.equal(bare.baseUrl, "https://api.typesafe.ai/v1");
  assert.equal(bare.model, "jev-latest");
  assert.equal(bare.timeoutMs, 8000);
  assert.equal(bare.apiKey, undefined);
  const tuned = readJevConfig({
    TYPESAFE_API_KEY: FAKE_KEY,
    DCU_AGENT_BASE_URL: "https://example.invalid/v2/",
    DCU_AGENT_MODEL: "jev-2",
    DCU_AGENT_TIMEOUT_MS: "1500"
  });
  assert.equal(tuned.baseUrl, "https://example.invalid/v2");
  assert.equal(tuned.model, "jev-2");
  assert.equal(tuned.timeoutMs, 1500);
  assert.equal(tuned.apiKey, FAKE_KEY);
});

test("Retry-After is read as seconds or as a date, and anything else is ignored", () => {
  assert.equal(retryAfterMs("3"), 3000);
  assert.equal(retryAfterMs("0"), 0);
  assert.equal(retryAfterMs(null), undefined);
  assert.equal(retryAfterMs("soon"), undefined);
  const inTwoSeconds = new Date(Date.now() + 2000).toUTCString();
  const parsed = retryAfterMs(inTwoSeconds);
  assert.ok(parsed >= 0 && parsed <= 3000, `date Retry-After parsed as ${parsed}`);
});

test("the 422 log line is built from question keys and option counts alone", () => {
  assert.equal(describeShape(planShapedRequest()), "progress(2) click_target(3)");
});

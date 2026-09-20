import test from "node:test";
import assert from "node:assert/strict";
import { readHarnessConfig, readHarnessMode } from "../dist/agent/config.js";
import { readJevConfig } from "../dist/agent/jev.js";

test("the harness defaults to the current LLM and needs no external model provider", () => {
  assert.equal(readHarnessMode({}), "llm");
  assert.deepEqual(readHarnessConfig({}), {
    mode: "llm",
    llm: "harness",
    jev: {
      required: false,
      apiKeyPresent: false,
      host: "api.typesafe.ai",
      model: "jev-latest",
      timeoutMs: 8000
    },
    ready: true
  });
});

test("hybrid mode reports Jev readiness without exposing its key", () => {
  const report = readHarnessConfig({
    DCU_AGENT_MODE: "llm+jev",
    DCU_JEV_API_KEY: "private-test-key",
    DCU_JEV_BASE_URL: "https://jev.example.test/v2/",
    DCU_JEV_MODEL: "jev-test",
    DCU_JEV_TIMEOUT_MS: "1250"
  });
  assert.equal(report.mode, "llm+jev");
  assert.equal(report.llm, "harness");
  assert.deepEqual(report.jev, {
    required: true,
    apiKeyPresent: true,
    host: "jev.example.test",
    model: "jev-test",
    timeoutMs: 1250
  });
  assert.equal(report.ready, true);
  assert.doesNotMatch(JSON.stringify(report), /private-test-key/);

  const missing = readHarnessConfig({ DCU_AGENT_MODE: "llm+jev" });
  assert.equal(missing.ready, false);
  assert.equal(missing.jev.apiKeyPresent, false);
});

test("the harness rejects a provider mode that it cannot invoke itself", () => {
  assert.throws(() => readHarnessMode({ DCU_AGENT_MODE: "jev" }), error => {
    assert.equal(error.name, "DcuError");
    assert.equal(error.code, "agent_config");
    assert.match(error.message, /llm or llm\+jev/);
    return true;
  });
});

test("Jev-specific variables override legacy agent variables and preserve legacy-only setups", () => {
  const preferred = readJevConfig({
    DCU_JEV_API_KEY: "new-key",
    DCU_JEV_BASE_URL: "https://new.example/v1/",
    DCU_JEV_MODEL: "new-model",
    DCU_JEV_TIMEOUT_MS: "9000",
    TYPESAFE_API_KEY: "legacy-key",
    DCU_AGENT_BASE_URL: "https://legacy.example/v1",
    DCU_AGENT_MODEL: "legacy-model",
    DCU_AGENT_TIMEOUT_MS: "1000"
  });
  assert.deepEqual(preferred, {
    apiKey: "new-key",
    baseUrl: "https://new.example/v1",
    model: "new-model",
    timeoutMs: 9000
  });

  const legacy = readJevConfig({
    TYPESAFE_API_KEY: "legacy-key",
    DCU_AGENT_BASE_URL: "https://legacy.example/v1/",
    DCU_AGENT_MODEL: "legacy-model",
    DCU_AGENT_TIMEOUT_MS: "1000"
  });
  assert.deepEqual(legacy, {
    apiKey: "legacy-key",
    baseUrl: "https://legacy.example/v1",
    model: "legacy-model",
    timeoutMs: 1000
  });
});

test("an explicitly empty Jev-specific variable does not reveal a legacy credential", () => {
  const config = readJevConfig({
    DCU_JEV_API_KEY: "",
    TYPESAFE_API_KEY: "legacy-key"
  });
  assert.equal(config.apiKey, undefined);
});

test("a malformed Jev URL is reported without reflecting possible credentials", () => {
  const report = readHarnessConfig({
    DCU_JEV_BASE_URL: "not a URL?token=private-value"
  });
  assert.equal(report.jev.host, "(invalid URL)");
  assert.doesNotMatch(JSON.stringify(report), /private-value/);
});

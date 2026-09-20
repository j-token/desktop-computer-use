import { createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DcuClient } from "../dist/client.js";
import { readSession, resolveNativePath } from "../dist/runtime.js";

const HELP = `Usage: node scripts/measure-elements.mjs [options]

Surveys how many accessibility elements real windows expose through get-app-state
with includeText: true. Every number written comes from a measurement; a field the
daemon did not return is recorded as null.

  --app <substring>            Windows whose app or title contains it (case-insensitive). Repeatable.
  --window-id <id>             Only this window id. Repeatable.
  --no-activate                Skip the foreground pass for every window.
  --skip-activate <substring>  Skip the foreground pass when the title contains it. Repeatable.
  --help                       Show this text.

This script never starts or stops a session and never takes screenshots. Start a
session yourself first:
  node skills/desktop-computer-use/scripts/dcu.mjs session start`;

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const fixtureDir = join(root, "tests", "fixtures", "uia");
const resultsDir = join(root, "artifacts", "validation", "uia-element-survey");
const KNOWN_NATIVE = ".cache/package-bin/win32-x64/desktop-computer-use-native.exe";
const RESPONSE_BYTES_LIMIT = 8 * 1024 * 1024;
const ACCESSIBILITY_MS_LIMIT = 2000;

function parseArgs(argv) {
  const options = { apps: [], windowIds: [], skipActivate: [], activate: true, help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const equals = argv[i].indexOf("=");
    const name = equals >= 0 ? argv[i].slice(0, equals) : argv[i];
    const inline = equals >= 0 ? argv[i].slice(equals + 1) : undefined;
    const value = () => {
      const raw = inline ?? argv[++i];
      if (raw === undefined || raw === "") throw Error(`${name} requires a value`);
      return raw;
    };
    if (name === "--help" || name === "-h") options.help = true;
    else if (name === "--no-activate") options.activate = false;
    else if (name === "--app") options.apps.push(value());
    else if (name === "--window-id") options.windowIds.push(value());
    else if (name === "--skip-activate") options.skipActivate.push(value());
    else throw Error(`Unknown option ${name}`);
  }
  return options;
}

function matchesFilters(window, options) {
  if (options.windowIds.length > 0 && options.windowIds.includes(window.id)) return true;
  const haystack = `${window.app ?? ""}\n${window.title ?? ""}`.toLowerCase();
  if (options.apps.some(app => haystack.includes(app.toLowerCase()))) return true;
  return options.apps.length === 0 && options.windowIds.length === 0;
}

function shortHash(value) {
  return createHash("sha1").update(String(value)).digest("hex").slice(0, 8);
}

function safeAppName(app, windowId) {
  const cleaned = String(app ?? "").replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "").toLowerCase();
  return cleaned || `window-${shortHash(windowId)}`;
}

function failure(error) {
  return { code: error?.code ?? null, message: error?.message ?? String(error) };
}

function percent(count, total) {
  return total > 0 ? Math.round((count / total) * 1000) / 10 : null;
}

function hasPositiveArea(element) {
  return element.bounds?.width > 0 && element.bounds?.height > 0;
}

function hasPatterns(element) {
  return Array.isArray(element.patterns) && element.patterns.length > 0;
}

function hasAutomationId(element) {
  return typeof element.automationId === "string" && element.automationId !== "";
}

const FLAG_TESTS = {
  emptyName: e => e.name === "",
  emptyNameButHasAutomationId: e => e.name === "" && hasAutomationId(e),
  hasAutomationId,
  offscreen: e => e.offscreen === true,
  disabled: e => e.enabled === false,
  zeroArea: e => !hasPositiveArea(e),
  redactedValue: e => e.value === "[redacted]"
};

function tokenEstimate(elements) {
  return elements.reduce((total, e) => {
    return total + Math.ceil(`${e.controlType} "${e.name}"${e.value ? ` = ${e.value}` : ""}`.length / 3);
  }, 0);
}

function analyze(result, roundtripMs) {
  const accessibility = result.accessibility ?? null;
  const elements = Array.isArray(accessibility?.elements) ? accessibility.elements : null;
  const measurement = {
    observationId: result.observationId ?? null,
    roundtripMs,
    timings: result.timings ?? null,
    elementCount: accessibility?.elementCount ?? null,
    elementsLength: elements ? elements.length : null,
    countsAgree: elements ? accessibility.elementCount === elements.length : null,
    totalResponseBytes: JSON.stringify(result).length,
    accessibilityBytes: accessibility ? JSON.stringify(accessibility).length : null,
    treeBytes: typeof accessibility?.tree === "string" ? accessibility.tree.length : null,
    elementsBytes: elements ? JSON.stringify(elements).length : null
  };
  if (!elements) {
    // The daemon answered without an accessibility payload; nothing below is measurable.
    return { ...measurement, accessibilityMissing: true };
  }
  const actionable = elements.filter(e => hasPatterns(e) && e.offscreen === false && hasPositiveArea(e));
  const byControlType = {};
  const byPattern = {};
  for (const element of elements) {
    const type = element.controlType ?? "(missing)";
    byControlType[type] = (byControlType[type] ?? 0) + 1;
    for (const pattern of element.patterns ?? []) byPattern[pattern] = (byPattern[pattern] ?? 0) + 1;
  }
  const flags = Object.fromEntries(Object.entries(FLAG_TESTS).map(([key, test]) => {
    const count = elements.filter(test).length;
    return [key, { count, percentOfTotal: percent(count, elements.length) }];
  }));
  return {
    ...measurement,
    actionable: actionable.length,
    patternsOnly: elements.filter(hasPatterns).length,
    onscreenOnly: elements.filter(e => e.offscreen === false).length,
    positiveAreaOnly: elements.filter(hasPositiveArea).length,
    enabledActionable: actionable.filter(e => e.enabled === true).length,
    byControlType: Object.fromEntries(Object.entries(byControlType).sort((a, b) => b[1] - a[1])),
    byPattern: Object.fromEntries(Object.entries(byPattern).sort((a, b) => b[1] - a[1])),
    flags,
    estimatedTokens: {
      note: "Rough estimate only: ceil(length / 3) over a candidate `controlType \"name\" = value` table row.",
      allElements: tokenEstimate(elements),
      actionableOnly: tokenEstimate(actionable)
    },
    sampleActionable: actionable.slice(0, 25).map(e => ({
      index: e.index, controlType: e.controlType, name: e.name,
      automationId: e.automationId, patterns: e.patterns, value: e.value ?? null
    }))
  };
}

async function observe(client, sessionId, windowId) {
  const started = performance.now();
  try {
    const params = { sessionId, windowId, includeText: true, includeScreenshot: false };
    return { ok: true, result: await client.call("get-app-state", params), roundtripMs: performance.now() - started };
  } catch (error) {
    return { ok: false, roundtripMs: performance.now() - started, error: failure(error) };
  }
}

function median(values) {
  if (values.length === 0) return null;
  const ordered = [...values].sort((a, b) => a - b);
  const middle = ordered.length >> 1;
  return ordered.length % 2 === 1 ? ordered[middle] : (ordered[middle - 1] + ordered[middle]) / 2;
}

function truncate(value, length) {
  const text = String(value ?? "");
  return text.length > length ? `${text.slice(0, length - 1)}…` : text;
}

function cell(value) {
  return value === null || value === undefined ? "null" : String(value).replaceAll("|", "\\|");
}

function reportMarkdown(survey) {
  const lines = [`# UIA element survey ${survey.recordedAt}`, ""];
  lines.push(`Session ${survey.sessionId} · native ${survey.nativePath} · ${survey.windows.length} windows`, "");
  lines.push("| app | title | pass | elementCount | actionable | enabledActionable | % empty name | % has automationId | accessibilityMs | roundtripMs | accessibilityBytes | treeBytes |");
  lines.push("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const entry of survey.windows) {
    for (const pass of entry.passes) {
      const m = pass.measurement;
      const head = `| ${cell(entry.window.app)} | ${cell(truncate(entry.window.title, 40))} | ${pass.pass} |`;
      lines.push(m
        ? `${head} ${cell(m.elementCount)} | ${cell(m.actionable)} | ${cell(m.enabledActionable)} | ${cell(m.flags?.emptyName.percentOfTotal)} | ${cell(m.flags?.hasAutomationId.percentOfTotal)} | ${cell(m.timings?.accessibilityMs)} | ${cell(Math.round(m.roundtripMs))} | ${cell(m.accessibilityBytes)} | ${cell(m.treeBytes)} |`
        : `${head} failed: ${cell(pass.error?.code)} ${cell(pass.error?.message)} | | | | | | | | |`);
    }
  }
  for (const entry of survey.windows) {
    for (const pass of entry.passes) {
      if (!pass.measurement?.byControlType) continue;
      lines.push("", `## ${entry.window.app} — ${truncate(entry.window.title, 40)} (${pass.pass})`, "", "### controlType breakdown", "");
      for (const [type, count] of Object.entries(pass.measurement.byControlType)) lines.push(`- ${type}: ${count}`);
      lines.push("", "### first 25 actionable elements", "");
      if (pass.measurement.sampleActionable.length === 0) lines.push("- none");
      for (const sample of pass.measurement.sampleActionable) {
        lines.push(`- [${sample.index}] ${sample.controlType} "${sample.name}" automationId=${cell(sample.automationId)} patterns=${(sample.patterns ?? []).join(",")} value=${cell(sample.value)}`);
      }
    }
  }
  lines.push("", survey.verdict.line, "");
  return lines.join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log(HELP);
    return 0;
  }
  const client = new DcuClient();
  let nativePath;
  try {
    nativePath = await resolveNativePath();
  } catch (error) {
    console.error(`No native daemon binary: ${error.message}`);
    console.error(`Set DCU_NATIVE_PATH, for example to ${KNOWN_NATIVE}`);
    return 1;
  }
  const session = await readSession(client.paths);
  if (!session) {
    console.error("No active session. This script never starts or stops one. Run:");
    console.error("  node skills/desktop-computer-use/scripts/dcu.mjs session start");
    return 1;
  }
  const sessionId = session.sessionId;
  let listed;
  try {
    listed = await client.call("list-windows", { sessionId });
  } catch (error) {
    console.error(`list-windows failed (${error?.code ?? "unknown"}): ${error?.message ?? error}`);
    console.error(`If the daemon is unreachable, set DCU_NATIVE_PATH, for example to ${KNOWN_NATIVE}`);
    return 1;
  }
  const targets = (listed.windows ?? []).filter(window => matchesFilters(window, options));
  console.log(`Surveying ${targets.length} of ${(listed.windows ?? []).length} windows.`);

  await mkdir(fixtureDir, { recursive: true });
  const usedFixtureNames = new Set();
  const windows = [];
  for (const window of targets) {
    let base = safeAppName(window.app, window.id);
    if (usedFixtureNames.has(base)) base = `${base}-${shortHash(window.id)}`;
    usedFixtureNames.add(base);
    const skipped = options.skipActivate.some(s => String(window.title ?? "").toLowerCase().includes(s.toLowerCase()));
    const entry = { window, fixtureBase: base, foregroundSkipped: !options.activate || skipped, passes: [] };
    for (const pass of ["as-found", "foreground"]) {
      if (pass === "foreground") {
        if (entry.foregroundSkipped) break;
        try {
          // observe() never activates, so a backgrounded window can report a collapsed
          // tree. scroll activates the window (windows_backend.cpp:1069) before scrolling.
          await client.call("scroll", { sessionId, windowId: window.id, direction: "up", amount: 1 });
        } catch (error) {
          entry.activateError = failure(error);
          break;
        }
      }
      const observation = await observe(client, sessionId, window.id);
      const record = { pass, ok: observation.ok, error: observation.error ?? null, measurement: null, fixture: null };
      if (observation.ok) {
        record.measurement = analyze(observation.result, observation.roundtripMs);
        record.fixture = join("tests", "fixtures", "uia", `${base}-${pass === "as-found" ? "asfound" : "foreground"}.json`);
        const payload = { window: observation.result.window ?? null, accessibility: observation.result.accessibility ?? null };
        await writeFile(join(root, record.fixture), `${JSON.stringify(payload, null, 2)}\n`, { encoding: "utf8" });
      }
      entry.passes.push(record);
      const label = `${window.app} ${truncate(window.title, 40)} [${pass}]`;
      console.log(observation.ok
        ? `  ${label}: elementCount=${record.measurement.elementCount} actionable=${record.measurement.actionable ?? "null"} accessibilityMs=${record.measurement.timings?.accessibilityMs ?? "null"} roundtripMs=${Math.round(observation.roundtripMs)}`
        : `  ${label}: failed ${observation.error.code} ${observation.error.message}`);
    }
    windows.push(entry);
  }

  const contributions = [];
  let maxResponseBytes = 0;
  let maxAccessibilityMs = 0;
  for (const entry of windows) {
    for (const pass of entry.passes) {
      if (!pass.measurement) continue;
      maxResponseBytes = Math.max(maxResponseBytes, pass.measurement.totalResponseBytes);
      const accessibilityMs = pass.measurement.timings?.accessibilityMs;
      if (typeof accessibilityMs === "number") maxAccessibilityMs = Math.max(maxAccessibilityMs, accessibilityMs);
    }
    const chosen = entry.passes.find(p => p.pass === "foreground" && typeof p.measurement?.actionable === "number")
      ?? entry.passes.find(p => p.pass === "as-found" && typeof p.measurement?.actionable === "number");
    if (chosen) {
      contributions.push({ windowId: entry.window.id, app: entry.window.app, pass: chosen.pass, actionable: chosen.measurement.actionable });
    }
  }
  const medianActionable = median(contributions.map(c => c.actionable));
  const needsNativeFix = medianActionable !== null
    && (medianActionable > 120 || maxResponseBytes > RESPONSE_BYTES_LIMIT || maxAccessibilityMs > ACCESSIBILITY_MS_LIMIT);
  const decision = medianActionable === null
    ? "NO DATA (no window produced a measurable actionable count)"
    : needsNativeFix
      ? "PROCEED WITH NATIVE FIX FIRST (>120, or any response >8MB, or any accessibilityMs >2000)"
      : medianActionable < 10
        ? "STOP (<10)"
        : "PROCEED (10-120)";
  const verdict = {
    medianActionable, windowsCounted: contributions.length, decision, maxResponseBytes, maxAccessibilityMs, contributions,
    line: `VERDICT: median actionable = ${medianActionable ?? "null"} across ${contributions.length} windows -> ${decision}`
  };

  const stamp = new Date().toISOString().replaceAll(":", "-");
  const survey = {
    recordedAt: new Date().toISOString(), platform: process.platform, nativePath, sessionId,
    filters: { apps: options.apps, windowIds: options.windowIds, skipActivate: options.skipActivate, activate: options.activate },
    windowsListed: (listed.windows ?? []).length, windows, verdict
  };
  await mkdir(resultsDir, { recursive: true });
  const surveyPath = join(resultsDir, `survey-${stamp}.json`);
  const reportPath = join(resultsDir, `report-${stamp}.md`);
  await writeFile(surveyPath, `${JSON.stringify(survey, null, 2)}\n`, { encoding: "utf8" });
  await writeFile(reportPath, reportMarkdown(survey), { encoding: "utf8" });

  const row = (app, title, pass, elems, action, a11yMs, bytes) =>
    `${truncate(app, 22).padEnd(22)} ${truncate(title, 40).padEnd(42)} ${String(pass).padEnd(11)}` +
    ` ${String(elems).padStart(6)} ${String(action).padStart(7)} ${String(a11yMs).padStart(7)} ${String(bytes).padStart(9)}`;
  console.log("");
  console.log(row("app", "title", "pass", "elems", "action", "a11yMs", "bytes"));
  for (const entry of windows) {
    for (const pass of entry.passes) {
      const m = pass.measurement;
      console.log(row(entry.window.app, entry.window.title, pass.pass, m?.elementCount ?? "null",
        m?.actionable ?? "null", m?.timings?.accessibilityMs ?? "null", m?.totalResponseBytes ?? "null"));
    }
  }
  console.log("");
  for (const contribution of contributions) {
    console.log(`median input: ${contribution.app} (${contribution.windowId}) contributed ${contribution.actionable} from the ${contribution.pass} pass`);
  }
  console.log(`max response bytes ${maxResponseBytes}, max accessibilityMs ${maxAccessibilityMs}`);
  console.log(`survey: ${surveyPath}`);
  console.log(`report: ${reportPath}`);
  console.log(verdict.line);
  return 0;
}

process.exitCode = await main();

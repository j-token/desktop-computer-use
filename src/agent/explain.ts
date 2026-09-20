import { readFile } from "node:fs/promises";
import { DcuClient } from "../client.js";
import { DcuError, nativeError } from "../errors.js";
import { buildCandidates, normalizeLabel } from "./filter.js";
import type {
  Candidate,
  CandidateRole,
  FilterReport,
  Observation,
  Operation,
  PatternName,
  UiaElement
} from "./types.js";
import type { JsonObject } from "../types.js";

const PATTERN_NAMES: readonly PatternName[] = ["invoke", "value", "select", "toggle"];

export interface ExplainOptions {
  app?: string;
  windowId?: string;
  sessionId?: string;
  fixturePath?: string;
  goal?: string;
  maxCandidates?: number;
  timeoutMs?: number;
  client?: DcuClient;
}

/** A candidate as it leaves the module: no rectangles, no areas, no merge bookkeeping. */
export interface ExplainCandidate {
  optionId: string;
  elementIndex: number;
  role: CandidateRole;
  label: string;
  altLabel?: string;
  value?: string;
  stateWord?: string;
  place: string;
  regionId: string;
  ops: Operation[];
}

export interface ExplainResult {
  source: "fixture" | "window";
  window: {
    app?: string;
    title?: string;
    id?: string;
    width: number;
    height: number;
  };
  goal?: string;
  candidates: ExplainCandidate[];
  disabledShortlist: ExplainCandidate[];
  report: FilterReport;
}

/**
 * Korean glues the particle onto the noun with no space, so the goal carries
 * "스팸메일함을" where the label reads "스팸메일함". Longest first, so "에서" is
 * stripped before "서" could be.
 */
const KOREAN_PARTICLES: readonly string[] = [
  "에서", "으로", "에게", "한테", "까지", "부터", "보다", "처럼", "마다", "라도", "이나",
  "은", "는", "이", "가", "을", "를", "에", "로", "와", "과", "의", "도", "만", "나"
];

/** A stem shorter than this is noise, and a one-character token would match every label. */
const MIN_GOAL_TOKEN_CHARS = 2;

function stripKoreanParticle(token: string): string | undefined {
  for (const particle of KOREAN_PARTICLES) {
    if (!token.endsWith(particle)) continue;
    const stem = token.slice(0, token.length - particle.length);
    if (stem.length >= MIN_GOAL_TOKEN_CHARS) return stem;
  }
  return undefined;
}

/**
 * Both the stem and the original survive: the stem is what a label matches, the
 * original still carries the particle for goals that name it verbatim.
 */
export function goalTokens(goal: string | undefined): string[] {
  if (!goal) return [];
  const tokens: string[] = [];
  for (const raw of normalizeLabel(goal).split(/[^\p{L}\p{N}]+/u)) {
    const token = normalizeLabel(raw);
    if (token.length < MIN_GOAL_TOKEN_CHARS) continue;
    tokens.push(token);
    const stem = stripKoreanParticle(token);
    if (stem !== undefined) tokens.push(stem);
  }
  return Array.from(new Set(tokens));
}

export function serializeCandidate(candidate: Candidate): ExplainCandidate {
  const serialized: ExplainCandidate = {
    optionId: candidate.optionId,
    elementIndex: candidate.elementIndex,
    role: candidate.role,
    label: candidate.label,
    place: candidate.place,
    regionId: candidate.regionId,
    ops: candidate.ops
  };
  if (candidate.altLabel !== undefined) serialized.altLabel = candidate.altLabel;
  if (candidate.value !== undefined) serialized.value = candidate.value;
  if (candidate.stateWord !== undefined) serialized.stateWord = candidate.stateWord;
  return serialized;
}

function asObject(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function num(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function str(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function readElement(value: unknown, position: number): UiaElement {
  const raw = asObject(value) ?? {};
  const bounds = asObject(raw.bounds) ?? {};
  const patterns = Array.isArray(raw.patterns)
    ? raw.patterns.filter((pattern): pattern is PatternName => PATTERN_NAMES.includes(pattern as PatternName))
    : [];
  const element: UiaElement = {
    index: Number.isInteger(raw.index) ? raw.index as number : position,
    name: str(raw.name),
    controlType: str(raw.controlType) || "Unknown",
    automationId: str(raw.automationId),
    className: str(raw.className),
    enabled: raw.enabled !== false,
    offscreen: raw.offscreen === true,
    focused: raw.focused === true,
    bounds: {
      x: num(bounds.x),
      y: num(bounds.y),
      width: num(bounds.width),
      height: num(bounds.height)
    },
    patterns
  };
  if (typeof raw.value === "string") element.value = raw.value;
  if (Array.isArray(raw.runtimeId)) {
    element.runtimeId = raw.runtimeId.filter((part): part is number => typeof part === "number");
  }
  return element;
}

/**
 * Accepts a `get-app-state` result or the full `{ id, ok, result }` envelope a
 * recorded observation may have been saved as.
 */
export function readObservation(value: unknown): Observation {
  const envelope = asObject(value);
  const source = envelope && asObject(envelope.result) ? asObject(envelope.result) : envelope;
  const windowValue = asObject(source?.window);
  const accessibility = asObject(source?.accessibility);
  if (!windowValue || !accessibility || !Array.isArray(accessibility.elements)) {
    throw new DcuError("invalid_observation", "Expected an observation with window and accessibility.elements");
  }
  const observation: Observation = {
    window: {
      width: num(windowValue.width),
      height: num(windowValue.height)
    },
    accessibility: {
      elements: accessibility.elements.map((element, position) => readElement(element, position))
    }
  };
  if (typeof windowValue.app === "string") observation.window.app = windowValue.app;
  if (typeof windowValue.title === "string") observation.window.title = windowValue.title;
  if (typeof windowValue.id === "string") observation.window.id = windowValue.id;
  if (typeof accessibility.elementCount === "number") {
    observation.accessibility.elementCount = accessibility.elementCount;
  }
  return observation;
}

async function readFixture(path: string): Promise<Observation> {
  let text: string;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    throw new DcuError("invalid_argument", `Cannot read fixture ${path}: ${(error as Error).message}`);
  }
  try {
    return readObservation(JSON.parse(text));
  } catch (error) {
    if (error instanceof DcuError) throw error;
    throw new DcuError("invalid_argument", `Fixture ${path} is not valid JSON: ${(error as Error).message}`);
  }
}

async function observeWindow(options: ExplainOptions): Promise<Observation> {
  const client = options.client ?? new DcuClient();
  const params: JsonObject = { includeText: true, includeScreenshot: false };
  if (options.app !== undefined) params.app = options.app;
  if (options.windowId !== undefined) params.windowId = options.windowId;
  if (options.sessionId !== undefined) params.sessionId = options.sessionId;
  const response = await client.request("get-app-state", params, { timeoutMs: options.timeoutMs });
  if (!response.ok) throw nativeError(response.error);
  return readObservation(response.result);
}

export async function explain(options: ExplainOptions): Promise<ExplainResult> {
  if (!options.fixturePath && options.app === undefined && options.windowId === undefined) {
    throw new DcuError("invalid_argument", "agent explain requires --app, --window-id, or --fixture");
  }
  const observation = options.fixturePath
    ? await readFixture(options.fixturePath)
    : await observeWindow(options);
  const filtered = buildCandidates(observation, {
    ...(options.maxCandidates === undefined ? {} : { maxCandidates: options.maxCandidates }),
    goalTokens: goalTokens(options.goal)
  });
  const result: ExplainResult = {
    source: options.fixturePath ? "fixture" : "window",
    window: {
      width: observation.window.width,
      height: observation.window.height
    },
    candidates: filtered.candidates.map(serializeCandidate),
    disabledShortlist: filtered.disabledShortlist.map(serializeCandidate),
    report: filtered.report
  };
  if (observation.window.app !== undefined) result.window.app = observation.window.app;
  if (observation.window.title !== undefined) result.window.title = observation.window.title;
  if (observation.window.id !== undefined) result.window.id = observation.window.id;
  if (options.goal !== undefined) result.goal = options.goal;
  return result;
}

const WIDE_CHARACTER =
  /[ᄀ-ᅟ⺀-꓏ꥠ-꥿가-힣豈-﫿︐-︙︰-﹯＀-｠￠-￦]/u;

function displayWidth(text: string): number {
  let width = 0;
  for (const character of text) width += WIDE_CHARACTER.test(character) ? 2 : 1;
  return width;
}

function clip(text: string, width: number): string {
  if (displayWidth(text) <= width) return text;
  let clipped = "";
  let used = 0;
  for (const character of text) {
    const size = WIDE_CHARACTER.test(character) ? 2 : 1;
    if (used + size > width - 1) break;
    clipped += character;
    used += size;
  }
  return `${clipped}…`;
}

function pad(text: string, width: number): string {
  const gap = width - displayWidth(text);
  return gap > 0 ? text + " ".repeat(gap) : text;
}

function describeCandidate(candidate: ExplainCandidate): string {
  const parts = [candidate.label];
  if (candidate.value) parts.push(`= ${candidate.value}`);
  if (candidate.stateWord) parts.push(`(${candidate.stateWord})`);
  if (candidate.altLabel) parts.push(`[${candidate.altLabel}]`);
  return parts.join(" ");
}

/** Human rendering for `--pretty`; the JSON envelope stays the machine contract. */
export function renderTable(result: ExplainResult): string {
  const lines: string[] = [];
  const title = [result.window.app, result.window.title].filter(Boolean).join(" — ");
  lines.push(title || "(unknown window)");
  if (result.goal) lines.push(`goal: ${result.goal}`);
  lines.push("");
  const columns = [10, 10, 52, 30, 22];
  const header = ["option", "role", "label", "place", "ops"];
  lines.push(header.map((cell, index) => pad(cell, columns[index])).join(" "));
  lines.push(columns.map(width => "-".repeat(width)).join(" "));
  for (const candidate of result.candidates) {
    const cells = [
      candidate.optionId,
      candidate.role,
      clip(describeCandidate(candidate), columns[2]),
      clip(candidate.place, columns[3]),
      candidate.ops.join(",") || "—"
    ];
    lines.push(cells.map((cell, index) => pad(cell, columns[index])).join(" ").trimEnd());
  }
  if (result.disabledShortlist.length > 0) {
    lines.push("");
    lines.push("disabled (not actionable right now):");
    for (const candidate of result.disabledShortlist) {
      lines.push(`  ${pad(candidate.optionId, 8)} ${clip(describeCandidate(candidate), 52)}`);
    }
  }
  lines.push("");
  lines.push("regions:");
  for (const region of result.report.regions) {
    lines.push(`  ${pad(region.regionId, 8)} ${pad(clip(region.label, 32), 32)} ${region.place} (${region.candidateCount})`);
  }
  lines.push("");
  const dropped = Object.entries(result.report.dropped)
    .filter(([, count]) => count > 0)
    .map(([reason, count]) => `${reason} ${count}`)
    .join(", ");
  lines.push(`raw ${result.report.rawCount} -> kept ${result.report.kept}` +
    `${result.report.overflow ? " (overflow)" : ""} in ${result.report.elapsedMs} ms`);
  lines.push(`dropped: ${dropped || "none"}`);
  return lines.join("\n");
}

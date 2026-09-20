import {
  MAIN_REGION_ID,
  area,
  buildRegions,
  center,
  contains,
  describePlace,
  smallestContainingRegion,
  type Region
} from "./regions.js";
import {
  DROP_REASONS,
  type Candidate,
  type CandidateRole,
  type DropReason,
  type ElementBounds,
  type FilterOptions,
  type FilterReport,
  type FilterResult,
  type Observation,
  type ObservationWindow,
  type Operation,
  type PatternName,
  type UiaElement
} from "./types.js";

const DEFAULT_MAX_CANDIDATES = 120;
const DEFAULT_MAX_LABEL_CHARS = 80;
const DEFAULT_MAX_VALUE_CHARS = 60;
const MAX_DISABLED_SHORTLIST = 8;
/**
 * Stage G compares rects inside a label bucket. Buckets larger than this are
 * repeated rows (list items), not wrapper chains, so collapsing them would only
 * pay the quadratic cost.
 */
const COLLAPSE_BUCKET_LIMIT = 64;
const CROWDED_LIST_ITEMS = 50;

const ROLE_BY_CONTROL_TYPE = new Map<string, CandidateRole>([
  ["Button", "button"],
  ["CheckBox", "checkbox"],
  ["ComboBox", "dropdown"],
  ["Edit", "textfield"],
  ["Hyperlink", "link"],
  ["MenuItem", "menuitem"],
  ["RadioButton", "radio"],
  ["Slider", "slider"],
  ["TabItem", "tab"],
  ["ListItem", "listitem"],
  ["TreeItem", "treeitem"]
]);

/** Never a click target, but may become a scroll region. */
const CONTAINER_TYPES = new Set(["Window", "Menu", "List", "Tree", "Tab", "Pane"]);
const EDITABLE_ROLES = new Set<CandidateRole>(["textfield", "dropdown"]);
const CLICKABLE_ROLES = new Set<CandidateRole>(["button", "link", "menuitem", "tab"]);
const SELECTABLE_ROLES = new Set<CandidateRole>(["tab", "listitem", "treeitem"]);
const CHECKED_ROLES = new Set<CandidateRole>(["toggle", "checkbox", "radio"]);
const ALL_PATTERNS: readonly PatternName[] = ["invoke", "value", "select", "toggle"];
const ON_WORDS = new Set(["on", "true", "1", "checked", "켜짐"]);

interface Settings {
  maxCandidates: number;
  maxLabelChars: number;
  maxValueChars: number;
  includeOffscreen: boolean;
  goalTokens: readonly string[];
}

interface Working extends Candidate {
  bounds: ElementBounds;
  normalizedLabel: string;
  focused: boolean;
  patternCount: number;
  order: number;
}

function cleanText(raw: string, maxChars: number): string {
  const collapsed = raw.replace(/[\x00-\x1f\x7f]+/gu, " ").replace(/\s+/gu, " ").trim();
  if (collapsed.length <= maxChars) return collapsed;
  return `${collapsed.slice(0, Math.max(0, maxChars - 1)).trimEnd()}…`;
}

export function normalizeLabel(text: string): string {
  return text
    .normalize("NFKC")
    .toLowerCase()
    .replace(/&&/gu, "")
    .replace(/&/gu, "")
    .replace(//gu, "&")
    .replace(/\s+/gu, " ")
    .trim()
    .replace(/(?:\.\.\.|…)+$/u, "")
    .trim();
}

/** "SearchBoxTextBlock" -> "search box text block". */
export function humanizeAutomationId(automationId: string): string {
  return automationId
    .replace(/[_\-.]+/gu, " ")
    .replace(/([a-z0-9])([A-Z])/gu, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/gu, "$1 $2")
    .replace(/([A-Za-z])(\d)/gu, "$1 $2")
    .replace(/\s+/gu, " ")
    .trim()
    .toLowerCase();
}

function isAscii(value: string): boolean {
  return /^[\x20-\x7e]+$/u.test(value);
}

/**
 * Automation ids are only worth showing when they read as words. Chromium and
 * WinUI hand out generated handles ("view 1001", "j 8 ksar n cuavse") that add
 * noise instead of an English grip on the control.
 */
function looksLikeWords(text: string): boolean {
  const tokens = text.split(" ").filter(token => token.length > 0);
  if (tokens.length === 0 || tokens.length > 6) return false;
  const digits = (text.match(/\d/gu) ?? []).length;
  if (digits / text.length > 0.2) return false;
  const words = tokens.filter(token => /^[a-z]{3,}$/u.test(token));
  if (words.length >= 2) return true;
  // A lone word next to a bare number is a generated handle ("view 1"), not a name.
  return words.some(token => token.length >= 4) && !tokens.some(token => /^\d+$/u.test(token));
}

function settingsFrom(options: FilterOptions | undefined): Settings {
  return {
    maxCandidates: options?.maxCandidates ?? DEFAULT_MAX_CANDIDATES,
    maxLabelChars: options?.maxLabelChars ?? DEFAULT_MAX_LABEL_CHARS,
    maxValueChars: options?.maxValueChars ?? DEFAULT_MAX_VALUE_CHARS,
    includeOffscreen: options?.includeOffscreen ?? false,
    goalTokens: goalTokensOf(options?.goalTokens)
  };
}

function goalTokensOf(tokens: readonly string[] | undefined): readonly string[] {
  if (!tokens) return [];
  const normalized = tokens.map(token => normalizeLabel(token)).filter(token => token.length > 0);
  return Array.from(new Set(normalized));
}

function emptyDrops(): Record<DropReason, number> {
  const dropped = {} as Record<DropReason, number>;
  for (const reason of DROP_REASONS) dropped[reason] = 0;
  return dropped;
}

/**
 * `uia.cpp` probes patterns with `GetCachedPattern`, which succeeds even when the
 * pattern is absent, so every element can claim every pattern. When that happens
 * the pattern list carries no information and roles alone decide the operations.
 */
function patternsAreInformative(elements: readonly UiaElement[]): boolean {
  if (elements.length === 0) return false;
  return !elements.every(element => ALL_PATTERNS.every(pattern => element.patterns?.includes(pattern)));
}

interface Naming {
  label: string;
  altLabel?: string;
}

function nameElement(element: UiaElement, settings: Settings): Naming | undefined {
  const name = cleanText(element.name ?? "", settings.maxLabelChars);
  const automationId = (element.automationId ?? "").trim();
  const value = cleanText(element.value ?? "", settings.maxLabelChars);
  if (!name && !automationId && !value) return undefined;
  const humanized = automationId ? humanizeAutomationId(automationId) : "";
  const label = name || humanized || value;
  if (!label) return undefined;
  const carriesAltLabel =
    name.length > 0 &&
    humanized.length > 0 &&
    isAscii(automationId) &&
    looksLikeWords(humanized) &&
    normalizeLabel(humanized) !== normalizeLabel(name);
  const naming: Naming = { label: cleanText(label, settings.maxLabelChars) };
  if (carriesAltLabel) naming.altLabel = cleanText(humanized, settings.maxLabelChars);
  return naming;
}

function conditionalRole(element: UiaElement, informative: boolean): CandidateRole {
  const has = (pattern: PatternName): boolean => informative && element.patterns.includes(pattern);
  if (has("toggle") && !has("invoke")) return "toggle";
  if (has("select") && !has("invoke")) return "listitem";
  return "button";
}

function roleFor(element: UiaElement, informative: boolean): CandidateRole | undefined {
  const declared = ROLE_BY_CONTROL_TYPE.get(element.controlType);
  if (declared === "button" && informative && element.patterns.includes("toggle") && !element.patterns.includes("invoke")) {
    return "toggle";
  }
  return declared;
}

function operationsFor(role: CandidateRole, element: UiaElement, informative: boolean): Operation[] {
  if (role === "scrollable") return ["scroll"];
  const has = (pattern: PatternName): boolean => !informative || element.patterns.includes(pattern);
  if (EDITABLE_ROLES.has(role)) {
    return has("value") ? ["set_value", "type_text", "click"] : ["type_text", "click"];
  }
  if (has("invoke") || has("select") || has("toggle") || CLICKABLE_ROLES.has(role)) return ["click"];
  // A value pattern on a non-editable role offers nothing to do; the value still
  // informs the label.
  return [];
}

function stateWordFor(role: CandidateRole, element: UiaElement, value: string | undefined): string | undefined {
  if (EDITABLE_ROLES.has(role) && !value) return "empty";
  if (CHECKED_ROLES.has(role) && value && ON_WORDS.has(normalizeLabel(value))) return "currently on";
  if (element.focused && SELECTABLE_ROLES.has(role)) return "selected";
  return undefined;
}

function dedupeKey(candidate: Working): string {
  const { bounds } = candidate;
  const rect = [bounds.x, bounds.y, bounds.width, bounds.height].map(part => Math.round(part / 4)).join(",");
  return `${candidate.role}|${candidate.normalizedLabel}|${rect}`;
}

function mergeInto(winner: Working, loser: Working): void {
  winner.mergedIndexes.push(loser.elementIndex, ...loser.mergedIndexes);
}

/** Below this a token is a syllable, not a word, and would overlap every label. */
const MIN_OVERLAP_CHARS = 2;

function labelTokensOf(candidate: Working): string[] {
  const text = `${candidate.normalizedLabel} ${candidate.altLabel ? normalizeLabel(candidate.altLabel) : ""}`;
  return text.split(/[^\p{L}\p{N}]+/u).filter(token => token.length > 0);
}

/**
 * Containment both ways, because neither side is reliably the longer one: the
 * goal says "스팸메일함을" where the label says "스팸메일함", and the goal says
 * "bluetooth" where the label says "Bluetooth settings".
 */
function overlaps(left: string, right: string): boolean {
  const shorter = left.length <= right.length ? left : right;
  const longer = left.length <= right.length ? right : left;
  return shorter.length >= MIN_OVERLAP_CHARS && longer.includes(shorter);
}

function matchesGoal(candidate: Working, goalTokens: readonly string[]): boolean {
  if (goalTokens.length === 0) return false;
  const labelTokens = labelTokensOf(candidate);
  return goalTokens.some(goalToken =>
    goalToken.length >= MIN_OVERLAP_CHARS &&
    labelTokens.some(labelToken => overlaps(goalToken, labelToken)));
}

function salience(candidate: Working, settings: Settings, crowdedRegions: ReadonlySet<string>): number {
  let score = 0;
  if (matchesGoal(candidate, settings.goalTokens)) score += 3;
  if (candidate.focused) score += 2;
  if (candidate.role === "button" || candidate.role === "textfield" || candidate.role === "menuitem" || candidate.role === "tab") {
    score += 2;
  }
  if (candidate.area >= 400 && candidate.area <= 40_000) score += 1;
  if (candidate.role === "listitem" && crowdedRegions.has(candidate.regionId)) score -= 2;
  return score;
}

function crowdedRegionsOf(candidates: readonly Working[]): Set<string> {
  const counts = new Map<string, number>();
  for (const candidate of candidates) {
    if (candidate.role !== "listitem") continue;
    counts.set(candidate.regionId, (counts.get(candidate.regionId) ?? 0) + 1);
  }
  const crowded = new Set<string>();
  for (const [regionId, count] of counts) {
    if (count > CROWDED_LIST_ITEMS) crowded.add(regionId);
  }
  return crowded;
}

function toCandidate(working: Working): Candidate {
  const candidate: Candidate = {
    optionId: working.optionId,
    elementIndex: working.elementIndex,
    role: working.role,
    label: working.label,
    place: working.place,
    regionId: working.regionId,
    ops: working.ops,
    center: working.center,
    area: working.area,
    mergedIndexes: [...working.mergedIndexes].sort((left, right) => left - right)
  };
  if (working.altLabel !== undefined) candidate.altLabel = working.altLabel;
  if (working.value !== undefined) candidate.value = working.value;
  if (working.stateWord !== undefined) candidate.stateWord = working.stateWord;
  return candidate;
}

export function buildCandidates(observation: Observation, options?: FilterOptions): FilterResult {
  const startedAt = performance.now();
  const settings = settingsFrom(options);
  const window: ObservationWindow = {
    width: Math.max(1, Math.round(observation.window?.width ?? 0)),
    height: Math.max(1, Math.round(observation.window?.height ?? 0))
  };
  const elements = observation.accessibility?.elements ?? [];
  const dropped = emptyDrops();
  const informative = patternsAreInformative(elements);

  // A. Visibility. Bounds are window-local, so a negative edge means scrolled off.
  const visible: UiaElement[] = [];
  for (const element of elements) {
    const bounds = element.bounds;
    if (element.offscreen === true && !settings.includeOffscreen) {
      dropped.offscreen += 1;
      continue;
    }
    if (!bounds || bounds.width <= 0 || bounds.height <= 0) {
      dropped.empty_bounds += 1;
      continue;
    }
    if (
      bounds.x + bounds.width <= 0 ||
      bounds.y + bounds.height <= 0 ||
      bounds.x >= window.width ||
      bounds.y >= window.height
    ) {
      dropped.outside_window += 1;
      continue;
    }
    visible.push(element);
  }

  // E. Scroll regions, reconstructed from rectangles alone.
  const regions = buildRegions(visible, window, element => nameElement(element, settings)?.label);
  const regionByElement = new Map<number, Region>();
  for (const region of regions) {
    if (region.elementIndex !== undefined) regionByElement.set(region.elementIndex, region);
  }

  const working: Working[] = [];
  const disabled: Working[] = [];
  for (const element of visible) {
    // B. Type and pattern.
    if (element.controlType === "ScrollBar") {
      dropped.scroll_bar += 1;
      continue;
    }
    const asRegion = regionByElement.get(element.index);
    let role: CandidateRole;
    if (asRegion) {
      role = "scrollable";
    } else {
      const declared = roleFor(element, informative);
      if (declared) {
        role = declared;
      } else if (CONTAINER_TYPES.has(element.controlType)) {
        dropped.container += 1;
        continue;
      } else {
        // Text, Image and every unrecognised ControlType_NNNNN — where the
        // Chromium and Electron nodes land. Kept only when there is something to
        // act on and something to show.
        const carriesPattern = !informative || element.patterns.some(pattern => ALL_PATTERNS.includes(pattern));
        const carriesText = Boolean(element.name) || Boolean(element.value);
        if (!carriesPattern || !carriesText) {
          dropped.inert += 1;
          continue;
        }
        role = conditionalRole(element, informative);
      }
    }

    // D. Naming.
    const naming = nameElement(element, settings) ?? (asRegion ? { label: asRegion.label } : undefined);

    // C. Disabled controls leave the action set; the goal picks the ones worth mentioning.
    if (element.enabled === false) {
      dropped.disabled += 1;
      if (naming) disabled.push(makeWorking(element, role, naming, regions, window, settings, informative));
      continue;
    }
    if (!naming) {
      dropped.unnamed += 1;
      continue;
    }
    working.push(makeWorking(element, role, naming, regions, window, settings, informative));
  }

  // F. Deduplicate.
  const byKey = new Map<string, Working>();
  const deduped: Working[] = [];
  for (const candidate of working) {
    const key = dedupeKey(candidate);
    const existing = byKey.get(key);
    if (existing) {
      mergeInto(existing, candidate);
      dropped.duplicate += 1;
      continue;
    }
    byKey.set(key, candidate);
    deduped.push(candidate);
  }

  // G. Parent/child collapse, bucketed by label so the pass stays linear in practice.
  const collapsed = collapseWrappers(deduped, dropped);

  // H. Reading order.
  const bandHeight = Math.max(1, window.height / 12);
  for (const candidate of collapsed) candidate.order = Math.floor(candidate.center.y / bandHeight);
  collapsed.sort((left, right) =>
    left.order - right.order ||
    left.center.x - right.center.x ||
    left.elementIndex - right.elementIndex);
  collapsed.forEach((candidate, position) => {
    candidate.order = position;
  });

  let kept = collapsed;
  let overflow = false;
  if (collapsed.length > settings.maxCandidates) {
    overflow = true;
    const crowded = crowdedRegionsOf(collapsed);
    const ranked = [...collapsed].sort((left, right) =>
      salience(right, settings, crowded) - salience(left, settings, crowded) ||
      left.order - right.order);
    const survivors = new Set(ranked.slice(0, settings.maxCandidates).map(candidate => candidate.elementIndex));
    dropped.over_cap += collapsed.length - survivors.size;
    kept = collapsed.filter(candidate => survivors.has(candidate.elementIndex));
  }

  const candidates = kept.map(toCandidate);
  const shortlist = shortlistDisabled(disabled, settings);
  const report: FilterReport = {
    rawCount: elements.length,
    kept: candidates.length,
    overflow,
    elapsedMs: Math.round((performance.now() - startedAt) * 1000) / 1000,
    regions: summarizeRegions(regions, kept),
    dropped
  };
  return { candidates, disabledShortlist: shortlist, report };
}

function makeWorking(
  element: UiaElement,
  role: CandidateRole,
  naming: Naming,
  regions: readonly Region[],
  window: ObservationWindow,
  settings: Settings,
  informative: boolean
): Working {
  const point = center(element.bounds);
  const region = smallestContainingRegion(regions, point, element.index);
  const rawValue = cleanText(element.value ?? "", settings.maxValueChars);
  const value = rawValue && normalizeLabel(rawValue) !== normalizeLabel(naming.label) ? rawValue : undefined;
  const stateWord = stateWordFor(role, element, rawValue || undefined);
  const candidate: Working = {
    optionId: `e${element.index}`,
    elementIndex: element.index,
    role,
    label: naming.label,
    place: describePlace(element.bounds, window, region),
    regionId: region.regionId,
    ops: operationsFor(role, element, informative),
    center: { x: Math.round(point.x), y: Math.round(point.y) },
    area: area(element.bounds),
    mergedIndexes: [],
    bounds: element.bounds,
    normalizedLabel: normalizeLabel(naming.label),
    focused: element.focused === true,
    patternCount: element.patterns?.length ?? 0,
    order: element.index
  };
  if (naming.altLabel !== undefined) candidate.altLabel = naming.altLabel;
  if (value !== undefined) candidate.value = value;
  if (stateWord !== undefined) candidate.stateWord = stateWord;
  return candidate;
}

function collapseWrappers(candidates: readonly Working[], dropped: Record<DropReason, number>): Working[] {
  const buckets = new Map<string, Working[]>();
  for (const candidate of candidates) {
    const bucket = buckets.get(candidate.normalizedLabel);
    if (bucket) bucket.push(candidate);
    else buckets.set(candidate.normalizedLabel, [candidate]);
  }
  const removed = new Set<number>();
  for (const bucket of buckets.values()) {
    if (bucket.length < 2 || bucket.length > COLLAPSE_BUCKET_LIMIT) continue;
    for (let i = 0; i < bucket.length; i += 1) {
      const left = bucket[i];
      if (removed.has(left.elementIndex)) continue;
      for (let j = i + 1; j < bucket.length; j += 1) {
        const right = bucket[j];
        if (removed.has(right.elementIndex) || removed.has(left.elementIndex)) continue;
        const leftInRight = contains(right.bounds, left.bounds);
        const rightInLeft = contains(left.bounds, right.bounds);
        if (!leftInRight && !rightInLeft) continue;
        const inner = leftInRight ? left : right;
        const winner = chooseWinner(left, right, inner, leftInRight && rightInLeft);
        const loser = winner === left ? right : left;
        mergeInto(winner, loser);
        removed.add(loser.elementIndex);
        dropped.collapsed += 1;
      }
    }
  }
  return candidates.filter(candidate => !removed.has(candidate.elementIndex));
}

function chooseWinner(left: Working, right: Working, inner: Working, sameRect: boolean): Working {
  if (sameRect) return left.elementIndex <= right.elementIndex ? left : right;
  const leftActs = left.ops.length > 0;
  const rightActs = right.ops.length > 0;
  if (leftActs !== rightActs) return leftActs ? left : right;
  // Both actionable: the inner rect is the real hit target — invoke() on a
  // wrapper often no-ops.
  if (leftActs && rightActs) return inner;
  if (left.patternCount !== right.patternCount) return left.patternCount > right.patternCount ? left : right;
  return left.elementIndex <= right.elementIndex ? left : right;
}

function shortlistDisabled(disabled: readonly Working[], settings: Settings): Candidate[] {
  if (disabled.length === 0) return [];
  const ranked = [...disabled].sort((left, right) => {
    const leftGoal = matchesGoal(left, settings.goalTokens) ? 1 : 0;
    const rightGoal = matchesGoal(right, settings.goalTokens) ? 1 : 0;
    return rightGoal - leftGoal || left.elementIndex - right.elementIndex;
  });
  return ranked.slice(0, MAX_DISABLED_SHORTLIST).map(toCandidate);
}

function summarizeRegions(regions: readonly Region[], candidates: readonly Working[]): FilterReport["regions"] {
  const counts = new Map<string, number>();
  for (const candidate of candidates) {
    counts.set(candidate.regionId, (counts.get(candidate.regionId) ?? 0) + 1);
  }
  return regions.map(region => {
    const summary: FilterReport["regions"][number] = {
      regionId: region.regionId,
      label: region.label,
      place: region.regionId === MAIN_REGION_ID ? region.phrase : `in the ${region.phrase}`,
      candidateCount: counts.get(region.regionId) ?? 0
    };
    if (region.elementIndex !== undefined) summary.elementIndex = region.elementIndex;
    return summary;
  });
}

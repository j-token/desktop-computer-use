export type PatternName = "invoke" | "value" | "select" | "toggle";

export type Operation = "click" | "type_text" | "set_value" | "scroll";

export type CandidateRole =
  | "button" | "toggle" | "checkbox" | "radio" | "textfield" | "dropdown"
  | "link" | "menuitem" | "tab" | "listitem" | "treeitem" | "slider" | "scrollable";

export interface ElementBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface UiaElement {
  index: number;
  name: string;
  controlType: string;
  automationId: string;
  className: string;
  enabled: boolean;
  offscreen: boolean;
  focused: boolean;
  bounds: ElementBounds;
  value?: string;
  patterns: PatternName[];
  runtimeId?: number[];
}

export interface ObservationWindow {
  width: number;
  height: number;
  app?: string;
  title?: string;
  id?: string;
}

export interface Observation {
  window: ObservationWindow;
  accessibility: {
    elementCount?: number;
    tree?: string;
    elements: UiaElement[];
  };
}

export interface Candidate {
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
  // code-only, never serialized outward:
  center: { x: number; y: number };
  area: number;
  mergedIndexes: number[];
}

export type DropReason =
  | "offscreen"
  | "empty_bounds"
  | "outside_window"
  | "scroll_bar"
  | "container"
  | "inert"
  | "disabled"
  | "unnamed"
  | "duplicate"
  | "collapsed"
  | "over_cap";

export const DROP_REASONS: readonly DropReason[] = [
  "offscreen",
  "empty_bounds",
  "outside_window",
  "scroll_bar",
  "container",
  "inert",
  "disabled",
  "unnamed",
  "duplicate",
  "collapsed",
  "over_cap"
];

export interface FilterOptions {
  maxCandidates?: number;
  maxLabelChars?: number;
  maxValueChars?: number;
  includeOffscreen?: boolean;
  goalTokens?: readonly string[];
}

export interface RegionSummary {
  regionId: string;
  label: string;
  place: string;
  elementIndex?: number;
  candidateCount: number;
}

export interface FilterReport {
  rawCount: number;
  kept: number;
  overflow: boolean;
  elapsedMs: number;
  regions: RegionSummary[];
  dropped: Record<DropReason, number>;
}

export interface FilterResult {
  candidates: Candidate[];
  disabledShortlist: Candidate[];
  report: FilterReport;
}

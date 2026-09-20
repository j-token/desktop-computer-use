import type { ElementBounds, ObservationWindow, UiaElement } from "./types.js";

export const MAIN_REGION_ID = "r_main";

/** Rectangles are compared with a small tolerance: UIA reports sub-pixel edges. */
export const CONTAINMENT_TOLERANCE = 2;

const MAX_REGIONS = 6;
const MIN_REGION_SHARE = 0.12;
/** A rect this close to the window is the window itself, not a region inside it. */
const WHOLE_WINDOW_SHARE = 0.9;

/**
 * Types that can carry a scroll viewport. `uia.cpp` has no case for the Document
 * control type, so it reaches us as the raw `ControlType_50030`.
 */
const REGION_NOUNS = new Map<string, string>([
  ["List", "list"],
  ["Tree", "tree"],
  ["Pane", "pane"],
  ["Document", "document area"],
  ["ControlType_50030", "document area"],
  ["Window", "window"]
]);

export interface Region {
  regionId: string;
  elementIndex?: number;
  controlType: string;
  bounds: ElementBounds;
  area: number;
  /** Words only — "left navigation list", never a rectangle. */
  phrase: string;
  label: string;
}

export interface Point {
  x: number;
  y: number;
}

export function area(bounds: ElementBounds): number {
  return Math.max(0, bounds.width) * Math.max(0, bounds.height);
}

export function center(bounds: ElementBounds): Point {
  return { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2 };
}

export function contains(outer: ElementBounds, inner: ElementBounds, tolerance = CONTAINMENT_TOLERANCE): boolean {
  return (
    outer.x - tolerance <= inner.x &&
    outer.y - tolerance <= inner.y &&
    outer.x + outer.width + tolerance >= inner.x + inner.width &&
    outer.y + outer.height + tolerance >= inner.y + inner.height
  );
}

export function containsPoint(bounds: ElementBounds, point: Point, tolerance = CONTAINMENT_TOLERANCE): boolean {
  return (
    point.x >= bounds.x - tolerance &&
    point.y >= bounds.y - tolerance &&
    point.x <= bounds.x + bounds.width + tolerance &&
    point.y <= bounds.y + bounds.height + tolerance
  );
}

function horizontalWord(point: Point, window: ObservationWindow): string {
  const share = point.x / Math.max(1, window.width);
  if (share < 1 / 3) return "left";
  if (share > 2 / 3) return "right";
  return "center";
}

function verticalWord(point: Point, window: ObservationWindow): string {
  const share = point.y / Math.max(1, window.height);
  if (share < 1 / 3) return "top";
  if (share > 2 / 3) return "bottom";
  return "middle";
}

/** Thirds of the window, in words. Never a number. */
export function thirds(point: Point, window: ObservationWindow): string {
  return `${verticalWord(point, window)}-${horizontalWord(point, window)}`;
}

function regionPhrase(bounds: ElementBounds, controlType: string, window: ObservationWindow): string {
  const noun = REGION_NOUNS.get(controlType) ?? "pane";
  const point = center(bounds);
  const widthShare = bounds.width / Math.max(1, window.width);
  if (widthShare >= 0.7) return `${verticalWord(point, window)} ${noun}`;
  const side = horizontalWord(point, window);
  if (side === "left" && (noun === "list" || noun === "tree")) return `left navigation ${noun}`;
  return `${side} ${noun}`;
}

/**
 * The daemon emits a flat element list with no parent link, so containment is
 * reconstructed from rectangles: the smallest rect that strictly contains this
 * one wins. `regions` must be sorted by area ascending.
 */
export function smallestContainingRegion(
  regions: readonly Region[],
  point: Point,
  skipElementIndex?: number
): Region {
  for (const region of regions) {
    if (region.elementIndex !== undefined && region.elementIndex === skipElementIndex) continue;
    if (containsPoint(region.bounds, point)) return region;
  }
  return regions[regions.length - 1];
}

function rectKey(bounds: ElementBounds): string {
  return `${Math.round(bounds.x / 4)},${Math.round(bounds.y / 4)},${Math.round(bounds.width / 4)},${Math.round(bounds.height / 4)}`;
}

/**
 * Scroll regions, smallest first, always ending with the synthesized whole-window
 * region so that every point lands somewhere.
 */
export function buildRegions(
  elements: readonly UiaElement[],
  window: ObservationWindow,
  labelFor: (element: UiaElement) => string | undefined
): Region[] {
  const windowArea = Math.max(1, window.width * window.height);
  const seen = new Set<string>();
  const picked: Region[] = [];
  const eligible = elements
    .filter(element => REGION_NOUNS.has(element.controlType) && element.enabled !== false)
    .filter(element => {
      const share = area(element.bounds) / windowArea;
      return share >= MIN_REGION_SHARE && share < WHOLE_WINDOW_SHARE;
    })
    .sort((left, right) => area(right.bounds) - area(left.bounds) || left.index - right.index);

  for (const element of eligible) {
    if (picked.length >= MAX_REGIONS) break;
    const key = rectKey(element.bounds);
    if (seen.has(key)) continue;
    seen.add(key);
    const phrase = regionPhrase(element.bounds, element.controlType, window);
    picked.push({
      regionId: `r${element.index}`,
      elementIndex: element.index,
      controlType: element.controlType,
      bounds: element.bounds,
      area: area(element.bounds),
      phrase,
      label: labelFor(element) ?? phrase
    });
  }

  const main: Region = {
    regionId: MAIN_REGION_ID,
    controlType: "Window",
    bounds: { x: 0, y: 0, width: window.width, height: window.height },
    area: windowArea,
    phrase: "main window area",
    label: "main window area"
  };
  picked.sort((left, right) => left.area - right.area || left.regionId.localeCompare(right.regionId));
  picked.push(main);
  return picked;
}

/** Words for where a control sits. Rectangles never leave this module. */
export function describePlace(
  bounds: ElementBounds,
  window: ObservationWindow,
  region: Region
): string {
  const point = center(bounds);
  const inTitleBar = point.y <= window.height * 0.08 && bounds.height < 60;
  if (inTitleBar) return "in the title bar";
  if (region.regionId !== MAIN_REGION_ID) return `in the ${region.phrase}`;
  return thirds(point, window);
}

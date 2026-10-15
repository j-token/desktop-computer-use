// LLM 하네스가 읽는 응답을 줄이는 압축 계층입니다. 네이티브 계약은 그대로 두고,
// 판단에 쓰이지 않는 진단 필드를 버리고 창·요소 레코드를 한 줄 문자열로 바꿉니다.
// 원본이 필요하면 CLI의 --raw를 사용합니다.

type Rec = Record<string, unknown>;

const TEXT_LIMIT = 300;

function isRecord(value: unknown): value is Rec {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function num(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function str(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function clip(text: string): string {
  const flat = text.replace(/\s+/gu, " ").trim();
  return flat.length > TEXT_LIMIT ? `${flat.slice(0, TEXT_LIMIT)}…(${flat.length})` : flat;
}

function quote(text: string): string {
  return JSON.stringify(clip(text));
}

function rect(value: unknown): string | undefined {
  if (!isRecord(value)) return undefined;
  const x = num(value.x), y = num(value.y), w = num(value.width), h = num(value.height);
  if (x === undefined || y === undefined || w === undefined || h === undefined) return undefined;
  return `${x},${y} ${w}x${h}`;
}

function hasArea(value: unknown): boolean {
  return isRecord(value) && (num(value.width) ?? 0) > 0 && (num(value.height) ?? 0) > 0;
}

/** `hwnd:1 "Title" app=x 10,20 800x600 fg` 형태의 창 한 줄. */
export function windowLine(window: Rec, includeApp = true): string {
  const parts = [str(window.id)];
  if (window.title) parts.push(quote(str(window.title)));
  if (includeApp && window.app) parts.push(`app=${str(window.app)}`);
  const bounds = rect(window);
  if (bounds) parts.push(bounds);
  if (window.isForeground === true) parts.push("fg");
  if (window.isMinimized === true) parts.push("min");
  if (window.isOffscreen === true) parts.push("offscreen");
  if (window.isEnabled === false) parts.push("disabled");
  if (window.ownerWindowId) parts.push(`owner=${str(window.ownerWindowId)}`);
  if (window.modal === true) parts.push("modal");
  return parts.join(" ");
}

function isWindowRecord(value: unknown): value is Rec {
  return isRecord(value) && typeof value.id === "string" && "title" in value && "width" in value;
}

/**
 * `12 Button "OK" 10,20 80x24 invoke` 형태의 요소 한 줄. 행동할 수 없는 요소
 * (면적·패턴·포커스가 모두 없음)는 undefined를 반환해 목록에서 뺍니다.
 */
export function elementLine(element: Rec): string | undefined {
  const bounds = element.bounds ?? element.frame;
  const actions = [
    ...(Array.isArray(element.patterns) ? element.patterns : []),
    ...(Array.isArray(element.actions) ? element.actions : [])
  ].map(String);
  const focused = element.focused === true;
  if (!hasArea(bounds) && actions.length === 0 && !focused) return undefined;

  const name = str(element.name);
  const value = str(element.value);
  const parts = [String(element.index), str(element.controlType) || "?"];
  if (name) parts.push(quote(name));
  else if (element.automationId) parts.push(`#${str(element.automationId)}`);
  if (value && value !== name) parts.push(`=${quote(value)}`);
  const area = rect(bounds);
  if (area && hasArea(bounds)) parts.push(area);
  if (actions.length > 0) parts.push(actions.join(","));
  if (focused) parts.push("focused");
  if (element.enabled === false) parts.push("disabled");
  if (element.offscreen === true) parts.push("offscreen");
  if (element.isSelected === true) parts.push("selected");
  return parts.join(" ");
}

function compactAccessibility(value: Rec): Rec {
  const elements = Array.isArray(value.elements) ? value.elements : [];
  const lines: string[] = [];
  for (const element of elements) {
    const line = isRecord(element) ? elementLine(element) : undefined;
    if (line) lines.push(line);
  }
  const out: Rec = { elements: lines };
  const hidden = elements.length - lines.length;
  if (hidden > 0) out.hidden = hidden;
  if (value.truncated === true) out.truncated = true;
  return out;
}

function compactScreenshot(value: Rec): Rec {
  const out: Rec = { path: value.path, width: value.width, height: value.height };
  if (value.mimeType && value.mimeType !== "image/jpeg") out.mimeType = value.mimeType;
  const fullWidth = num(value.fullWidth);
  if (fullWidth !== undefined && fullWidth !== value.width) {
    out.full = `${fullWidth}x${num(value.fullHeight) ?? "?"}`;
  }
  if (value.variant === "full") out.variant = "full";
  return out;
}

const DROPPED_KEYS = new Set([
  "timings", "capturedAt", "observedAt", "runtimeId", "tree",
  // CLI와 MCP가 원본 응답에서 세션 파일에 저장하므로 LLM에게는 반복되는 잡음입니다.
  "sessionId"
]);

function compactValue(value: unknown, key = ""): unknown {
  if (Array.isArray(value)) {
    if (key === "windows" && value.every(isWindowRecord)) {
      return value.map(window => windowLine(window));
    }
    return value.map(item => compactValue(item));
  }
  if (!isRecord(value)) return value;
  if (key === "window" && isWindowRecord(value)) return windowLine(value);
  if (key === "screenshot") return compactScreenshot(value);
  if (key === "accessibility") return compactAccessibility(value);
  if (key === "verification" && isRecord(value) && value.state === "unverified" && Object.keys(value).length === 1) {
    return undefined;
  }

  // list-apps: 앱 이름이 키가 되므로 창 줄에서는 app을 생략합니다.
  if (Array.isArray(value.apps) && value.apps.every(app => isRecord(app) && Array.isArray(app.windows))) {
    const apps: Rec = {};
    for (const app of value.apps as Rec[]) {
      apps[str(app.name) || str(app.id)] = (app.windows as unknown[])
        .filter(isWindowRecord)
        .map(window => windowLine(window, false));
    }
    return { ...compactObject(value, ["apps"]), apps };
  }
  return compactObject(value);
}

function compactObject(value: Rec, skip: readonly string[] = []): Rec {
  const out: Rec = {};
  for (const [key, item] of Object.entries(value)) {
    if (skip.includes(key) || DROPPED_KEYS.has(key)) continue;
    if (key === "toggles" && Array.isArray(item) && item.length === 0) continue;
    if (key === "overlayRegions" && Array.isArray(item) && item.length === 0) continue;
    // 관찰 결과의 coordinateSpace는 항상 "window"라 정보가 없습니다.
    if (key === "coordinateSpace" && item === "window") continue;
    const compacted = compactValue(item, key);
    if (compacted !== undefined) out[key] = compacted;
  }
  return out;
}

/** 네이티브 result 값을 LLM용으로 압축합니다. */
export function compactResult(value: unknown): unknown {
  return compactValue(value);
}

/** 응답 봉투에서 요청 id를 지우고 result를 압축합니다. 오류는 그대로 둡니다. */
export function compactResponse(response: { ok: boolean; result?: unknown; error?: unknown }): Rec {
  return response.ok
    ? { ok: true, result: compactResult(response.result) }
    : { ok: false, error: response.error };
}

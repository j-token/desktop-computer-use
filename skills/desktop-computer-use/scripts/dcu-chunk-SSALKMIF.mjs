#!/usr/bin/env node

var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __commonJS = (cb, mod) => function __require() {
  return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));

// src/errors.ts
var DcuError = class extends Error {
  code;
  details;
  constructor(code, message, details) {
    super(message);
    this.name = "DcuError";
    this.code = code;
    this.details = details;
  }
};
function nativeError(error) {
  return new DcuError(error?.code ?? "native_error", error?.message ?? "Native computer-use request failed", error?.details);
}
function asError(error) {
  return error instanceof Error ? error : new Error(String(error));
}

// src/runtime.ts
import { chmod, lstat, mkdir, open, readFile, stat, unlink, writeFile } from "node:fs/promises";
import { createHash, randomBytes } from "node:crypto";
import { tmpdir, userInfo } from "node:os";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import { createConnection } from "node:net";
var TOKEN_MIN_LENGTH = 32;
var startPromise;
function isWindows(platform = process.platform) {
  return platform === "win32";
}
function safeUserName() {
  const value = process.env.USERNAME ?? process.env.USER ?? userInfo().username ?? "user";
  return value.replace(/[^a-zA-Z0-9_.-]/g, "_").slice(0, 48) || "user";
}
function resolveRuntimePaths(env = process.env, platform = process.platform) {
  const explicitDirectory = env.DCU_RUNTIME_DIR;
  const defaultDirectory = isWindows(platform) ? join(env.LOCALAPPDATA || env.TEMP || tmpdir(), "desktop-computer-use") : join(env.XDG_RUNTIME_DIR || join(tmpdir(), `desktop-computer-use-${safeUserName()}`), "desktop-computer-use");
  const directory = resolve(explicitDirectory || defaultDirectory);
  const explicitEndpoint = env.DCU_ENDPOINT;
  const endpointSuffix = createHash("sha256").update(directory, "utf8").digest("hex").slice(0, 12);
  const endpoint = explicitEndpoint || (isWindows(platform) ? `\\\\.\\pipe\\desktop-computer-use-${safeUserName()}-${endpointSuffix}` : join(directory, "daemon.sock"));
  return {
    directory,
    endpoint,
    tokenFile: join(directory, "token"),
    sessionFile: join(directory, "session.json"),
    bindEndpointToToken: !explicitEndpoint && isWindows(platform)
  };
}
function endpointForToken(paths, token) {
  if (!paths.bindEndpointToToken) return paths.endpoint;
  const tokenFingerprint = createHash("sha256").update(token, "utf8").digest("hex").slice(0, 12);
  return `${paths.endpoint}-${tokenFingerprint}`;
}
function resolveSkillRoot() {
  if (process.env.DCU_SKILL_ROOT) {
    return resolve(process.env.DCU_SKILL_ROOT);
  }
  if (process.argv[1]) {
    return resolve(dirname(process.argv[1]), "..");
  }
  return process.cwd();
}
async function ensurePrivateDirectory(directory) {
  if (process.platform === "win32") {
    const privateRoots = [process.env.LOCALAPPDATA, process.env.TEMP].filter((value) => Boolean(value)).map((value) => resolve(value));
    const normalizedDirectory = resolve(directory).toLowerCase();
    const isUserPrivatePath = privateRoots.some((root) => {
      const normalizedRoot = root.toLowerCase();
      return normalizedDirectory === normalizedRoot || normalizedDirectory.startsWith(`${normalizedRoot}${sep}`);
    });
    if (!isUserPrivatePath) {
      throw new DcuError(
        "invalid_environment",
        `Windows runtime directory must be inside LOCALAPPDATA or TEMP: ${directory}`
      );
    }
  }
  await mkdir(directory, { recursive: true, mode: 448 });
  if (process.platform !== "win32") {
    await chmod(directory, 448);
  }
}
function wait(delayMs) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, delayMs));
}
async function endpointReachable(endpoint) {
  return new Promise((resolvePromise) => {
    const socket = createConnection(endpoint);
    let done = false;
    const finish = (reachable) => {
      if (done) {
        return;
      }
      done = true;
      socket.destroy();
      resolvePromise(reachable);
    };
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(150, () => finish(false));
  });
}
async function acquireDaemonLock(paths, endpoint) {
  const lockPath = join(paths.directory, "daemon.lock");
  for (let attempt = 0; attempt < 240; attempt += 1) {
    if (await endpointReachable(endpoint)) {
      return { existing: true };
    }
    try {
      const handle = await open(lockPath, "wx", 384);
      return { handle, existing: false };
    } catch (error) {
      const lockError = error;
      if (lockError.code !== "EEXIST") {
        throw new DcuError("runtime_error", `Unable to acquire daemon startup lock: ${asError(error).message}`);
      }
      try {
        const lockInfo = await stat(lockPath);
        if (Date.now() - lockInfo.mtimeMs > 15e3) {
          await unlink(lockPath);
        }
      } catch (statError) {
        const statErr = statError;
        if (statErr.code !== "ENOENT") {
          throw new DcuError("runtime_error", `Unable to inspect daemon startup lock: ${asError(statError).message}`);
        }
      }
      await wait(50);
    }
  }
  throw new DcuError("transport_timeout", "Timed out waiting for another desktop computer-use daemon to start");
}
async function releaseDaemonLock(paths, handle) {
  try {
    await handle.close();
  } finally {
    try {
      await unlink(join(paths.directory, "daemon.lock"));
    } catch (error) {
      const err = error;
      if (err.code !== "ENOENT") {
        throw error;
      }
    }
  }
}
async function ensureRuntime(paths = resolveRuntimePaths()) {
  await ensurePrivateDirectory(paths.directory);
  return paths;
}
function validToken(value) {
  return value.length >= TOKEN_MIN_LENGTH && value.length <= 256 && /^[A-Za-z0-9_=-]+$/.test(value);
}
async function ensureToken(paths = resolveRuntimePaths()) {
  await ensureRuntime(paths);
  try {
    const current = (await readFile(paths.tokenFile, "utf8")).trim();
    if (!validToken(current)) {
      throw new DcuError("invalid_environment", `Token file is invalid: ${paths.tokenFile}`);
    }
    return current;
  } catch (error) {
    if (error instanceof DcuError) throw error;
    const err = error;
    if (err.code !== "ENOENT") {
      throw new DcuError(
        "runtime_error",
        `Unable to read token file: ${asError(error).message}`
      );
    }
    const token = randomBytes(48).toString("base64url");
    try {
      await writeFile(paths.tokenFile, `${token}
`, { encoding: "utf8", mode: 384, flag: "wx" });
      if (process.platform !== "win32") await chmod(paths.tokenFile, 384);
      return token;
    } catch (createError) {
      const createErr = createError;
      if (createErr.code !== "EEXIST") {
        throw new DcuError("runtime_error", `Unable to create token file: ${asError(createError).message}`);
      }
      for (let attempt = 0; attempt < 8; attempt += 1) {
        try {
          const winner = (await readFile(paths.tokenFile, "utf8")).trim();
          if (validToken(winner)) return winner;
        } catch {
        }
        await new Promise((resolvePromise) => setTimeout(resolvePromise, 5));
      }
      throw new DcuError(
        "invalid_environment",
        `Token file was concurrently created but is invalid: ${paths.tokenFile}`
      );
    }
  }
}
async function readSession(paths = resolveRuntimePaths()) {
  try {
    const raw = await readFile(paths.sessionFile, "utf8");
    const parsed = JSON.parse(raw);
    if (typeof parsed.sessionId !== "string" || !parsed.sessionId) return void 0;
    return { sessionId: parsed.sessionId, startedAt: typeof parsed.startedAt === "string" ? parsed.startedAt : "" };
  } catch (error) {
    const err = error;
    if (err.code === "ENOENT") return void 0;
    throw new DcuError("runtime_error", `Unable to read session state: ${asError(error).message}`);
  }
}
async function writeSession(sessionId, paths = resolveRuntimePaths()) {
  await ensureRuntime(paths);
  const payload = JSON.stringify({ sessionId, startedAt: (/* @__PURE__ */ new Date()).toISOString() }) + "\n";
  await writeFile(paths.sessionFile, payload, { encoding: "utf8", mode: 384 });
  if (process.platform !== "win32") await chmod(paths.sessionFile, 384);
}
async function clearSession(paths = resolveRuntimePaths()) {
  const { unlink: unlink2 } = await import("node:fs/promises");
  try {
    await unlink2(paths.sessionFile);
  } catch (error) {
    const err = error;
    if (err.code !== "ENOENT") {
      throw new DcuError("runtime_error", `Unable to clear session state: ${asError(error).message}`);
    }
  }
}
function nativeCandidates(skillRoot, platform = process.platform) {
  const win = isWindows(platform);
  const binary = win ? "desktop-computer-use-native.exe" : "desktop-computer-use-native";
  const triplet = win ? "win32-x64" : "linux-x64";
  return [
    join(skillRoot, "bin", triplet, binary),
    join(skillRoot, "..", "..", "bin", triplet, binary),
    join(process.cwd(), "bin", triplet, binary),
    join(process.cwd(), "build", binary),
    join(process.cwd(), "out", binary),
    join(process.cwd(), "build", "native", binary)
  ];
}
async function resolveNativePath(skillRoot = resolveSkillRoot()) {
  const explicit = process.env.DCU_NATIVE_PATH;
  const candidates = explicit ? [explicit] : nativeCandidates(skillRoot);
  for (const candidate of candidates) {
    const resolved = isAbsolute(candidate) ? candidate : resolve(candidate);
    try {
      const info = await stat(resolved);
      if (!info.isFile()) continue;
      if (process.platform !== "win32" && info.uid === process.getuid?.() && !(info.mode & 64)) {
        await chmod(resolved, info.mode | 64);
      }
      return resolved;
    } catch {
    }
  }
  throw new DcuError(
    "native_not_found",
    `Native executable not found. Set DCU_NATIVE_PATH or install the platform binary. Searched: ${candidates.join(", ")}`
  );
}
async function startNativeDaemon(paths = resolveRuntimePaths(), skillRoot = resolveSkillRoot()) {
  if (startPromise) {
    return startPromise;
  }
  startPromise = (async () => {
    await ensureRuntime(paths);
    const endpoint = endpointForToken(paths, await ensureToken(paths));
    const lock = await acquireDaemonLock(paths, endpoint);
    if (lock.existing) {
      return;
    }
    if (!lock.handle) {
      throw new DcuError("runtime_error", "Daemon startup lock was not acquired");
    }
    try {
      if (await endpointReachable(endpoint)) return;
      if (process.platform !== "win32") {
        const endpointDirectory = resolve(dirname(endpoint));
        if (endpointDirectory === resolve(paths.directory)) {
          try {
            const endpointInfo = await lstat(endpoint);
            const ownedByCurrentUser = typeof process.getuid !== "function" || endpointInfo.uid === process.getuid();
            if (endpointInfo.isSocket() && ownedByCurrentUser) {
              await unlink(endpoint);
            }
          } catch (error) {
            const endpointError = error;
            if (endpointError.code !== "ENOENT") {
              throw new DcuError(
                "runtime_error",
                `Unable to inspect the daemon endpoint: ${asError(error).message}`
              );
            }
          }
        }
      }
      const nativePath = await resolveNativePath(skillRoot);
      const daemonLogPath = join(paths.directory, "daemon.log");
      let daemonLog;
      try {
        daemonLog = await open(daemonLogPath, "a", 384);
        if (process.platform !== "win32") {
          await chmod(daemonLogPath, 384);
        }
      } catch (error) {
        throw new DcuError(
          "runtime_error",
          `Unable to open the private daemon log ${daemonLogPath}: ${asError(error).message}`
        );
      }
      try {
        const child = spawn(
          nativePath,
          ["--serve", "--endpoint", endpoint, "--token-file", paths.tokenFile],
          {
            detached: true,
            stdio: ["ignore", daemonLog.fd, daemonLog.fd],
            windowsHide: true,
            cwd: dirname(nativePath)
          }
        );
        let childExited = false;
        child.once("exit", () => {
          childExited = true;
        });
        await new Promise((resolvePromise, reject) => {
          child.once("spawn", () => resolvePromise());
          child.once("error", reject);
        });
        child.unref();
        const deadline = Date.now() + 1e4;
        while (Date.now() < deadline) {
          if (childExited) {
            throw new DcuError(
              "runtime_error",
              `Native daemon exited before opening ${endpoint}. See ${daemonLogPath}`
            );
          }
          if (await endpointReachable(endpoint)) {
            return;
          }
          await wait(50);
        }
        throw new DcuError(
          "transport_timeout",
          `Timed out waiting for native daemon at ${endpoint}. See ${daemonLogPath}`
        );
      } finally {
        await daemonLog.close();
      }
    } finally {
      await releaseDaemonLock(paths, lock.handle);
    }
  })().finally(() => {
    startPromise = void 0;
  });
  return startPromise;
}
function usageInstructions(platform = process.platform) {
  const indicatorInstructions = [
    "Active sessions show a noninteractive top banner, high-contrast cursor ring, and blue inward-fading screen-edge border.",
    "Press Esc twice within 1 second or run `dcu session stop` to stop the session and release held input."
  ];
  if (isWindows(platform)) {
    return [
      ...indicatorInstructions,
      "Keep the target window visible on the active desktop.",
      "If Windows blocks input or capture, grant permissions to the native executable."
    ];
  }
  return [
    ...indicatorInstructions,
    "Run setup to install and enable the matching user-local GNOME extension, compile its schemas,",
    "and inspect Wayland portal requirements.",
    "Setup never logs out or restarts GNOME Shell; doctor remains the readiness gate.",
    "A graphical login session is required; SSH alone is not a desktop session.",
    "On Wayland, approve the RemoteDesktop and ScreenCast portal prompts when requested."
  ];
}

// src/transport.ts
import { createConnection as createConnection2 } from "node:net";
import { randomUUID } from "node:crypto";
import { StringDecoder } from "node:string_decoder";
var MAX_FRAME_BYTES = 16 * 1024 * 1024;
var DEFAULT_TIMEOUT_MS = 65e3;
function transportError(message, requestSent, code = "transport_error") {
  const error = new Error(message);
  error.name = "DcuTransportError";
  error.requestSent = requestSent;
  error.code = code;
  return error;
}
function responseFromLine(line, requestId) {
  let parsed;
  try {
    parsed = JSON.parse(line);
  } catch {
    throw transportError("Native daemon returned invalid JSON", true, "protocol_error");
  }
  if (!parsed || typeof parsed !== "object") {
    throw transportError("Native daemon returned a non-object response", true, "protocol_error");
  }
  const response = parsed;
  if (response.id !== requestId) {
    throw transportError("Native daemon response id did not match the request", true, "protocol_error");
  }
  if (typeof response.ok !== "boolean") {
    throw transportError("Native daemon response omitted ok", true, "protocol_error");
  }
  const hasMalformedError = response.ok === false && (!response.error || typeof response.error.code !== "string" || typeof response.error.message !== "string");
  if (hasMalformedError) {
    throw transportError("Native daemon returned a malformed error response", true, "protocol_error");
  }
  return response;
}
async function sendNativeRequest(endpoint, method, token, params = {}, timeoutMs = DEFAULT_TIMEOUT_MS) {
  const request = { id: randomUUID(), token, method, params };
  const payload = `${JSON.stringify(request)}
`;
  const byteLength = Buffer.byteLength(payload, "utf8");
  if (byteLength > MAX_FRAME_BYTES) {
    throw new DcuError(
      "invalid_argument",
      `Request exceeds the ${MAX_FRAME_BYTES} byte protocol limit`
    );
  }
  return new Promise((resolve2, reject) => {
    let socket;
    let settled = false;
    let requestSent = false;
    let bytes = 0;
    let text = "";
    const decoder = new StringDecoder("utf8");
    let sawLine = false;
    const finishError = (error) => {
      if (settled) {
        return;
      }
      settled = true;
      socket?.destroy();
      if (error instanceof Error && "requestSent" in error) {
        reject(error);
        return;
      }
      reject(transportError(
        error instanceof Error ? error.message : String(error),
        requestSent,
        error?.code || "transport_error"
      ));
    };
    const finishResponse = (line) => {
      if (settled) {
        return;
      }
      settled = true;
      socket?.destroy();
      try {
        resolve2(responseFromLine(line, request.id));
      } catch (error) {
        reject(error);
      }
    };
    try {
      socket = createConnection2(endpoint);
      socket.setTimeout(timeoutMs, () => {
        finishError(transportError(`Native daemon timed out after ${timeoutMs}ms`, requestSent, "timeout"));
      });
      socket.once("connect", () => {
        if (settled || !socket) {
          return;
        }
        requestSent = true;
        socket.write(payload, "utf8");
      });
      socket.on("data", (chunk) => {
        if (settled) {
          return;
        }
        const incoming = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
        bytes += incoming.byteLength;
        if (bytes > MAX_FRAME_BYTES) {
          finishError(transportError(
            `Native response exceeds the ${MAX_FRAME_BYTES} byte protocol limit`,
            requestSent,
            "frame_too_large"
          ));
          return;
        }
        text += decoder.write(incoming);
        const newline = text.indexOf("\n");
        if (newline >= 0) {
          sawLine = true;
          const line = text.slice(0, newline).replace(/\r$/, "");
          finishResponse(line);
        }
      });
      socket.once("end", () => {
        text += decoder.end();
        const newline = text.indexOf("\n");
        if (newline >= 0 && !sawLine) {
          sawLine = true;
          finishResponse(text.slice(0, newline).replace(/\r$/, ""));
          return;
        }
        if (!sawLine) {
          finishError(transportError(
            "Native daemon closed the connection without a response",
            requestSent,
            "protocol_error"
          ));
        }
      });
      socket.once("error", (error) => finishError(error));
    } catch (error) {
      finishError(error);
    }
  });
}
function ensureOk(response) {
  if (!response.ok) throw nativeError(response.error);
  return response.result;
}

// src/client.ts
var DEFAULT_TIMEOUT_MS2 = 65e3;
var DAEMON_START_TIMEOUT_MS = 1e4;
var RETRY_DELAY_MS = 50;
var STARTABLE_CODES = /* @__PURE__ */ new Set([
  "ENOENT",
  "ECONNREFUSED",
  "ECONNRESET",
  "EPIPE",
  "EPERM",
  "ENOTFOUND",
  "transport_error"
]);
function canStart(error) {
  if (!(error instanceof Error) || !("requestSent" in error)) {
    return false;
  }
  const failure = error;
  return !failure.requestSent && (!failure.code || STARTABLE_CODES.has(failure.code));
}
function wait2(delayMs) {
  return new Promise((resolve2) => setTimeout(resolve2, delayMs));
}
var DcuClient = class {
  paths;
  skillRoot;
  constructor(paths = resolveRuntimePaths(), skillRoot = resolveSkillRoot()) {
    this.paths = paths;
    this.skillRoot = skillRoot;
  }
  async request(method, params = {}, options = {}) {
    await ensureRuntime(this.paths);
    const token = await ensureToken(this.paths);
    const endpoint = endpointForToken(this.paths, token);
    const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS2;
    try {
      return await sendNativeRequest(endpoint, method, token, params, timeoutMs);
    } catch (error) {
      if (options.autoStart === false || !canStart(error)) {
        throw error;
      }
      await startNativeDaemon(this.paths, this.skillRoot);
      const deadline = Date.now() + Math.min(timeoutMs, DAEMON_START_TIMEOUT_MS);
      let lastError = error;
      while (Date.now() < deadline) {
        try {
          return await sendNativeRequest(endpoint, method, token, params, timeoutMs);
        } catch (retryError) {
          lastError = retryError;
          if (!canStart(retryError)) {
            throw retryError;
          }
          await wait2(RETRY_DELAY_MS);
        }
      }
      throw lastError;
    }
  }
  async call(method, params = {}, options = {}) {
    const response = await this.request(method, params, options);
    return ensureOk(response);
  }
  async callEnvelope(method, params = {}, options = {}) {
    const response = await this.request(method, params, options);
    if (!response.ok) {
      throw nativeError(response.error);
    }
    return response;
  }
};
function isTransportFailure(error) {
  return error instanceof Error && "requestSent" in error;
}
function errorToResponse(error) {
  if (error instanceof DcuError) {
    return {
      ok: false,
      error: {
        code: error.code,
        message: error.message,
        ...error.details === void 0 ? {} : { details: error.details }
      }
    };
  }
  if (isTransportFailure(error)) {
    return {
      ok: false,
      error: { code: error.code || "transport_error", message: error.message }
    };
  }
  const err = error instanceof Error ? error : new Error(String(error));
  return { ok: false, error: { code: "client_error", message: err.message } };
}

// src/compact.ts
var TEXT_LIMIT = 300;
function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function num(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : void 0;
}
function str(value) {
  return typeof value === "string" ? value : "";
}
function clip(text) {
  const flat = text.replace(/\s+/gu, " ").trim();
  return flat.length > TEXT_LIMIT ? `${flat.slice(0, TEXT_LIMIT)}\u2026(${flat.length})` : flat;
}
function quote(text) {
  return JSON.stringify(clip(text));
}
function rect(value) {
  if (!isRecord(value)) return void 0;
  const x = num(value.x), y = num(value.y), w = num(value.width), h = num(value.height);
  if (x === void 0 || y === void 0 || w === void 0 || h === void 0) return void 0;
  return `${x},${y} ${w}x${h}`;
}
function hasArea(value) {
  return isRecord(value) && (num(value.width) ?? 0) > 0 && (num(value.height) ?? 0) > 0;
}
function windowLine(window, includeApp = true) {
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
function isWindowRecord(value) {
  return isRecord(value) && typeof value.id === "string" && "title" in value && "width" in value;
}
function elementLine(element) {
  const bounds = element.bounds ?? element.frame;
  const actions = [
    ...Array.isArray(element.patterns) ? element.patterns : [],
    ...Array.isArray(element.actions) ? element.actions : []
  ].map(String);
  const focused = element.focused === true;
  if (!hasArea(bounds) && actions.length === 0 && !focused) return void 0;
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
function compactAccessibility(value) {
  const elements = Array.isArray(value.elements) ? value.elements : [];
  const lines = [];
  for (const element of elements) {
    const line = isRecord(element) ? elementLine(element) : void 0;
    if (line) lines.push(line);
  }
  const out = { elements: lines };
  const hidden = elements.length - lines.length;
  if (hidden > 0) out.hidden = hidden;
  if (value.truncated === true) out.truncated = true;
  return out;
}
function compactScreenshot(value) {
  const out = { path: value.path, width: value.width, height: value.height };
  if (value.mimeType && value.mimeType !== "image/jpeg") out.mimeType = value.mimeType;
  const fullWidth = num(value.fullWidth);
  if (fullWidth !== void 0 && fullWidth !== value.width) {
    out.full = `${fullWidth}x${num(value.fullHeight) ?? "?"}`;
  }
  if (value.variant === "full") out.variant = "full";
  return out;
}
var DROPPED_KEYS = /* @__PURE__ */ new Set([
  "timings",
  "capturedAt",
  "observedAt",
  "runtimeId",
  "tree",
  // CLI와 MCP가 원본 응답에서 세션 파일에 저장하므로 LLM에게는 반복되는 잡음입니다.
  "sessionId"
]);
function compactValue(value, key = "") {
  if (Array.isArray(value)) {
    if ((key === "windows" || key === "ownedWindows") && value.every(isWindowRecord)) {
      return value.map((window) => windowLine(window));
    }
    return value.map((item) => compactValue(item));
  }
  if (!isRecord(value)) return value;
  if (key === "window" && isWindowRecord(value)) return windowLine(value);
  if (key === "screenshot") return compactScreenshot(value);
  if (key === "accessibility") return compactAccessibility(value);
  if (key === "verification" && isRecord(value) && value.state === "unverified" && Object.keys(value).length === 1) {
    return void 0;
  }
  if (Array.isArray(value.apps) && value.apps.every((app) => isRecord(app) && Array.isArray(app.windows))) {
    const apps = {};
    for (const app of value.apps) {
      apps[str(app.name) || str(app.id)] = app.windows.filter(isWindowRecord).map((window) => windowLine(window, false));
    }
    return { ...compactObject(value, ["apps"]), apps };
  }
  return compactObject(value);
}
function compactObject(value, skip = []) {
  const out = {};
  for (const [key, item] of Object.entries(value)) {
    if (skip.includes(key) || DROPPED_KEYS.has(key)) continue;
    if (key === "toggles" && Array.isArray(item) && item.length === 0) continue;
    if (key === "overlayRegions" && Array.isArray(item) && item.length === 0) continue;
    if (key === "coordinateSpace" && item === "window") continue;
    const compacted = compactValue(item, key);
    if (compacted !== void 0) out[key] = compacted;
  }
  return out;
}
function compactResult(value) {
  return compactValue(value);
}
function compactResponse(response) {
  return response.ok ? { ok: true, result: compactResult(response.result) } : { ok: false, error: response.error };
}

// src/methods.ts
var SESSIONLESS_METHODS = /* @__PURE__ */ new Set([
  "doctor",
  "capabilities",
  "session.start",
  "session.status",
  "session.stop",
  "daemon.shutdown",
  "toggle.status"
]);
var TOGGLE_KEYS = ["shift", "ctrl", "alt", "win", "space"];
var TOGGLE_KEY_ALIASES = [...TOGGLE_KEYS, "control", "super", "meta"];
var MODIFIER_NAMES = ["shift", "ctrl", "control", "alt", "option", "win", "meta", "super"];
function isValidModifiers(value) {
  return value.split("+").every((part) => MODIFIER_NAMES.includes(part.toLowerCase()));
}
var WINDOW_TARGET_METHODS = /* @__PURE__ */ new Set([
  "get-app-state",
  "get-full-screenshot",
  "click",
  "drag",
  "scroll",
  "type-text",
  "press-key",
  "hotkey",
  "set-value",
  "paste-text"
]);

export {
  __commonJS,
  __export,
  __toESM,
  DcuError,
  resolveRuntimePaths,
  endpointForToken,
  resolveSkillRoot,
  ensureRuntime,
  ensureToken,
  readSession,
  writeSession,
  clearSession,
  resolveNativePath,
  usageInstructions,
  DcuClient,
  errorToResponse,
  compactResult,
  compactResponse,
  SESSIONLESS_METHODS,
  TOGGLE_KEY_ALIASES,
  MODIFIER_NAMES,
  isValidModifiers,
  WINDOW_TARGET_METHODS
};

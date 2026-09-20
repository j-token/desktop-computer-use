import { DcuError, nativeError } from "./errors.js";
import {
  ensureRuntime,
  ensureToken,
  resolveRuntimePaths,
  resolveSkillRoot,
  startNativeDaemon
} from "./runtime.js";
import { ensureOk, sendNativeRequest } from "./transport.js";
import type { CallOptions, JsonObject, NativeResponse, RuntimePaths, TransportFailure } from "./types.js";

const DEFAULT_TIMEOUT_MS = 65_000;
const DAEMON_START_TIMEOUT_MS = 10_000;
const RETRY_DELAY_MS = 50;
const STARTABLE_CODES = new Set([
  "ENOENT",
  "ECONNREFUSED",
  "ECONNRESET",
  "EPIPE",
  "EPERM",
  "ENOTFOUND",
  "transport_error"
]);

function canStart(error: unknown): error is TransportFailure {
  if (!(error instanceof Error) || !("requestSent" in error)) {
    return false;
  }
  const failure = error as TransportFailure;
  return !failure.requestSent && (!failure.code || STARTABLE_CODES.has(failure.code));
}

function wait(delayMs: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, delayMs));
}

export class DcuClient {
  readonly paths: RuntimePaths;
  private readonly skillRoot: string;
  constructor(paths = resolveRuntimePaths(), skillRoot = resolveSkillRoot()) {
    this.paths = paths;
    this.skillRoot = skillRoot;
  }

  async request(method: string, params: JsonObject = {}, options: CallOptions = {}): Promise<NativeResponse> {
    await ensureRuntime(this.paths);
    const token = await ensureToken(this.paths);
    const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    try {
      return await sendNativeRequest(this.paths.endpoint, method, token, params, timeoutMs);
    } catch (error) {
      if (options.autoStart === false || !canStart(error)) {
        throw error;
      }
      await startNativeDaemon(this.paths, this.skillRoot);
      const deadline = Date.now() + Math.min(timeoutMs, DAEMON_START_TIMEOUT_MS);
      let lastError: unknown = error;
      while (Date.now() < deadline) {
        try {
          // Only pre-write connection failures are retried. Once the request reaches
          // the daemon, the mutation is never sent again by this client.
          return await sendNativeRequest(this.paths.endpoint, method, token, params, timeoutMs);
        } catch (retryError) {
          lastError = retryError;
          if (!canStart(retryError)) {
            throw retryError;
          }
          await wait(RETRY_DELAY_MS);
        }
      }
      throw lastError;
    }
  }

  async call(method: string, params: JsonObject = {}, options: CallOptions = {}): Promise<unknown> {
    const response = await this.request(method, params, options);
    return ensureOk(response);
  }

  async callEnvelope(method: string, params: JsonObject = {}, options: CallOptions = {}): Promise<NativeResponse> {
    const response = await this.request(method, params, options);
    if (!response.ok) {
      throw nativeError(response.error);
    }
    return response;
  }
}

export function isTransportFailure(error: unknown): error is TransportFailure {
  return error instanceof Error && "requestSent" in error;
}

export function errorToResponse(error: unknown): {
  ok: false;
  error: { code: string; message: string; details?: unknown };
} {
  if (error instanceof DcuError) {
    return {
      ok: false,
      error: {
        code: error.code,
        message: error.message,
        ...(error.details === undefined ? {} : { details: error.details })
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

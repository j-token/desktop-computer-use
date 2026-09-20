import { createConnection, type Socket } from "node:net";
import { randomUUID } from "node:crypto";
import { StringDecoder } from "node:string_decoder";
import type { NativeRequest, NativeResponse, TransportFailure, JsonObject } from "./types.js";
import { DcuError, nativeError } from "./errors.js";

export const MAX_FRAME_BYTES = 16 * 1024 * 1024;
export const DEFAULT_TIMEOUT_MS = 65_000;

function transportError(message: string, requestSent: boolean, code = "transport_error"): TransportFailure {
  const error = new Error(message) as TransportFailure;
  error.name = "DcuTransportError";
  error.requestSent = requestSent;
  error.code = code;
  return error;
}

function responseFromLine(line: string, requestId: string): NativeResponse {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    throw transportError("Native daemon returned invalid JSON", true, "protocol_error");
  }
  if (!parsed || typeof parsed !== "object") {
    throw transportError("Native daemon returned a non-object response", true, "protocol_error");
  }
  const response = parsed as Partial<NativeResponse>;
  if (response.id !== requestId) {
    throw transportError("Native daemon response id did not match the request", true, "protocol_error");
  }
  if (typeof response.ok !== "boolean") {
    throw transportError("Native daemon response omitted ok", true, "protocol_error");
  }
  const hasMalformedError = response.ok === false && (
    !response.error ||
    typeof response.error.code !== "string" ||
    typeof response.error.message !== "string"
  );
  if (hasMalformedError) {
    throw transportError("Native daemon returned a malformed error response", true, "protocol_error");
  }
  return response as NativeResponse;
}

export async function sendNativeRequest(
  endpoint: string,
  method: string,
  token: string,
  params: JsonObject = {},
  timeoutMs = DEFAULT_TIMEOUT_MS
): Promise<NativeResponse> {
  const request: NativeRequest = { id: randomUUID(), token, method, params };
  const payload = `${JSON.stringify(request)}\n`;
  const byteLength = Buffer.byteLength(payload, "utf8");
  if (byteLength > MAX_FRAME_BYTES) {
    throw new DcuError(
      "invalid_argument",
      `Request exceeds the ${MAX_FRAME_BYTES} byte protocol limit`
    );
  }

  return new Promise<NativeResponse>((resolve, reject) => {
    let socket: Socket | undefined;
    let settled = false;
    let requestSent = false;
    let bytes = 0;
    let text = "";
    const decoder = new StringDecoder("utf8");
    let sawLine = false;
    const finishError = (error: unknown): void => {
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
        (error as NodeJS.ErrnoException)?.code || "transport_error"
      ));
    };
    const finishResponse = (line: string): void => {
      if (settled) {
        return;
      }
      settled = true;
      socket?.destroy();
      try {
        resolve(responseFromLine(line, request.id));
      } catch (error) {
        reject(error);
      }
    };
    try {
      socket = createConnection(endpoint);
      socket.setTimeout(timeoutMs, () => {
        finishError(transportError(`Native daemon timed out after ${timeoutMs}ms`, requestSent, "timeout"));
      });
      socket.once("connect", () => {
        if (settled || !socket) {
          return;
        }
        requestSent = true;
        // Keep the write side open until the daemon has replied. The native
        // watcher treats a client half-close as ownership loss and interrupts
        // an in-flight input operation.
        socket.write(payload, "utf8");
      });
      socket.on("data", (chunk: Buffer | string) => {
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
      socket.once("error", error => finishError(error));
    } catch (error) {
      finishError(error);
    }
  });
}

export function ensureOk(response: NativeResponse): unknown {
  if (!response.ok) throw nativeError(response.error);
  return response.result;
}

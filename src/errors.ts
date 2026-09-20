import type { NativeError } from "./types.js";

export class DcuError extends Error {
  readonly code: string;
  readonly details?: unknown;
  constructor(code: string, message: string, details?: unknown) {
    super(message);
    this.name = "DcuError";
    this.code = code;
    this.details = details;
  }
}

export function nativeError(error: NativeError | undefined): DcuError {
  return new DcuError(error?.code ?? "native_error", error?.message ?? "Native computer-use request failed", error?.details);
}

export function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

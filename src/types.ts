export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue };

export interface NativeRequest {
  id: string;
  token: string;
  method: string;
  params: JsonObject;
}

export interface NativeError {
  code: string;
  message: string;
  details?: JsonValue;
}

export interface NativeResponse<T = JsonValue> {
  id: string;
  ok: boolean;
  result?: T;
  error?: NativeError;
}

export interface RuntimePaths {
  directory: string;
  endpoint: string;
  tokenFile: string;
  sessionFile: string;
}

export interface SessionState {
  sessionId: string;
  startedAt: string;
}

export interface CallOptions {
  autoStart?: boolean;
  timeoutMs?: number;
}

export interface TransportFailure extends Error {
  requestSent: boolean;
  code?: string;
}

import { chmod, mkdir, open, type FileHandle } from "node:fs/promises";
import { join } from "node:path";
import { resolveRuntimePaths } from "../runtime.js";
import { asError } from "../errors.js";
import type { JevUsage } from "./jev.js";

/**
 * One NDJSON record per event, in a user-private file. What may be written is the
 * short version: the shape of the decision, the call that was made and the label
 * it acted on. What may never be written is the API key, the state payload — it
 * carries the visible text of someone's window — and any value the redaction
 * pass blanked.
 */

export interface TraceRunRecord {
  type: "run";
  at: string;
  runId: string;
  goal: string;
  window: { id?: string; app?: string; title?: string };
  budgets: { maxSteps: number; maxDecisions: number; maxDurationMs: number; maxInputTokens: number };
  gate: { minConfidence: number; minMargin: number };
  destructive: { requested: string; effective: string; degradedReason?: string };
}

export interface TraceAnswer {
  choice?: string;
  confidence?: number;
  probabilities?: Record<string, number>;
}

export interface TraceDecideRecord {
  type: "decide";
  at: string;
  step: number;
  decision: number;
  /** Which look at this step this was: 2 means the gate refused the first one. */
  attempt: number;
  model: string;
  latencyMs: number;
  usage: JevUsage;
  candidateCount: number;
  /** Everything UIA reported, before the filter; the gap between the two is the point. */
  rawElementCount: number;
  estimatedStateTokens: number;
  /** Every answer with its full probability vector. Choices are option ids, never window text. */
  answers: Record<string, TraceAnswer>;
  gate: {
    passed: boolean;
    minConfidence: number;
    minMargin: number;
    reason?: string;
    question?: string;
    readings?: {
      question: string;
      choice: string;
      confidence: number;
      margin: number;
      /** False where the floors were never applied, because nothing was decided on that answer. */
      gated: boolean;
    }[];
  };
  status?: string;
  operation?: string;
  targetLabel?: string;
}

export interface TraceActRecord {
  type: "act";
  at: string;
  step: number;
  method: string;
  /** The call's params minus the session id and minus any typed string. */
  params: Record<string, unknown>;
  targetLabel?: string;
  outcome: "ok" | "error";
  code?: string;
  message?: string;
  /** Measured across the round trip by this process. */
  elapsedMs: number;
  /** Reported by the daemon for the observation it attached, when it attached one. */
  nativeMs?: number;
  textSource?: string;
  textChars?: number;
}

export interface TraceNoteRecord {
  type: "note";
  at: string;
  step: number;
  event: string;
  detail?: Record<string, unknown>;
}

export interface TraceSummaryRecord {
  type: "summary";
  at: string;
  runId: string;
  status: string;
  reason: string;
  steps: number;
  decisions: number;
  usage: JevUsage;
  durationMs: number;
  expectation?: string;
  lastObservationId?: string;
}

export type TraceRecord =
  | TraceRunRecord
  | TraceDecideRecord
  | TraceActRecord
  | TraceNoteRecord
  | TraceSummaryRecord;

export interface Trace {
  readonly path: string;
  write(record: TraceRecord): Promise<void>;
  close(): Promise<void>;
}

/** `${runtimeDir}/agent`, unless DCU_AGENT_TRACE_DIR names somewhere else. */
export function traceDirectory(env: NodeJS.ProcessEnv = process.env): string {
  const override = env.DCU_AGENT_TRACE_DIR?.trim();
  if (override) return override;
  return join(resolveRuntimePaths(env).directory, "agent");
}

export function tracePath(runId: string, directory = traceDirectory()): string {
  return join(directory, `${runId}.jsonl`);
}

class FileTrace implements Trace {
  constructor(readonly path: string, private handle: FileHandle | undefined) {}

  async write(record: TraceRecord): Promise<void> {
    if (!this.handle) return;
    await this.handle.write(`${JSON.stringify(record)}\n`, null, "utf8");
  }

  async close(): Promise<void> {
    const handle = this.handle;
    this.handle = undefined;
    if (handle) await handle.close();
  }
}

/** A trace that goes nowhere, for a caller that does not want a file on disk. */
export function nullTrace(): Trace {
  return {
    path: "",
    async write(): Promise<void> { /* nothing is recorded */ },
    async close(): Promise<void> { /* nothing to close */ }
  };
}

/** Records in memory, for tests and for a caller that wants to inspect rather than keep. */
export function memoryTrace(records: TraceRecord[] = []): Trace & { records: TraceRecord[] } {
  return {
    path: "",
    records,
    async write(record: TraceRecord): Promise<void> {
      records.push(record);
    },
    async close(): Promise<void> { /* nothing to close */ }
  };
}

export interface CreateTraceOptions {
  runId: string;
  directory?: string;
}

export async function createTrace(options: CreateTraceOptions): Promise<Trace> {
  const directory = options.directory ?? traceDirectory();
  const path = tracePath(options.runId, directory);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const handle = await open(path, "a", 0o600);
  if (process.platform !== "win32") {
    try {
      await chmod(path, 0o600);
    } catch (error) {
      await handle.close();
      throw new Error(`Unable to secure the agent trace ${path}: ${asError(error).message}`);
    }
  }
  return new FileTrace(path, handle);
}

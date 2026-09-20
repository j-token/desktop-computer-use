export { runCli, parseArgs } from "./cli.js";
export { DcuClient } from "./client.js";
export { resolveRuntimePaths, ensureRuntime, ensureToken } from "./runtime.js";
export { parseGnomeShellMajor, selectGnomeVariant, runLocalSetup } from "./setup.js";
export { sendNativeRequest, MAX_FRAME_BYTES } from "./transport.js";
export { buildCandidates, humanizeAutomationId, normalizeLabel } from "./agent/filter.js";
export { buildRegions, describePlace, MAIN_REGION_ID } from "./agent/regions.js";
export { explain, goalTokens, readObservation, renderTable, serializeCandidate } from "./agent/explain.js";
export { DECISION_OPERATIONS, describeControl, describeStep, estimateTokens, serializeState } from "./agent/state.js";
export {
  DEFAULT_MIN_CONFIDENCE,
  DEFAULT_MIN_MARGIN,
  HOTKEY_CHORDS,
  MAX_CHOICE_OPTIONS,
  PRESS_KEYS,
  SCROLL_DIRECTIONS,
  buildQuestions,
  marginOf,
  parseAnswers,
  targetQuestionFor
} from "./agent/questions.js";
export {
  DEFAULT_BASE_URL,
  DEFAULT_MODEL,
  DEFAULT_TIMEOUT_MS,
  FetchJevTransport,
  describeShape,
  jevHost,
  readJevConfig,
  retryAfterMs
} from "./agent/jev.js";
export { MAX_STATE_TOKENS, decide, doctor, renderDecision } from "./agent/decide.js";
export {
  DEFAULT_MAX_DURATION_MS,
  DEFAULT_MAX_INPUT_TOKENS,
  DEFAULT_MAX_STEPS,
  DESTRUCTIVE_CHORDS,
  DESTRUCTIVE_KEYS,
  DESTRUCTIVE_LABEL_PATTERNS,
  MAX_STEPS_CEILING,
  NO_PROGRESS_REPEATS,
  NoProgressDetector,
  SENSITIVE_LABEL,
  SENSITIVE_VALUE,
  actionKey,
  candidatesHash,
  describeAction,
  exhaustedBudget,
  findDestructive,
  redactCandidates,
  resolveBudgets,
  resolveDestructiveMode,
  resolveGate
} from "./agent/guards.js";
export {
  PASTE_LENGTH_THRESHOLD,
  SCROLL_AMOUNT,
  TextResolver,
  WAIT_MS,
  goalQuotes,
  isTargetFocused,
  needsPaste,
  observeCall,
  planAction,
  regionPoint,
  runPlan,
  traceableParams
} from "./agent/executor.js";
export { createTrace, memoryTrace, nullTrace, traceDirectory, tracePath } from "./agent/trace.js";
export {
  Agent,
  NativeCaller,
  createJevDecideStep,
  describeChange,
  dryRun,
  pickWindow,
  prepareTarget,
  windowsByTitle
} from "./agent/agent.js";
export { DROP_REASONS } from "./agent/types.js";
export type {
  DecideOptions,
  DecideResult,
  DoctorResult
} from "./agent/decide.js";
export type {
  JevAnswers,
  JevConfig,
  JevQuestion,
  JevRequest,
  JevResponse,
  JevTransport
} from "./agent/jev.js";
export type {
  Decision,
  DecisionRegion,
  DecisionTarget,
  GateFailure,
  GateSettings,
  QuestionPlan
} from "./agent/questions.js";
export type {
  DecisionOperation,
  HistoryStep,
  StateCandidate,
  StateInputs
} from "./agent/state.js";
export type {
  Budgets,
  BudgetCounters,
  BudgetReason,
  DestructiveFinding,
  DestructiveGate,
  DestructiveMode
} from "./agent/guards.js";
export type {
  ActionContext,
  ActionPlan,
  CallOutcome,
  NativeCall,
  PlanRun,
  ResolvedText
} from "./agent/executor.js";
export type { Trace, TraceRecord } from "./agent/trace.js";
export type {
  AgentConfig,
  AgentDeps,
  AgentEvent,
  AgentResult,
  AgentStatus,
  DecideStep,
  DecideStepInput,
  DecideStepResult,
  DryRunResult
} from "./agent/agent.js";
export type {
  Candidate,
  CandidateRole,
  DropReason,
  FilterOptions,
  FilterReport,
  FilterResult,
  Observation,
  Operation,
  PatternName,
  RegionSummary,
  UiaElement
} from "./agent/types.js";

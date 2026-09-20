# Architecture

Desktop Computer Use separates model reasoning from operating-system access. The harness and optional Jev helper choose actions, while a local native daemon owns observation, input, session state, and cleanup.

## Components

| Component | Location | Responsibility |
|---|---|---|
| Harness skill | `skills/desktop-computer-use/` | Tells the running LLM how to inspect state, choose a workflow, operate safely, and recover. |
| CLI and MCP server | `src/` | Validates arguments, manages the local runtime, exposes JSON and MCP interfaces, and converts native screenshots to MCP image content. |
| Agent helpers | `src/agent/` | Filters accessibility state, enumerates candidate actions, calls Jev when requested, applies guards, and traces bounded runs. |
| Common native daemon | `native/common/` | Authenticates requests, serializes operations, owns session leases, dispatches methods, and coordinates cancellation. |
| Platform backends | `native/windows/`, `native/linux/` | Capture the target window, read accessible controls, inject input, and implement platform permission checks. |
| GNOME extension | `extensions/` | Shows the Linux session indicator, reports window geometry, and exposes the emergency-stop route. |

## Runtime flow

1. `dcu setup` creates a user-private runtime directory and authentication token. It also installs the matching GNOME extension on supported Linux desktops.
2. The CLI starts or connects to the native daemon over a Windows named pipe or Linux Unix socket.
3. `session start` establishes one visible input lease. Input commands without the active session ID fail closed.
4. The harness requests an observation. The backend returns window geometry, an observation ID, optional accessibility elements, and an optional screenshot path.
5. The harness either selects an action itself or gives a short subgoal to the Jev helper.
6. The native daemon validates the observation and target, delivers the action, and can attach a follow-up observation to the same response.
7. `Esc`, `session stop`, idle expiry, client loss, or daemon shutdown ends the session and releases held input.

Each local connection carries one authenticated UTF-8 NDJSON request and one response. The protocol caps frames at 16 MiB. See [the protocol reference](../skills/desktop-computer-use/references/protocol.md) for the complete request, recovery, and unknown-outcome rules.

## Harness modes

### `llm`

The LLM already running the skill owns the observe, decide, act loop. It calls the ordinary CLI or MCP methods directly. No external model configuration is required.

### `llm+jev`

The harness still owns the goal, typed text, session lifecycle, destructive-action decisions, and recovery. It delegates only a short accessibility-based subgoal to the Jev loop. The loop filters the current tree, asks independent choice questions in one request, applies confidence and safety gates, executes one action at a time, and stops when it succeeds, blocks, or reaches its budget.

`agent run` and `agent decide` are always Jev-backed helper commands. Setting `DCU_AGENT_MODE=llm` changes how the harness uses the skill; it does not turn those commands into a second harness LLM.

## State and ordering invariants

- An action requires the current session ID.
- Element indexes belong to the latest observation and cannot be reused after window geometry changes.
- Native operations are serialized, while cancellation can bypass a blocked capture or input operation.
- A transport failure after request bytes were written can leave a mutation outcome unknown. The client does not replay that mutation automatically.
- Input ownership is released on normal stop, timeout, cancellation, disconnect, and process termination paths.
- A successful input-delivery response does not prove that the target application has finished processing or repainting. Observe again when visible final state matters.

## Trust boundary and local data

The token, session record, traces, and captured images live in user-private runtime locations. The MCP server reads a returned image file and emits image content to its client; the JSON CLI reports the path. The native process is the only component that touches operating-system input APIs.

Local `.env` files, traces, screenshots, accessibility dumps, and desktop logs can contain credentials or personal information. They are runtime data and are excluded from the source repository.

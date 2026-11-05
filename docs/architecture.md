# Architecture

Desktop Computer Use separates model reasoning from operating-system access. The LLM running the skill chooses actions, while a local native daemon owns observation, input, session state, and cleanup.

## Components

| Component | Location | Responsibility |
|---|---|---|
| Harness skill | `skills/desktop-computer-use/` | Tells the running LLM how to inspect state, choose a workflow, operate safely, and recover. |
| CLI and MCP server | `src/` | Validates arguments, manages the local runtime, exposes JSON and MCP interfaces, and converts native screenshots to MCP image content. |
| Common native daemon | `native/common/` | Authenticates requests, schedules operations across agent sessions, owns session leases, dispatches methods, and coordinates cancellation. |
| Platform backends | `native/windows/`, `native/linux/` | Capture the target window, read accessible controls, inject input, and implement platform permission checks. |
| GNOME extension | `extensions/` | Shows the Linux session indicator, reports window geometry, and exposes the emergency-stop route. |

## Runtime flow

1. `dcu setup` creates a user-private runtime directory and authentication token. It also installs the matching GNOME extension on supported Linux desktops.
2. The CLI starts or connects to the native daemon over a Windows named pipe or Linux Unix socket.
3. `session start` establishes one visible input lease. With `multiAgent`, later agents join that desktop session and each gets its own agent session ID. Input commands without an active session ID fail closed.
4. The harness requests an observation. The backend returns window geometry, an observation ID, optional accessibility elements, and an optional screenshot path.
5. The harness selects the next action itself.
6. The native daemon validates the observation and target, delivers the action, and can attach a follow-up observation to the same response.
7. Two `Esc` presses within 1 second, `session stop`, idle expiry, client loss, or daemon shutdown ends the session and releases held input.

Each local connection carries one authenticated UTF-8 NDJSON request and one response. The protocol caps frames at 16 MiB. See [the protocol reference](../skills/desktop-computer-use/references/protocol.md) for the complete request, recovery, and unknown-outcome rules.

## State and ordering invariants

- An action requires an active agent session ID. Observations, toggles, idle expiry, and per-agent stop belong to one agent session; the indicator, emergency stop, and input lease belong to the shared desktop session.
- Element indexes belong to the latest observation of the same agent session and cannot be reused after window geometry changes.
- Scheduling uses a shared desktop lock for operations and an exclusive one for start, stop, and cleanup. Physical input (pointer, keyboard, toggles, activation) holds a separate mutex because the desktop has one pointer and keyboard. A backend that is not thread-safe (Linux AT-SPI and Xlib) also holds a backend mutex, so its operations run one at a time. Lock order is desktop, physical input, backend, state. A request that cannot get its turn within 45 seconds fails with `busy`.
- Headless requests never take the physical-input mutex; anything that would need real input fails with `headless_unavailable` instead of falling back.
- `set-state`, `scroll-to`, and `select-option` only call accessibility patterns, so they skip the physical-input mutex and the toggle-owner check even without `headless`.
- Cancellation can bypass a blocked capture or input operation; stopping one agent cancels only that agent's operation.
- A transport failure after request bytes were written can leave a mutation outcome unknown. The client does not replay that mutation automatically.
- Input ownership is released on normal stop, timeout, cancellation, disconnect, and process termination paths.
- A successful input-delivery response does not prove that the target application has finished processing or repainting. Observe again when visible final state matters.

## Trust boundary and local data

The token, session record, and captured images live in user-private runtime locations. The MCP server reads a returned image file and emits image content to its client; the JSON CLI reports the path. The native process is the only component that touches operating-system input APIs.

Screenshots, accessibility dumps, and desktop logs can contain credentials or personal information. They are runtime data and are excluded from the source repository.

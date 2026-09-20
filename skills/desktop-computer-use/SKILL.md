---
name: desktop-computer-use
description: >-
  Inspect and operate visible native desktop applications and games with the standalone
  authenticated computer-use CLI. Use for OS-level UI, window, screenshot, keyboard, mouse,
  and paced drag work on Windows or Linux; use browser/page automation for DOM-only tasks.
---

# Desktop Computer Use

Use the bundled `scripts/dcu.mjs` entrypoint (run it as `node <skill-root>/scripts/dcu.mjs`; below,
`dcu` is that command). It starts the packaged native daemon on demand and prints one JSON
response per command. Set `DCU_NATIVE_PATH` only for development when a packaged platform binary
is not present.

## Safe observation loop

1. Run `dcu capabilities`, then `dcu session start` and wait for the visible “computer in use”
   indicator. While active, the indicator shows a top banner, a high-contrast cursor ring, and
   a blue screen-edge border that fades inward. Stop with `dcu session stop` or press `Esc`.
2. Use `dcu list-apps`, `dcu list-windows --app <app>`, and
   `dcu get-app-state --app <app>` before choosing a target. The state response includes a fresh
   observation ID, the window rectangle, a screenshot path, and optional accessibility elements.
3. Use an element index only from the latest observation. Coordinates are window-local logical
   action coordinates. Map a point from the latest screenshot with the returned action transform:
   if `screenshot.actionTransform` is present, use `actionX = (pixelX - offsetX) / scaleX` and
   `actionY = (pixelY - offsetY) / scaleY`. Otherwise use `scaleX`/`scaleY` when present, falling
   back to `scale`; an omitted offset is zero. Never substitute the window's screen `x`/`y` for a
   screenshot offset. Re-observe when the window rectangle, monitor scale, screenshot size, or
   transform changes.
4. After every UI-changing action, inspect its returned observation when requested with
   `--observe screenshot|text|both`, or call `get-app-state` again before selecting another index.
A delivered synthetic input is unverified unless a returned state proves the requested change.

On Linux, run `dcu setup` when installing the skill. It selects the legacy archive for GNOME
42–44 or the modern archive for GNOME 45–50, installs it under the current user's GNOME extension
directory, compiles its schemas when `glib-compile-schemas` is available, and requests enablement
without logging out or restarting the shell. If a command or native library is missing, setup
reports the package names and an apt command; when passwordless sudo is unavailable, it reports
the command for a root shell and leaves `installed` false. Resolve those diagnostics and confirm
`dcu doctor` before starting a session. Doctor remains fail-closed until the native backend and
indicator are ready.

## Commands and input

```text
dcu doctor | capabilities
dcu session start | status | stop
dcu list-apps | list-windows --app APP | get-app-state --app APP [--include-text]
dcu click --app APP (--element-index N | --x X --y Y)
dcu drag --app APP (--from-element-index N --to-element-index N | --from-x X --from-y Y --to-x X --to-y Y)
dcu scroll --app APP --x X --y Y --direction down [--amount 3]
dcu type-text --app APP --text TEXT
dcu press-key --app APP --key KEY
dcu hotkey --app APP --key MODIFIER+KEY
dcu set-value --app APP --element-index N --value TEXT
dcu paste-text --app APP --text TEXT
```

`drag` defaults to `--duration-ms 240 --steps 12 --hold-before-ms 50 --hold-after-ms 50`.
It emits each intermediate move immediately so native applications and games observe a real
button-held drag. The daemon releases held buttons and modifiers when a drag is cancelled, a
session stops, or the connection is lost. Never resend a mutation after a timeout until a fresh
observation establishes whether it arrived.

Use `--text-stdin` or `--value-stdin` for sensitive values. Use `--no-screenshot` only when the
tree or action result is sufficient. Linux requires a logged-in desktop session; Wayland may ask
for RemoteDesktop/ScreenCast portal approval, and X11 actions may require XTest/AT-SPI packages.

## Harness LLM and optional Jev acceleration

The LLM is **you, the model running this skill in the harness**. Do not create or call a
separate LLM API client. At the start of a task, run `dcu agent config` to read the mode
without a model request. If the user explicitly selects a mode, use
`dcu agent config --mode llm|llm+jev` for that task. Never read or print `.env` contents
to determine the mode; the config command reports only non-secret settings.

### `llm`: the harness owns every decision

Follow the safe observation loop above. Plan the task yourself, inspect the current
accessibility tree, choose one current element, and call the corresponding native command.
Prefer `get-app-state --include-text --no-screenshot` for accessible controls. Request a
screenshot when the text is insufficient. Use `--observation-id` and `--window-id` for
element actions and `--observe text` to receive the next state in the same round trip.
Use that returned state for the next decision instead of taking a duplicate observation.
Generate needed text yourself from the user's goal and verify the result after input.
No Jev key or model request is required in this mode. Do not invoke `agent run`,
`agent decide`, or `agent doctor` in the LLM-only workflow: those commands call Jev.

### `llm+jev`: the harness plans, Jev executes short subgoals

1. Observe and plan as above. With a configured Jev key, run `dcu agent doctor` once to
   check the service. If the key is absent or the check fails, report the fallback briefly
   and continue with the harness's direct `llm` workflow.
2. Give Jev one concrete subgoal for one window, usually at most five actions. Do arithmetic,
   interpretation, writing, and cross-window planning yourself. Supply exact field text with
   repeated `--text 'field label=value'`; Jev cannot generate text.
3. Run `dcu agent run --window-id ID --goal 'concrete subgoal' --max-steps 5` with those text
   presets. Reuse the active session. Use `--expect-element-name REGEX` when a visible label
   provides a meaningful success check. Keep destructive-action handling at its default.
4. Inspect the returned `status`, `reason`, `history`, and `detail`, including on a nonzero
   exit. The helper returns control on success, uncertainty, missing text, failure, or budget
   exhaustion. Verify the subgoal against a fresh observation before planning the next one.
5. On uncertainty, missing text, no progress, or a Jev service error, observe afresh and handle
   the next step yourself. Do not repeat the same failing helper call. A budget exhaustion
   means partial work, not proof of failure or success. On an uncertain native transport
   result, inspect state before any further input. On Esc/session loss, stop; do not restart
   the session automatically. A destructive-action stop preserves the user's authorization
   boundary; switching to direct control must not bypass it.

Example, after observing an editable field named `Search` in window `ID`:

```text
dcu agent run --window-id ID --goal 'Enter the supplied query in Search and submit it' --text 'Search=weather in Seoul' --max-steps 5
```

This reduces harness round trips for simple actions by using Jev's batched choice questions
and native action responses with attached observations. It does not guarantee a particular
latency or support screens without accessible controls.

## Jev helper commands

`dcu agent run` takes one plain-language goal and drives a single window until the goal is
reached, blocked, or a budget runs out. It observes the window's accessibility tree, asks a
TypeSafe Jev model which listed control to act on next, executes that one
choice in code, and re-observes to check what changed. The model never emits coordinates or
selectors; it always picks from an enumerated list of controls, keys, chords and regions that
the code built from the observation.

Read this before running it:

1. Model requests send the visible text of the target window — window title, control names and
   values, whatever the accessibility tree exposes — to the configured Jev endpoint.
   Jev uses `https://api.typesafe.ai` by default.
   `--no-values` strips every
   field's value but not its name. Document titles, file names, message subjects: anything a
   control is named after leaves the machine on every step.
2. Esc stops the session at any time. While active, the indicator shows a top banner, a
   high-contrast cursor ring, and a blue screen-edge border, exactly as in the safe observation
   loop above.
3. The loop takes the target window to the foreground on every action it sends, so anything
   typed by hand into another window during a run lands in the agent's target instead.

### Subcommands

```text
dcu agent config [--mode llm|llm+jev] [--env-file FILE]
dcu agent run --goal "..." (--app APP | --window-id ID) [options]
dcu agent decide --goal "..." (--app APP | --window-id ID | --fixture FILE) [options]
dcu agent explain (--app APP | --window-id ID | --fixture FILE) [--goal "..."]
dcu agent doctor
```

**`agent run`** is the loop: observe, decide, guard, act, compare — repeated until the model
reports the goal reached, a guard blocks the step, or a budget runs out. Flags (spellings from
`src/cli.ts` `OPTION_DEFS`):

- `--goal "..."` (required) and `--app APP` or `--window-id ID` (one required). `--app` matches
  the executable name first and the window title second, so a packaged app such as Calculator
  can be named the way its title bar names it.
- `--max-steps N` — default 25, ceiling 100. A step is one delivered action; a gate-retry
  observation or a `busy` backoff spends a decision, not a step.
- `--env-file PATH` — load configuration from an explicit file instead of the skill's `.env`.
- `--dry-run` — observe once, decide once, act zero times; prints the plan instead.
- `--stream` — one NDJSON record per step on stdout ahead of the closing summary line. The
  last line is always the same `{"id":null,"ok":...,"result":...}` envelope, so a reader that
  keeps only the last line still works.
- `--on-destructive stop|confirm|allow` — default `stop`. `confirm` has nowhere to ask (no TTY
  under MCP, and the loop owns the desktop while it runs) and degrades to `stop`.
- `--text field=value` — repeatable. Text for a field the goal's own quoted runs cannot supply;
  matched to a decision's target by its label.
- `--no-values` — strip every candidate's value, not only the ones the redaction pass judges
  sensitive (a name matching password/PIN/OTP/token/card wording, or a 6+ digit value).
- `--start-session` — start a session if none is active, instead of failing with
  `session_required`.
- `--resume-session` — once per run, if the session is lost mid-run (Esc, the 120-second idle
  lease), start a new one and continue rather than stopping.
- `--expect-element-name REGEX` — the objective success check; see the sharp edges below.
- `--min-confidence` / `--min-margin` — gate floors, default 0.55 / 0.15.
- `--max-candidates`, `--timeout-ms`, `--session-id` — tuning and overrides shared with
  `explain` and `decide`.

Prints exactly one JSON line on stdout and exits 0 only when `result.status === "succeeded"`.
Every run appends an NDJSON trace under the runtime directory (`DCU_AGENT_TRACE_DIR` moves it);
the trace never carries the API key, the state payload, or a value the redaction pass stripped.

**`agent decide`** asks for one decision and sends nothing to the desktop. It observes once (or
reads `--fixture`), asks Jev which single action comes next, and prints the answer;
`performedInput` in its result is always `false`. No click, keystroke or scroll is ever sent by
this command.

**`agent explain`** turns one observation into a candidate table offline: no model, no network,
no API key. Observes `--app`/`--window-id`, or works from a recorded observation with
`--fixture FILE`; the candidates it prints are exactly the ones `decide` and `run` build
internally from the same observation.

**`agent config`** reports the harness workflow (`llm` by default) and Jev key presence without
making a model request. `--mode` is a config override for this command only, not a saved setting.
For hybrid mode without a key, it exits 1 with `ready: false` and still returns the mode;
read that JSON and use the direct LLM fallback. `ready` checks credentials, not connectivity.

**`agent doctor`** checks Jev. It reports key presence, host, model, and ping latency.
It never prints the key itself. `DCU_AGENT_MODE` selects the harness workflow, not the
implementation of an explicit Jev helper call.

### Configuration

Set process environment variables or copy this skill's `.env.example` to `.env` and fill in
the provider settings. For the same variable name, process environment variables take
precedence, including explicitly empty values. `DCU_JEV_*` names take precedence over their
legacy aliases even when the two names come from different sources.
Agent commands read only the skill's `.env`, or the file selected by `--env-file` /
`DCU_ENV_FILE`; it does not search arbitrary working directories. Release packaging excludes
real `.env` files.

- `DCU_AGENT_MODE` — `llm` (default) or `llm+jev`. The LLM is the current harness model.
- `TYPESAFE_API_KEY` — Jev key, needed for the hybrid helper.
- `DCU_JEV_API_KEY` — preferred alias for the Jev key; when set it overrides `TYPESAFE_API_KEY`.
- `DCU_JEV_BASE_URL`, `DCU_JEV_MODEL`, `DCU_JEV_TIMEOUT_MS` — Jev-specific settings.

Existing Jev configuration names remain supported:

- `DCU_AGENT_BASE_URL` — Jev base URL, default `https://api.typesafe.ai/v1`.
- `DCU_AGENT_MODEL` — model name, default `jev-latest`.
- `DCU_AGENT_TIMEOUT_MS` — per-request timeout, default 8000.
- `DCU_AGENT_TRACE_DIR` — where the NDJSON trace is written, default `<runtime dir>/agent`.

### Measuring cost and speed

Each run reports duration, action count, decision count, and provider token usage. Its local
trace also records decision latency and native call time. Action calls carry `observe: "text"`,
so the next observation shares the action's native round trip. Compare the same goal and
starting state across repeated trials before drawing performance conclusions.

Raw desktop recordings and historical local benchmarks are not distributed with this skill.
No latency, success-rate, or cost guarantee applies across applications or model versions.

### When it works and when it does not

The loop needs an accessibility tree. A window whose observation carries fewer than 5 elements
and yields no candidates stops with `no_accessible_ui`
(`BARE_TREE_ELEMENTS` in `src/agent/agent.ts`). Games, canvas-drawn apps, and other custom-drawn
UI that paints pixels without populating UI Automation are expected to fall in this bucket by
the same mechanism, but no validation run in this repository has driven a game or a canvas app
to confirm it — say so as a mechanism, not as a measured result, if asked.

Foreground access can block a run before a decision is even asked: a window another app is
holding the foreground against (UWP apps release it reluctantly) fails `activate` after 750 ms
of retry with `focus_denied`, and `SetForegroundWindow` cannot be forced past Windows' own
foreground-switching restriction.

### Failure reasons

`result.reason` on any status other than `succeeded`. From `src/agent/agent.ts` and
`src/agent/questions.ts`; a reason not produced anywhere in the source is not documented here.

| reason | status | means |
|---|---|---|
| `missing_answer`, `malformed_answer`, `unknown_choice`, `low_confidence`, `low_margin` | `blocked` | the gate rejected an answer (`questions.ts` `GateFailureReason`). `low_confidence`/`low_margin` get one retry — 600 ms wait, fresh observation, re-decide — before the run stops; a retried stop carries `detail.retried: true`. |
| `no_progress` | `aborted` | the same action was chosen against the same-looking screen three times running (`NoProgressDetector`). |
| `max_steps`, `max_decisions`, `max_duration`, `max_input_tokens` | `budget_exhausted` | a budget was reached — defaults 25 steps (ceiling 100), steps+10 decisions, 180,000 ms, 400,000 input tokens. |
| `field_not_replaceable` | `blocked` | the daemon reported `pattern_unavailable` on a value-pattern field; typing instead would insert at the caret and corrupt what is there, so the run stops rather than guess. |
| `no_accessible_ui` | `blocked` | see above. |
| `session_lost` | `aborted` | the session died mid-run and `--resume-session` was not passed, or had already been used once this run. |
| `focus_denied` | `failed` | `activate` was refused before any input was sent. |
| `unstable_window` | `aborted` | the daemon reported `stale_observation` twice running for the same action. A single `stale_observation` does **not** stop the run by itself — it silently re-observes and retries; only the second one in a row surfaces as `unstable_window`. |
| `index_desync` | `aborted` | an element index the executor sent no longer resolves (`element_not_found`, `element_required`, `element_not_actionable`) twice running. |
| `transport_unstable` | `aborted` | a mutation whose request bytes were written but whose outcome is unknown — terminal for that call per `references/protocol.md` — happened twice running. |
| `observation_failed` | `failed` | a fresh observation could not be taken. |
| `expectation_unmet` | `failed` | the run reached `succeeded`, but the final observation's elements do not match `--expect-element-name`; the status is downgraded from `succeeded` to `failed` because of it. |
| `text_unavailable` | `blocked` | an `enter_text` decision named a field with no text to give it — the goal's quoted runs are exhausted and no `--text field=value` matched. |

Also seen, not asked for above but worth knowing: `model_reported_blocked` — the model itself
answered "blocked" (`blocked`); `destructive_action` — the destructive guard fired, see the
sharp edges below (`blocked`); `session_required` / `window_not_found` / `session_start_failed`
— the target could not be prepared (`failed`); `agent_state_too_large` — the filtered state
exceeded the 24,000-token estimate ceiling (`failed`); and `goal_reached` — the one success
reason (`succeeded`).

### Result and session handling

- **`--expect-element-name` checks that an element with a matching name exists in the final
  observation — not that the goal was achieved.** It re-observes rather than trusting the last
  snapshot, so it is good for asserting a specific, freshly-generated label is on screen (a
  calculator result: `--expect-element-name "15\s*$"`). It is not a state check: a run that
  stopped early can still report `expectation: met` if that name happens to be visible anywhere
  else in the window — a sidebar link named after the goal's destination, say, even though the
  run never reached it. Check `status: "succeeded"` together with `expectation: "met"`, never
  `expectation` alone.
- **A normal `agent run` saves a returned new session ID for later CLI commands.** The
  result also includes `sessionId` for explicit `--session-id` use. A dry run does not save
  a newly started session; start the session explicitly before a dry run when subsequent
  commands need to share it. An ID does not prove the session is still active: respect Esc
  and session-loss results rather than automatically restarting.

### Writing goals that work

- **Put any text the agent must type inside quotes in the goal.** The decision model chooses a
  field, never text; the executor pulls the typed string from the goal's quoted runs in order,
  then from `--text field=value` once those run out. Measured working example:
  `dcu agent run --window-id <id> --goal '주소창에 "youtube.com" 입력하고 Enter 눌러서 유튜브
  사이트 열기' --max-steps 10`.
- **Name the control you mean.** Once the candidate count is over the cap, the salience ranking
  that decides what survives gives weight to words shared between the goal and a control's
  name; a goal that only describes the action around a control scores lower than one that also
  names it. Measured on a 371-element screen: `왼쪽 메뉴에서 스팸메일함을 연다` scrolled six
  steps and aborted with `no_progress`, while `스팸메일함 열기` succeeded in one step at 0.97
  confidence, because Korean glues the particle onto the noun and the un-stripped goal word
  never matched the candidate label in that historical run. The current tokenizer also
  considers Korean words with common particles removed.

For MCP clients, run `dcu mcp serve`. It uses the official MCP stdio transport and returns native
screenshots as image content in addition to JSON text.

Read [the protocol reference](references/protocol.md) when diagnosing stale observations,
permissions, transport failures, or session ownership.

# Validation

Desktop automation needs several kinds of evidence. A passing unit test proves a different property from a successful native build, and a successful input response does not prove that the target application processed or painted the result.

This guide describes reproducible checks. Raw desktop evidence is runtime data: keep screenshots, accessibility dumps, session files, tokens, and logs outside the repository.

## Validation levels

| Level | What it establishes | Command or location |
|---|---|---|
| TypeScript compile | Public and internal TypeScript types are consistent. | `npm run typecheck` |
| Adapter suite | CLI, MCP, transport, agent, environment, and packaging behavior. | `npm test` |
| Native contract suite | Authentication, sessions, dispatch, request validation, and supported platform contract tests. | CMake build followed by CTest |
| Integration harness | Real transport, extension, capture, input, or cancellation behavior in a controlled environment. | `tests/integration/` |
| Live desktop check | Visible behavior in the target OS session, including final state and cleanup. | Manual or harness-assisted procedure below |

Report each level separately. Do not describe a protocol test or nested compositor test as full host-desktop validation.

## Automated adapter checks

Install locked dependencies and run the complete Node suite:

```powershell
npm ci
npm test
```

For a compile-only check:

```powershell
npm run typecheck
```

`npm test` rebuilds the TypeScript output and skill bundle before running `tests/*.test.js`. The tests use fixtures and local test servers; they do not prove platform permissions or real input delivery.

## Native contract checks

Configure and build the repository, then run CTest:

```powershell
cmake -S . -B build
cmake --build build --config Release --parallel 2
ctest --test-dir build -C Release --output-on-failure
```

On Linux, the root build also enables the portal request-lifetime test when the required GIO development package is available. A skipped optional target must be reported as skipped rather than passed.

The concurrent runtime-token harness exercises first-start coordination separately:

```powershell
node tests/integration/verify-runtime-race.mjs
```

## Native drag fixture

`tests/integration/drag-fixture.cpp` builds a disposable window that records pointer down, movement, and release as flushed NDJSON. The Windows fixture and Linux GTK fixture use the same blue-to-green drag task.

Build the fixture in its own directory:

```powershell
cmake -S tests/integration -B build/integration
cmake --build build/integration --config Release
```

Use an output log outside the repository. Then:

1. Launch the fixture in the real graphical session.
2. Start a Desktop Computer Use session and obtain a fresh observation of the fixture.
3. Confirm that the visible session indicators appear.
4. Derive window-local coordinates from the observation and drag from the blue target to the green target.
5. Observe again and verify the completed-drag count.
6. Analyze the event log with `node tests/integration/analyze-drag.mjs <log-path>`.
7. Repeat with a long drag, stop the session during the action, and verify a prompt native button release and an inactive session.

Do not assume client-area fixture coordinates equal the backend's window-local coordinates. Window frames, DPI scaling, and monitor geometry can change the transform.

## MCP image smoke test

The MCP smoke needs a built native executable and a visible test window. Use the disposable drag fixture rather than a personal application:

```powershell
$env:DCU_NATIVE_PATH = (Resolve-Path build/Release/desktop-computer-use-native.exe)
$env:DCU_SMOKE_WINDOW_ID = 'hwnd:123456'
node tests/mcp-native-smoke.mjs
```

The test starts a private runtime when `DCU_RUNTIME_DIR` is unset, captures the target through MCP, validates JPEG or PNG bytes, and stops the session and daemon. On Linux, use the Linux executable path and platform window ID.

## Linux integration harnesses

The reusable Linux checks are grouped by boundary:

- `tests/integration/eis/` checks the real libei/EIS sender sequence and cancellation against a local test server.
- `tests/integration/gnome-container/` checks the GNOME 42–44 and 45–50 extension variants in an isolated nested compositor.
- `tests/integration/linux-native/` exercises the Linux daemon, CLI, screenshot route, drag, cancellation, restart, and cleanup in an isolated X11 environment.

Follow the README in each directory for prerequisites and commands. Nested compositor results establish behavior inside that environment; they do not establish portal approval or input delivery in the host user's logged-in Wayland session.

## Live desktop acceptance

For a change that affects capture, focus, input, cancellation, indicators, or permissions, record all of the following in the pull request:

- operating system and version;
- desktop or shell version;
- native build type and compiler;
- exact commands and relevant non-secret environment settings;
- target fixture and requested action;
- observed final application state;
- session state after normal stop and emergency stop;
- limitations or paths that were not exercised.

Test these edge cases when the change touches them:

- fractional DPI and multiple monitors;
- window movement or resize between observation and action;
- stale element indexes;
- cancellation during a held mouse button;
- client disconnect and daemon termination;
- stop followed by session restart;
- final repaint after input delivery;
- unavailable accessibility trees;
- portal denial or permission revocation.

## Evidence handling

Write generated evidence to a temporary or ignored directory outside the checkout. Review every artifact before sharing it. Redact user names, absolute paths, window titles, document text, URLs, tokens, and identifiers.

A useful public report contains the environment, exact command, pass or failure condition, measured result, and known limit. A path to a private or deleted artifact is not public evidence, so do not cite it as if readers can inspect it.

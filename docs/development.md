# Development

Run all commands from the repository root unless a section says otherwise.

## Prerequisites

All platforms require:

- Node.js 22 or newer
- npm
- CMake 3.22 or newer
- A C++20 compiler

Windows builds use Visual Studio 2022 with the Desktop development with C++ workload. Linux builds need a compiler, Ninja or Make, pkg-config, GLib/GIO, X11, XTest, XRandR, JPEG, PNG, PipeWire, AT-SPI, and nlohmann-json development packages. The GitHub Actions package list in `.github/workflows/build.yml` is the maintained Ubuntu reference.

## Build the TypeScript client and skill

Install the locked dependencies and build both the TypeScript output and bundled skill launcher:

```powershell
npm ci
npm run build:all
```

`npm run build` compiles `src/` into `dist/`. `npm run bundle` creates `skills/desktop-computer-use/scripts/dcu.mjs` and refreshes the bundled dependency license report.

For a compile-only check that does not emit TypeScript output:

```powershell
npm run typecheck
```

## Build the native daemon

Configure, build, and run the native contract tests:

```powershell
cmake -S . -B build
cmake --build build --config Release --parallel 2
ctest --test-dir build -C Release --output-on-failure
```

Windows multi-configuration generators normally write the executable to `build/Release/desktop-computer-use-native.exe`. Single-configuration Linux generators normally write `build/desktop-computer-use-native`.

Set `DCU_NATIVE_PATH` while developing so the CLI uses that binary:

```powershell
$env:DCU_NATIVE_PATH = (Resolve-Path build/Release/desktop-computer-use-native.exe)
node skills/desktop-computer-use/scripts/dcu.mjs doctor
```

On Linux, point the variable at the Linux executable instead.

## Run automated tests

The main suite rebuilds the adapters and runs the Node test files under `tests/`:

```powershell
npm test
```

Run a focused Node test with the built-in test runner:

```powershell
node --test tests/cli.test.js
```

Native contract tests run through CTest as shown above. Reusable desktop and transport harnesses live under `tests/integration/`; follow [Validation](validation.md) before running anything that opens a window or sends input.

The real MCP image smoke test needs a built native daemon and a visible target window:

```powershell
$env:DCU_NATIVE_PATH = (Resolve-Path build/Release/desktop-computer-use-native.exe)
$env:DCU_SMOKE_WINDOW_ID = 'hwnd:123456'
node tests/mcp-native-smoke.mjs
```

The smoke test owns an isolated runtime unless `DCU_RUNTIME_DIR` is already set. It starts and stops its session and daemon, captures the requested window through MCP, and validates the returned image bytes.

## Configure agent development

Copy the environment template and keep the resulting `.env` file local:

```powershell
Copy-Item skills/desktop-computer-use/.env.example skills/desktop-computer-use/.env
```

The preferred Jev variables are:

| Variable | Default | Purpose |
|---|---|---|
| `DCU_AGENT_MODE` | `llm` | Selects the harness workflow: `llm` or `llm+jev`. |
| `DCU_JEV_API_KEY` | none | Enables Jev requests in hybrid mode. |
| `DCU_JEV_BASE_URL` | `https://api.typesafe.ai/v1` | Selects the Jev endpoint. |
| `DCU_JEV_MODEL` | `jev-latest` | Selects the Jev model. |
| `DCU_JEV_TIMEOUT_MS` | `8000` | Sets the per-request timeout. |
| `DCU_AGENT_TRACE_DIR` | runtime agent directory | Moves the NDJSON agent trace. |
| `DCU_ENV_FILE` | skill-root `.env` | Selects another environment file. |

The legacy `TYPESAFE_API_KEY` and `DCU_AGENT_BASE_URL`, `DCU_AGENT_MODEL`, and `DCU_AGENT_TIMEOUT_MS` names remain supported. A `DCU_JEV_*` value wins over its legacy alias even when the preferred value is empty or came from the selected file. For the same variable name, an existing process environment value wins over the file value.

`agent config` is offline and never prints the key. `agent doctor` makes a small Jev request and reports the host, model, and latency without exposing the key.

## Package the skill

Build both GNOME extension archives first:

```powershell
pwsh -File extensions/package.ps1
```

For a complete package, arrange the native artifacts as follows:

```text
artifacts/native/
├─ linux-x64/desktop-computer-use-native
└─ win32-x64/desktop-computer-use-native.exe

extensions/dist/
├─ desktop-computer-use-gnome-legacy.zip
└─ desktop-computer-use-gnome-modern.zip
```

Then assemble the release directory:

```powershell
npm run package:skill -- --native-dir artifacts/native --extension-dir extensions/dist
```

The default output is `dist/desktop-computer-use-skill`. Packaging rejects incomplete native or extension inputs. Use `--adapters-only` only for CLI and MCP development where native artifacts are intentionally absent.

## Generated and private files

Do not commit:

- `.env` files or credentials
- compiled binaries and package output
- screenshots and screen recordings
- accessibility-tree captures
- runtime directories, tokens, traces, or execution logs
- unreviewed fixtures copied from a real desktop

When a regression needs a fixture, reduce and sanitize it first. `scripts/sanitize-fixtures.mjs` supports the reviewed UI Automation fixture workflow.

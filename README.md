# Desktop Computer Use

[![Build](https://github.com/j-token/desktop-computer-use/actions/workflows/build.yml/badge.svg)](https://github.com/j-token/desktop-computer-use/actions/workflows/build.yml)

Desktop Computer Use is a native desktop automation daemon, a JSON CLI and MCP server, and a Codex skill. It gives an LLM harness a small, authenticated local interface for observing and controlling regular desktop applications on Windows and Linux.

The project supports two harness workflows:

- `llm`: the LLM already running the skill observes the desktop, chooses an action, and sends it through the CLI.
- `llm+jev`: the same harness keeps control of planning and text, while [Jev](https://github.com/browser-use/jev-ultrafast) executes short, bounded subgoals against the accessibility tree.

Here, **LLM means the model in the harness running the skill**. The project does not create a second LLM client or require a separate LLM API key. Jev is optional and only needs a key in `llm+jev` mode.

## What it provides

- A C++20 native daemon for desktop observation and input.
- A TypeScript CLI and MCP server over an authenticated local NDJSON transport.
- A harness-first Codex skill with direct LLM and hybrid LLM + Jev workflows.
- Visible session indicators and an emergency stop with `Esc`.
- Windows UI Automation and Linux accessibility integration for standard application controls.

The agent workflows require an accessibility tree. Games, canvas-only interfaces, and other custom-drawn screens that do not expose accessible controls are outside their supported scope.

## Quick start from source

Requirements:

- Node.js 22 or newer
- CMake 3.22 or newer
- A C++20 compiler

Build the TypeScript client and the skill bundle:

```powershell
npm ci
npm run build:all
node skills/desktop-computer-use/scripts/dcu.mjs --help
```

Build and test the native daemon:

```powershell
cmake -S . -B build
cmake --build build --config Release --parallel 2
ctest --test-dir build -C Release --output-on-failure
```

Point the CLI at the locally built executable, then check platform readiness:

```powershell
$env:DCU_NATIVE_PATH = (Resolve-Path build/Release/desktop-computer-use-native.exe)
node skills/desktop-computer-use/scripts/dcu.mjs setup
node skills/desktop-computer-use/scripts/dcu.mjs doctor
```

On single-configuration Linux generators, the executable is normally `build/desktop-computer-use-native`. Platform dependencies and full build commands are in [Development](docs/development.md).

## Configure the harness workflow

Copy the checked-in template to `.env` in the skill directory, or set the same values as process environment variables:

```powershell
Copy-Item skills/desktop-computer-use/.env.example skills/desktop-computer-use/.env
```

For direct harness control:

```dotenv
DCU_AGENT_MODE=llm
```

For the hybrid workflow:

```dotenv
DCU_AGENT_MODE=llm+jev
DCU_JEV_API_KEY=your-key
```

`TYPESAFE_API_KEY` remains a supported legacy alias. `DCU_JEV_API_KEY` takes precedence when both names are present, including when the preferred name came from the file and the legacy name came from the process. For the same variable name, a process environment value takes precedence over the file value. Agent commands load `skills/desktop-computer-use/.env` by default; use `--env-file PATH` or `DCU_ENV_FILE` to select another file. The CLI does not search the current working directory for credentials.

Inspect the resolved mode without making a network request:

```powershell
node skills/desktop-computer-use/scripts/dcu.mjs agent config
```

With `llm+jev` configured, test the Jev connection:

```powershell
node skills/desktop-computer-use/scripts/dcu.mjs agent doctor
```

Load `desktop-computer-use` in the harness and ask it to operate the desktop. The skill reads `agent config` and follows the selected workflow. The standalone `agent run` and `agent decide` commands are Jev helpers; they do not start a harness LLM.

## Install the skill

For source-checkout development, build the bundle first and install the skill by copy:

```powershell
npm ci
npm run build:all
npx skills add . --skill desktop-computer-use --copy
```

The source checkout does not track compiled native binaries. Build the daemon and set `DCU_NATIVE_PATH`, or assemble a complete package before installing. From the assembled package directory, use the same `npx skills add . --skill desktop-computer-use --copy` command; the package includes the Windows and Linux binaries and both GNOME extension archives.

## Architecture

```text
LLM harness
  └─ desktop-computer-use skill
       ├─ direct native actions                         (llm)
       └─ bounded accessibility subgoal → Jev helper   (llm+jev)
              │
              ▼
       TypeScript CLI / MCP server
              │  authenticated local NDJSON
              ▼
       native daemon
              │
              ├─ Windows UI Automation and Win32 input/capture
              └─ Linux AT-SPI, portal/EIS, X11, and GNOME integration
```

The native executable is the only component that injects desktop input. A session must be active before input is accepted. Each session shows a top banner, a high-contrast cursor marker, and a blue screen-edge indicator. Press `Esc` or run `dcu session stop` to stop the session and release held input.

See [Architecture](docs/architecture.md) for component boundaries, state flow, and failure handling. The wire and recovery contract is in [the skill protocol reference](skills/desktop-computer-use/references/protocol.md).

## Repository layout

```text
.
├─ .github/              GitHub Actions and contribution templates
├─ docs/                 architecture, development, and validation guides
├─ extensions/           GNOME Shell indicator implementations and packaging
├─ native/               shared C++ daemon plus Windows and Linux backends
├─ scripts/              build, packaging, and fixture-sanitizing utilities
├─ skills/               installable desktop-computer-use skill
├─ src/                  TypeScript CLI, MCP server, transport, and agent helpers
└─ tests/
   ├─ fixtures/          reviewed test fixtures
   └─ integration/       reusable native and desktop integration harnesses
```

Generated builds, packaged artifacts, local credentials, screenshots, desktop recordings, and execution logs do not belong in source control.

## Build, test, and package

Run the TypeScript, CLI, MCP, transport, and packaging tests with:

```powershell
npm test
```

Create an adapter-only skill package for CLI and MCP development:

```powershell
npm run package:skill -- --adapters-only
```

A complete skill package also requires Windows and Linux native binaries plus both GNOME extension archives. See [Development](docs/development.md#package-the-skill) for the expected artifact layout and command.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before sending a change. It lists the focused checks for TypeScript, native, packaging, and live desktop changes. Reproducible desktop test procedures are documented in [Validation](docs/validation.md); raw screenshots, accessibility dumps, session files, credentials, and execution logs must stay outside the repository.

Third-party components and their licenses are recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). `npm run bundle` generates the dependency license report shipped with the skill.

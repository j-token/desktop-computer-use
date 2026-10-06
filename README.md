# Desktop Computer Use

[![Build](https://github.com/j-token/desktop-computer-use/actions/workflows/build.yml/badge.svg)](https://github.com/j-token/desktop-computer-use/actions/workflows/build.yml)

Desktop Computer Use is a native desktop automation daemon, a JSON CLI and MCP server, and a Codex skill. It gives an LLM harness a small, authenticated local interface for observing and controlling regular desktop applications on Windows and Linux.

The LLM already running the skill observes the desktop, chooses every action, and sends it through the CLI or MCP server. The project does not create a second model client or require an API key.

## What it provides

- A C++20 native daemon for desktop observation and input.
- A TypeScript CLI and MCP server over an authenticated local NDJSON transport.
- A harness-first skill in which the running LLM makes every decision.
- Visible session indicators and an emergency stop with `Esc` pressed twice within 1 second.
- Windows UI Automation and Linux accessibility integration for standard application controls.

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
       └─ direct native actions
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

The native executable is the only component that injects desktop input. A session must be active before input is accepted. Each session shows a top banner, a high-contrast cursor marker, and a blue screen-edge indicator. Press `Esc` twice within 1 second or run `dcu session stop` to stop the session and release held input; a single `Esc` only shows a reminder on the banner.

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
├─ src/                  TypeScript CLI, MCP server, and transport
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

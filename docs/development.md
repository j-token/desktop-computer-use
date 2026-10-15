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

CLI 회귀 테스트는 실제 경로뿐 아니라 디렉터리 링크를 통한 실행도 검증합니다. Windows에서는 관리자 권한 없이 생성할 수 있는 junction을, Linux에서는 디렉터리 symlink를 사용하여 컴파일된 CLI와 번들의 `--help` 출력 및 잘못된 옵션의 JSON 오류를 확인합니다. 격리된 로컬 fixture로 `capabilities`의 요청 전달과 정상 JSON 출력도 검증하며, 네이티브 데몬이나 GUI 세션은 시작하지 않습니다. CI는 Windows와 Linux 양쪽에서 어댑터 테스트를 실행합니다.

`npx skills`가 만든 `.claude/skills` 등의 경로는 실제 설치 디렉터리를 가리키는 링크일 수 있습니다. Node는 기본적으로 main 모듈의 링크를 실제 경로로 해석하지만 `process.argv[1]`에는 호출 경로가 남습니다. CLI 진입점에서 이 경로들을 realpath로 해석하지 않고 URL 문자열만 비교하면 모든 명령이 출력 없이 종료 코드 `0`으로 끝날 수 있습니다. 이 증상이 발생하는 이전 설치본은 수정된 번들을 `npm run build:all`로 생성한 뒤 갱신해야 합니다.

Native contract tests run through CTest as shown above. Reusable desktop and transport harnesses live under `tests/integration/`; follow [Validation](validation.md) before running anything that opens a window or sends input.

The real MCP image smoke test needs a built native daemon and a visible target window:

```powershell
$env:DCU_NATIVE_PATH = (Resolve-Path build/Release/desktop-computer-use-native.exe)
$env:DCU_SMOKE_WINDOW_ID = 'hwnd:123456'
node tests/mcp-native-smoke.mjs
```

The smoke test owns an isolated runtime unless `DCU_RUNTIME_DIR` is already set. It starts and stops its session and daemon, captures the requested window through MCP, and validates the returned image bytes.

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

# Contributing

Thank you for improving Desktop Computer Use. Keep changes focused, explain the user-visible behavior they change, and include the smallest validation that proves the result.

## Set up the repository

Install Node.js 22 or newer, CMake 3.22 or newer, and a C++20 toolchain. Then run:

```powershell
npm ci
npm run build:all
npm test
```

Native build requirements and platform-specific commands are documented in [docs/development.md](docs/development.md).

## Choose the relevant checks

- TypeScript, CLI, MCP, transport, or agent changes: run `npm run typecheck` and `npm test`.
- Common or platform-native changes: configure and build with CMake, then run `ctest --test-dir build -C Release --output-on-failure`.
- Packaging changes: run the package tests and assemble the relevant adapter-only or complete package.
- Desktop behavior changes: run the smallest applicable harness under `tests/integration/` and record the environment, exact command, and observed outcome in the pull request.

Do not treat a successful input API response as proof that a visible action completed. For capture, focus, drag, cancellation, or session-indicator changes, verify the resulting application state and the session cleanup path. See [docs/validation.md](docs/validation.md).

## Keep runtime data private

Never commit API keys, `.env` files, runtime tokens, raw screenshots, accessibility dumps, screen recordings, session files, or execution logs. These can expose personal data even when filenames look anonymous. Store test evidence outside the repository and attach only reviewed, redacted material when a pull request needs it.

Fixtures under `tests/fixtures/` must be reduced to the behavior under test and reviewed for user names, paths, document contents, tokens, and other identifying data.

## Pull requests

Describe:

1. The concrete problem and the resulting behavior.
2. The affected platforms and components.
3. The commands you ran and their results.
4. Any live desktop case that remains untested.

Update public documentation when commands, configuration, environment variables, output fields, or supported behavior change. Keep generated files in sync when the build intentionally tracks them.

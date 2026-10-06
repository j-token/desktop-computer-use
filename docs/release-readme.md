# Desktop Computer Use 0.1.0

This package contains the standalone CLI, Windows x64 and Linux x64 native executables, and GNOME 42–50 extension archives. Node.js 22 or newer is required; no C++ SDK or Orca installation is needed.

Install in Claude Code as a plugin:

```text
/plugin marketplace add j-token/desktop-computer-use
/plugin install desktop-computer-use@desktop-computer-use
```

Install for another agent as a skill:

```text
npx skills add https://github.com/j-token/desktop-computer-use/tree/release/skills/desktop-computer-use
```

From an extracted copy of this package, `npx skills add . --skill desktop-computer-use --copy` works the same way. The `skills/desktop-computer-use/` directory is self-contained, so either installer gets the native binaries.

Read [the skill instructions](skills/desktop-computer-use/SKILL.md). You can also run the bundled launcher directly from this directory:

```text
node skills/desktop-computer-use/scripts/dcu.mjs setup
node skills/desktop-computer-use/scripts/dcu.mjs doctor
node skills/desktop-computer-use/scripts/dcu.mjs session start
node skills/desktop-computer-use/scripts/dcu.mjs session stop
```

An active session shows an Esc banner, highlighted cursor, and blue edges fading inward. Press Esc twice within 1 second to stop. Linux setup reports required packages and extension activation steps; it does not restart your login session. Wayland permission prompts must be approved in the graphical session.

See the [protocol](skills/desktop-computer-use/references/protocol.md) and [third-party notices](skills/desktop-computer-use/references/THIRD_PARTY_NOTICES.md). The optional `mcp serve` command uses the same backend and returns screenshot image content.

Windows native app and Farmer game drags were verified. Actual Ubuntu host Wayland portal operation remains pending extension activation; isolated GNOME extension tests do not establish host input support. Consult the accompanying validation report when present for measured coverage and remaining gaps.

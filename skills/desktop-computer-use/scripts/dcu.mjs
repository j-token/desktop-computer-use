#!/usr/bin/env node

import {
  DcuClient,
  DcuError,
  SESSIONLESS_METHODS,
  TOGGLE_KEY_ALIASES,
  WINDOW_TARGET_METHODS,
  clearSession,
  compactResponse,
  endpointForToken,
  ensureRuntime,
  ensureToken,
  errorToResponse,
  isValidModifiers,
  readSession,
  resolveNativePath,
  resolveRuntimePaths,
  resolveSkillRoot,
  usageInstructions,
  writeSession
} from "./dcu-chunk-SSALKMIF.mjs";

// src/cli.ts
import { stdin, stdout } from "node:process";
import { realpathSync } from "node:fs";
import { resolve as resolve2 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// src/setup.ts
import { execFile } from "node:child_process";
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
var EXTENSION_UUID = "desktop-computer-use@local";
var COMMAND_TIMEOUT_MS = 8e3;
var APT_TIMEOUT_MS = 6e4;
var COMMAND_PACKAGES = {
  "gnome-shell": "gnome-shell",
  "gnome-extensions": "gnome-shell",
  gsettings: "libglib2.0-bin",
  "glib-compile-schemas": "libglib2.0-bin",
  unzip: "unzip",
  ldd: "libc-bin",
  "wl-copy": "wl-clipboard",
  xclip: "xclip"
};
var LIBRARY_PACKAGES = [
  { pattern: /^libgio-2\.0\.so(?:\.|$)/, packageName: "libglib2.0-0" },
  { pattern: /^libgobject-2\.0\.so(?:\.|$)/, packageName: "libglib2.0-0" },
  { pattern: /^libglib-2\.0\.so(?:\.|$)/, packageName: "libglib2.0-0" },
  { pattern: /^libX11\.so(?:\.|$)/, packageName: "libx11-6" },
  { pattern: /^libXtst\.so(?:\.|$)/, packageName: "libxtst6" },
  { pattern: /^libXrandr\.so(?:\.|$)/, packageName: "libxrandr2" },
  { pattern: /^libjpeg\.so(?:\.|$)/, packageName: "libjpeg-turbo8" },
  { pattern: /^libpng16\.so(?:\.|$)/, packageName: "libpng16-16" },
  { pattern: /^libpipewire-0\.3\.so(?:\.|$)/, packageName: "libpipewire-0.3-0" },
  { pattern: /^libatspi\.so(?:\.|$)/, packageName: "at-spi2-core" }
];
function parseGnomeShellMajor(shellVersion) {
  if (!shellVersion) return void 0;
  const match = shellVersion.match(/(?:GNOME\s+Shell\s+)?(\d+)(?:\.\d+)?/i);
  if (!match) return void 0;
  const major = Number(match[1]);
  return Number.isSafeInteger(major) ? major : void 0;
}
function selectGnomeVariant(shellVersion) {
  const major = parseGnomeShellMajor(shellVersion);
  if (major === void 0) return void 0;
  if (major > 50) return void 0;
  if (major >= 45) return "modern";
  if (major >= 42) return "legacy";
  return void 0;
}
function commandOutput(result) {
  return [result.stderr.trim(), result.stdout.trim()].filter(Boolean).join(" ").slice(0, 2e3);
}
function commandFailure(result, fallback) {
  if (result.notFound) return `${result.command} is not installed`;
  if (result.timedOut) return `${result.command} timed out`;
  return commandOutput(result) || `${fallback} (exit code ${result.exitCode ?? "unknown"})`;
}
function runCommand(command, args, timeoutMs = COMMAND_TIMEOUT_MS) {
  return new Promise((resolveResult) => {
    execFile(
      command,
      args,
      { encoding: "utf8", maxBuffer: 1e6, timeout: timeoutMs, windowsHide: true },
      (error, stdoutText, stderrText) => {
        const processError = error;
        const exitCode = typeof processError?.code === "number" ? processError.code : null;
        resolveResult({
          command,
          args,
          ok: !processError,
          notFound: processError?.code === "ENOENT",
          timedOut: processError?.code === "ETIMEDOUT" || processError?.killed === true,
          stdout: String(stdoutText ?? ""),
          stderr: String(stderrText ?? ""),
          exitCode
        });
      }
    );
  });
}
function uniqueStrings(values) {
  return [...new Set(values)];
}
function packageForLibrary(libraryName) {
  return LIBRARY_PACKAGES.find((entry) => entry.pattern.test(libraryName))?.packageName;
}
function parseMissingLibraries(output2) {
  const missing = [];
  for (const line of output2.split(/\r?\n/)) {
    const match = line.match(/^\s*(\S+)\s*=>\s*not found\s*$/i);
    if (match) missing.push(match[1]);
  }
  return uniqueStrings(missing);
}
function commandProbeDefinitions() {
  const clipboardCommand = process.env.XDG_SESSION_TYPE === "wayland" ? "wl-copy" : "xclip";
  return [
    { name: "gnome-shell", args: ["--version"] },
    { name: "gnome-extensions", args: ["help"] },
    { name: "gsettings", args: ["--version"] },
    { name: "glib-compile-schemas", args: ["--version"] },
    { name: "unzip", args: ["-v"] },
    { name: "ldd", args: ["--version"] },
    { name: clipboardCommand, args: ["--version"] }
  ];
}
async function collectPrerequisites(nativePath) {
  const commandResults = /* @__PURE__ */ new Map();
  const probes = await Promise.all(
    commandProbeDefinitions().map(async (definition) => {
      const result = await runCommand(definition.name, definition.args);
      commandResults.set(definition.name, result);
      return result;
    })
  );
  const shellProbe = commandResults.get("gnome-shell");
  const shellOutput = shellProbe && shellProbe.ok ? `${shellProbe.stdout} ${shellProbe.stderr}`.trim() : void 0;
  const shellVersion = shellOutput || void 0;
  const shellMajor = parseGnomeShellMajor(shellVersion);
  const gsettingsProbe = commandResults.get("gsettings");
  const disabledSetting = gsettingsProbe?.ok ? await runCommand("gsettings", ["get", "org.gnome.shell", "disable-user-extensions"]) : void 0;
  let userExtensionsDisabled = null;
  if (disabledSetting?.ok) {
    if (disabledSetting.stdout.trim() === "true") userExtensionsDisabled = true;
    if (disabledSetting.stdout.trim() === "false") userExtensionsDisabled = false;
  }
  const missingCommands = probes.filter((result) => result.notFound).map((result) => result.command);
  const missingLibraries = [];
  if (nativePath && commandResults.get("ldd")?.notFound !== true) {
    const lddResult2 = await runCommand("ldd", [nativePath]);
    commandResults.set("ldd:native", lddResult2);
    missingLibraries.push(...parseMissingLibraries(`${lddResult2.stdout}
${lddResult2.stderr}`));
  }
  const unmappedLibraries = missingLibraries.filter((name) => !packageForLibrary(name));
  const aptPackages = uniqueStrings([
    ...missingCommands.map((command) => COMMAND_PACKAGES[command]).filter(Boolean),
    ...missingLibraries.map(packageForLibrary).filter((name) => Boolean(name))
  ]);
  const lddResult = commandResults.get("ldd:native");
  const nativeDependenciesReady = nativePath === void 0 ? null : lddResult?.ok === true && missingLibraries.length === 0;
  const linkedRuntimeReady = missingCommands.length === 0 && nativeDependenciesReady !== false ? nativeDependenciesReady : false;
  return {
    shellVersion,
    shellMajor,
    userExtensionsDisabled,
    commandResults,
    missingCommands: uniqueStrings(missingCommands),
    missingLibraries: uniqueStrings(missingLibraries),
    unmappedLibraries,
    aptPackages,
    linkedRuntimeReady
  };
}
function runningAsRoot() {
  return typeof process.getuid === "function" && process.getuid() === 0;
}
function aptInstallCommand(packages, privileged) {
  if (packages.length === 0) return null;
  const packageList = packages.join(" ");
  const prefix = runningAsRoot() ? "" : privileged ? "sudo " : "";
  const command = `${prefix}apt-get update && ${prefix}apt-get install -y ${packageList}`;
  if (!privileged && !runningAsRoot()) {
    return `${command}  # run as root; passwordless sudo is unavailable`;
  }
  return command;
}
async function prepareRuntimePackages(state, nativePath) {
  if (state.aptPackages.length === 0) {
    return { attempted: false, installed: true, command: null, state };
  }
  const sudoProbe = runningAsRoot() ? void 0 : await runCommand("sudo", ["-n", "true"]);
  const privileged = runningAsRoot() || sudoProbe?.ok === true;
  const command = aptInstallCommand(state.aptPackages, privileged);
  const aptProbe = await runCommand("apt-get", ["--version"]);
  if (!privileged || aptProbe.notFound) {
    const reason = aptProbe.notFound ? "apt-get is not installed" : "passwordless sudo is unavailable; run the reported apt command as root";
    return { attempted: false, installed: false, command, error: reason, state };
  }
  const prefix = runningAsRoot() ? [] : ["sudo"];
  const updateResult = await runCommand(
    prefix.length > 0 ? "sudo" : "apt-get",
    prefix.length > 0 ? ["-n", "apt-get", "update"] : ["update"],
    APT_TIMEOUT_MS
  );
  if (!updateResult.ok) {
    return {
      attempted: true,
      installed: false,
      command,
      error: commandFailure(updateResult, "apt-get update failed"),
      state
    };
  }
  const installResult = await runCommand(
    prefix.length > 0 ? "sudo" : "apt-get",
    prefix.length > 0 ? ["-n", "apt-get", "install", "-y", ...state.aptPackages] : ["install", "-y", ...state.aptPackages],
    APT_TIMEOUT_MS
  );
  if (!installResult.ok) {
    return {
      attempted: true,
      installed: false,
      command,
      error: commandFailure(installResult, "apt-get install failed"),
      state
    };
  }
  return {
    attempted: true,
    installed: true,
    command,
    state: await collectPrerequisites(nativePath)
  };
}
function extensionArchiveRoots(skillRoot) {
  return uniqueStrings([
    join(skillRoot, "extensions"),
    join(skillRoot, "extensions", "dist"),
    join(skillRoot, "..", "..", "extensions", "dist"),
    join(skillRoot, "..", "..", "extensions"),
    join(process.cwd(), "extensions", "dist"),
    join(process.cwd(), "extensions")
  ].map((path) => resolve(path)));
}
async function findArchive(skillRoot, variant) {
  const archiveName = `desktop-computer-use-gnome-${variant}.zip`;
  for (const root of extensionArchiveRoots(skillRoot)) {
    const candidate = join(root, archiveName);
    try {
      if ((await stat(candidate)).isFile()) return candidate;
    } catch {
    }
  }
  return void 0;
}
async function findArchives(skillRoot) {
  const archives = [];
  for (const variant of ["modern", "legacy"]) {
    const archive = await findArchive(skillRoot, variant);
    if (archive) archives.push(archive);
  }
  return archives;
}
function userExtensionPaths() {
  const dataHome = process.env.XDG_DATA_HOME ? resolve(process.env.XDG_DATA_HOME) : join(homedir(), ".local", "share");
  const root = resolve(dataHome, "gnome-shell", "extensions");
  const destination = resolve(root, EXTENSION_UUID);
  if (destination === root || !destination.startsWith(`${root}${sep}`)) {
    throw new DcuError("invalid_environment", "GNOME extension destination escaped the user extension directory");
  }
  return { root, destination };
}
async function isDirectory(path) {
  try {
    return (await lstat(path)).isDirectory();
  } catch {
    return false;
  }
}
async function assertNoSymlinks(root) {
  const entries = await readdir(root, { withFileTypes: true });
  for (const entry of entries) {
    const entryPath = join(root, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error(`Refusing symbolic link in extension archive: ${entry.name}`);
    }
    if (entry.isDirectory()) await assertNoSymlinks(entryPath);
  }
}
function safeArchiveEntry(entry) {
  const normalized = entry.replaceAll("\\", "/");
  if (!normalized || normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized)) return false;
  return !normalized.split("/").includes("..");
}
async function extractedExtensionRoot(stage) {
  const directMetadata = join(stage, "metadata.json");
  if (await isRegularFile(directMetadata)) return stage;
  const nestedDestination = join(stage, EXTENSION_UUID);
  if (await isRegularFile(join(nestedDestination, "metadata.json"))) return nestedDestination;
  const children = await readdir(stage, { withFileTypes: true });
  const directories = children.filter((entry) => entry.isDirectory());
  if (directories.length === 1) {
    const candidate = join(stage, directories[0].name);
    if (await isRegularFile(join(candidate, "metadata.json"))) return candidate;
  }
  throw new Error("Extension archive does not contain metadata.json at its root");
}
async function isRegularFile(path) {
  try {
    return (await lstat(path)).isFile();
  } catch {
    return false;
  }
}
async function validateExtensionMetadata(root) {
  const metadataPath = join(root, "metadata.json");
  const metadata = JSON.parse(await readFile(metadataPath, "utf8"));
  if (metadata.uuid !== EXTENSION_UUID) {
    throw new Error(`Extension metadata uuid must be ${EXTENSION_UUID}`);
  }
}
async function installFromZip(archive, destination) {
  const listing = await runCommand("unzip", ["-Z1", archive], 15e3);
  if (!listing.ok) throw new Error(commandFailure(listing, "Unable to inspect the GNOME extension archive"));
  const entries = listing.stdout.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean);
  if (entries.length === 0 || entries.some((entry) => !safeArchiveEntry(entry))) {
    throw new Error("Refusing an unsafe or empty GNOME extension archive");
  }
  const stage = await mkdtemp(join(tmpdir(), "dcu-extension-"));
  try {
    const extracted = await runCommand("unzip", ["-q", archive, "-d", stage], 15e3);
    if (!extracted.ok) throw new Error(commandFailure(extracted, "Unable to extract the GNOME extension archive"));
    const extensionRoot = await extractedExtensionRoot(stage);
    await assertNoSymlinks(extensionRoot);
    await validateExtensionMetadata(extensionRoot);
    const existing = await lstat(destination).catch((error) => {
      const code = error.code;
      if (code === "ENOENT") return void 0;
      throw error;
    });
    if (existing?.isSymbolicLink()) {
      throw new Error("Refusing to replace a symbolic-link GNOME extension destination");
    }
    await mkdir(dirname(destination), { recursive: true, mode: 448 });
    await rm(destination, { recursive: true, force: true });
    await cp(extensionRoot, destination, { recursive: true, force: true });
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}
async function installExtension(archive, variant, prerequisites, destination) {
  if (!variant) {
    return {
      attempted: false,
      method: null,
      installed: false,
      error: "Unsupported or unknown GNOME Shell version; cannot select an extension variant"
    };
  }
  if (!archive) {
    return {
      attempted: false,
      method: null,
      installed: false,
      error: `Packaged ${variant} GNOME extension archive was not found`
    };
  }
  const gnomeExtensions = prerequisites.commandResults.get("gnome-extensions");
  if (gnomeExtensions?.notFound !== true) {
    const result = await runCommand("gnome-extensions", ["install", "--force", archive], 2e4);
    if (!result.ok) {
      return {
        attempted: true,
        method: "gnome-extensions",
        installed: false,
        error: commandFailure(result, "GNOME extension installation failed")
      };
    }
    return { attempted: true, method: "gnome-extensions", installed: true };
  }
  const unzip = prerequisites.commandResults.get("unzip");
  if (unzip?.notFound === true) {
    return {
      attempted: false,
      method: null,
      installed: false,
      error: "gnome-extensions and unzip are not installed; install one before setup"
    };
  }
  try {
    await installFromZip(archive, destination);
    return { attempted: true, method: "unzip", installed: true };
  } catch (error) {
    return {
      attempted: true,
      method: "unzip",
      installed: false,
      error: error instanceof Error ? error.message : String(error)
    };
  }
}
async function queryExtensionList(enabled) {
  const result = await runCommand("gnome-extensions", enabled ? ["list", "--enabled"] : ["list"]);
  if (result.notFound) return { available: false, contains: null, error: "gnome-extensions is not installed" };
  if (!result.ok) {
    return {
      available: false,
      contains: null,
      error: commandFailure(result, `Unable to query ${enabled ? "enabled " : ""}GNOME extensions`)
    };
  }
  const contains = result.stdout.split(/\r?\n/).some((line) => line.trim() === EXTENSION_UUID);
  return { available: true, contains };
}
async function compileExtensionSchemas(destination) {
  const schemasPath = join(destination, "schemas");
  if (!await isDirectory(schemasPath)) {
    return { attempted: false, compiled: null };
  }
  const result = await runCommand("glib-compile-schemas", [schemasPath], 1e4);
  if (result.notFound) {
    return {
      attempted: false,
      compiled: null,
      error: "glib-compile-schemas is not installed; install libglib2.0-bin"
    };
  }
  if (!result.ok) {
    return {
      attempted: true,
      compiled: false,
      error: commandFailure(result, "GNOME extension schema compilation failed")
    };
  }
  return { attempted: true, compiled: true };
}
async function enableExtension(recognizedByShell, prerequisites) {
  const enabledState = await queryExtensionList(true);
  if (recognizedByShell !== true) {
    return {
      attempted: false,
      requested: false,
      enabled: enabledState.contains,
      ...enabledState.error ? { error: enabledState.error } : {}
    };
  }
  if (prerequisites.commandResults.get("gnome-extensions")?.notFound === true) {
    return {
      attempted: false,
      requested: false,
      enabled: enabledState.contains,
      error: "gnome-extensions is unavailable; enable desktop-computer-use@local manually"
    };
  }
  const result = await runCommand("gnome-extensions", ["enable", EXTENSION_UUID], 1e4);
  if (!result.ok) {
    return {
      attempted: true,
      requested: true,
      enabled: enabledState.contains,
      error: commandFailure(result, "GNOME extension enable failed")
    };
  }
  const finalState = await queryExtensionList(true);
  return {
    attempted: true,
    requested: true,
    enabled: finalState.contains,
    ...finalState.error ? { error: finalState.error } : {}
  };
}
function setupRuntimeResult(prerequisites, runtimeInstall, nativePath, sudoAvailable) {
  const installResult = {
    attempted: runtimeInstall.attempted,
    installed: runtimeInstall.installed,
    command: runtimeInstall.command,
    ...runtimeInstall.error ? { error: runtimeInstall.error } : {}
  };
  return {
    missingCommands: prerequisites.missingCommands,
    missingLibraries: prerequisites.missingLibraries,
    unmappedLibraries: prerequisites.unmappedLibraries,
    linkedRuntimeReady: prerequisites.linkedRuntimeReady,
    readinessScope: "Linked libraries and required commands; run doctor for libei, portals, and desktop permissions",
    aptPackages: prerequisites.aptPackages,
    aptInstallCommand: aptInstallCommand(prerequisites.aptPackages, sudoAvailable),
    sudoAvailable,
    nativeChecked: Boolean(nativePath),
    install: installResult
  };
}
async function runLocalSetup() {
  const paths = resolveRuntimePaths();
  await ensureRuntime(paths);
  const token = await ensureToken(paths);
  const skillRoot = resolveSkillRoot();
  let nativePath;
  let nativeError;
  try {
    nativePath = await resolveNativePath(skillRoot);
  } catch (error) {
    nativeError = error instanceof Error ? error.message : String(error);
  }
  const result = {
    platform: process.platform,
    runtimeDirectory: paths.directory,
    endpoint: endpointForToken(paths, token),
    tokenFile: paths.tokenFile,
    nativePath: nativePath ?? null,
    nativeAvailable: Boolean(nativePath),
    ...nativeError ? { nativeError } : {},
    instructions: usageInstructions()
  };
  if (process.platform !== "linux") return result;
  let prerequisites = await collectPrerequisites(nativePath);
  const sudoProbe = runningAsRoot() ? void 0 : await runCommand("sudo", ["-n", "true"]);
  const sudoAvailable = runningAsRoot() || sudoProbe?.ok === true;
  const runtimeInstall = await prepareRuntimePackages(prerequisites, nativePath);
  if (runtimeInstall.attempted && runtimeInstall.installed) {
    prerequisites = runtimeInstall.state;
  }
  const selectedVariant = selectGnomeVariant(prerequisites.shellVersion);
  const archives = await findArchives(skillRoot);
  const selectedArchive = selectedVariant ? await findArchive(skillRoot, selectedVariant) : void 0;
  const extensionPaths = userExtensionPaths();
  const install = await installExtension(selectedArchive, selectedVariant, prerequisites, extensionPaths.destination);
  const installedOnDisk = await isDirectory(extensionPaths.destination);
  const listState = await queryExtensionList(false);
  const extensionRecognizedByShell = listState.contains;
  const schema = installedOnDisk ? await compileExtensionSchemas(extensionPaths.destination) : { attempted: false, compiled: null };
  let userExtensionsError;
  if (installedOnDisk && prerequisites.userExtensionsDisabled === true) {
    const settingResult = await runCommand("gsettings", [
      "set",
      "org.gnome.shell",
      "disable-user-extensions",
      "false"
    ]);
    if (settingResult.ok) prerequisites.userExtensionsDisabled = false;
    else userExtensionsError = commandFailure(settingResult, "Cannot enable user extensions");
  }
  const enable = await enableExtension(extensionRecognizedByShell, prerequisites);
  const extensionEnabled = enable.enabled;
  const activationRequired = prerequisites.userExtensionsDisabled === true || extensionRecognizedByShell !== true || extensionEnabled !== true;
  const runtime = setupRuntimeResult(prerequisites, runtimeInstall, nativePath, sudoAvailable);
  result.gnome = {
    shellVersion: prerequisites.shellVersion ?? null,
    shellMajor: prerequisites.shellMajor ?? null,
    selectedVariant: selectedVariant ?? null,
    selectedArchive: selectedArchive ?? null,
    archives,
    userExtensionsDisabled: prerequisites.userExtensionsDisabled,
    ...userExtensionsError ? { userExtensionsError } : {},
    extensionRecognizedByShell,
    extensionInstalledOnDisk: installedOnDisk,
    extensionEnabled,
    install,
    schema,
    enable,
    activationRequired,
    // Shell can keep the previous module loaded after an on-disk upgrade.
    freshLoginOrShellReloadMayBeRequired: activationRequired || install.installed,
    runtime,
    note: "setup installs the user-local GNOME extension, compiles its schemas when possible, and requests enablement. It never logs out or restarts GNOME Shell; session.start remains blocked until doctor and the indicator report ready."
  };
  return result;
}

// src/cli.ts
var OPTION_DEFS = {
  "app": { key: "app", type: "string" },
  "window-id": { key: "windowId", type: "string" },
  "observation-id": { key: "observationId", type: "string" },
  "element-index": { key: "elementIndex", type: "number" },
  "from-element-index": { key: "fromElementIndex", type: "number" },
  "to-element-index": { key: "toElementIndex", type: "number" },
  "x": { key: "x", type: "number" },
  "y": { key: "y", type: "number" },
  "from-x": { key: "fromX", type: "number" },
  "from-y": { key: "fromY", type: "number" },
  "to-x": { key: "toX", type: "number" },
  "to-y": { key: "toY", type: "number" },
  "coords": { key: "coords", type: "string" },
  "button": { key: "button", type: "string" },
  "mouse-button": { key: "button", type: "string" },
  "duration-ms": { key: "durationMs", type: "number" },
  "steps": { key: "steps", type: "number" },
  "hold-before-ms": { key: "holdBeforeMs", type: "number" },
  "hold-after-ms": { key: "holdAfterMs", type: "number" },
  "direction": { key: "direction", type: "string" },
  "amount": { key: "amount", type: "number" },
  "text": { key: "text", type: "string" },
  "key": { key: "key", type: "string" },
  "value": { key: "value", type: "string" },
  "include-text": { key: "includeText", type: "boolean" },
  "include-screenshot": { key: "includeScreenshot", type: "boolean" },
  "screenshot": { key: "includeScreenshot", type: "boolean" },
  "format": { key: "format", type: "string" },
  "quality": { key: "quality", type: "number" },
  "max-edge": { key: "maxEdge", type: "number" },
  "observe": { key: "observe", type: "string" },
  "session-id": { key: "sessionId", type: "string" },
  "modifiers": { key: "modifiers", type: "string" },
  "all": { key: "all", type: "boolean" },
  "restore-window": { key: "restoreWindow", type: "boolean" },
  "activate": { key: "activate", type: "boolean" },
  "text-stdin": { key: "textStdin", type: "boolean" },
  "value-stdin": { key: "valueStdin", type: "boolean" },
  "timeout-ms": { key: "timeoutMs", type: "number" },
  "json": { key: "json", type: "boolean" },
  "pretty": { key: "pretty", type: "boolean" },
  "raw": { key: "raw", type: "boolean" },
  "help": { key: "help", type: "boolean" }
};
function parseNumber(name, value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) throw new DcuError("invalid_argument", `--${name} must be a finite number`);
  return parsed;
}
function parseBoolean(name, value) {
  if (value === void 0) return true;
  if (value === "true" || value === "1") return true;
  if (value === "false" || value === "0") return false;
  throw new DcuError("invalid_argument", `--${name} expects true or false`);
}
function optionValue(name, raw) {
  const definition = OPTION_DEFS[name];
  if (!definition) throw new DcuError("invalid_argument", `Unknown option --${name}`);
  if (definition.type === "boolean") return parseBoolean(name, raw);
  if (raw === void 0 || raw === "") throw new DcuError("invalid_argument", `--${name} requires a value`);
  return definition.type === "number" ? parseNumber(name, raw) : raw;
}
function looksLikeOption(value) {
  return value.startsWith("--") && !/^-\d+(?:\.\d+)?$/.test(value);
}
function parseArgs(argv) {
  const command = [];
  const positional = [];
  const options = {};
  let optionsEnded = false;
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (optionsEnded) {
      positional.push(token);
      continue;
    }
    if (token === "--") {
      optionsEnded = true;
      continue;
    }
    if (!token.startsWith("--")) {
      const allowsSubcommand = command.length === 1 && ["session", "daemon", "mcp", "toggle"].includes(command[0]);
      if (command.length === 0 || allowsSubcommand) {
        command.push(token);
      } else {
        positional.push(token);
      }
      continue;
    }
    const equals = token.indexOf("=");
    const rawName = equals >= 0 ? token.slice(2, equals) : token.slice(2);
    const negated = rawName.startsWith("no-");
    const name = negated ? rawName.slice(3) : rawName;
    const definition = OPTION_DEFS[name];
    if (!definition) throw new DcuError("invalid_argument", `Unknown option ${token}`);
    let rawValue = equals >= 0 ? token.slice(equals + 1) : void 0;
    if (rawValue === void 0 && definition.type !== "boolean" && i + 1 < argv.length && !looksLikeOption(argv[i + 1])) {
      rawValue = argv[++i];
    }
    if (negated && definition.type !== "boolean") {
      throw new DcuError("invalid_argument", `--no-${name} is only valid for boolean options`);
    }
    const value = negated ? false : optionValue(name, rawValue);
    options[definition.key] = value;
  }
  return { command, positional, options };
}
function asRecord(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}
function setIfDefined(params, key, value) {
  if (value !== void 0) params[key] = value;
}
function validateChoice(params, key, choices) {
  const value = params[key];
  if (value !== void 0 && !choices.includes(String(value))) {
    throw new DcuError("invalid_argument", `${key} must be ${choices.join(", ")}`);
  }
}
function validateNonNegativeNumber(params, key) {
  const value = params[key];
  if (value !== void 0 && (!Number.isFinite(Number(value)) || Number(value) < 0)) {
    throw new DcuError("invalid_argument", `${key} must be a non-negative number`);
  }
}
function validateNonNegativeInteger(params, key) {
  const value = params[key];
  if (value !== void 0 && (!Number.isInteger(value) || Number(value) < 0)) {
    throw new DcuError("invalid_argument", `${key} must be a non-negative integer`);
  }
}
function validateMaximum(params, key, maximum, message) {
  const value = params[key];
  if (value !== void 0 && Number(value) > maximum) {
    throw new DcuError("invalid_argument", message);
  }
}
function validateCommon(params, method) {
  validateChoice(params, "button", ["left", "right", "middle"]);
  validateChoice(params, "direction", ["up", "down", "left", "right"]);
  validateChoice(params, "observe", ["none", "screenshot", "text", "both"]);
  validateChoice(params, "format", ["jpeg", "png"]);
  validateChoice(params, "coords", ["reduced", "full"]);
  if (params.modifiers !== void 0 && !isValidModifiers(String(params.modifiers))) {
    throw new DcuError("invalid_argument", "modifiers must join shift, ctrl, alt, or win with +");
  }
  const hasValidQuality = params.quality === void 0 || Number.isInteger(params.quality) && Number(params.quality) >= 1 && Number(params.quality) <= 100;
  if (!hasValidQuality) {
    throw new DcuError("invalid_argument", "quality must be an integer between 1 and 100");
  }
  for (const key of ["durationMs", "steps", "holdBeforeMs", "holdAfterMs", "maxEdge", "amount"]) {
    validateNonNegativeNumber(params, key);
  }
  for (const key of ["x", "y", "fromX", "fromY", "toX", "toY"]) {
    validateNonNegativeNumber(params, key);
  }
  for (const key of ["elementIndex", "fromElementIndex", "toElementIndex"]) {
    validateNonNegativeInteger(params, key);
  }
  if (params.amount !== void 0 && (Number(params.amount) < 1 || Number(params.amount) > 1e3)) {
    throw new DcuError("invalid_argument", "amount must be between 1 and 1000");
  }
  validateMaximum(params, "maxEdge", 16384, "maxEdge must be at most 16384");
  for (const key of ["durationMs", "holdBeforeMs", "holdAfterMs"]) {
    validateMaximum(params, key, 1e4, `${key} must be at most 10000`);
  }
  validateMaximum(params, "steps", 1e3, "steps must be at most 1000");
  const hasElementIndex = params.elementIndex !== void 0 || params.fromElementIndex !== void 0 || params.toElementIndex !== void 0;
  if (hasElementIndex && params.observationId === void 0) {
    throw new DcuError("invalid_argument", "element indexes require --observation-id");
  }
  if (method === "drag" && params.steps !== void 0 && (!Number.isInteger(params.steps) || Number(params.steps) < 1)) {
    throw new DcuError("invalid_argument", "steps must be a positive integer");
  }
}
function paramsFromOptions(options, method) {
  const params = {};
  const excluded = /* @__PURE__ */ new Set([
    "json",
    "pretty",
    "raw",
    "help",
    "timeoutMs",
    "textStdin",
    "valueStdin",
    "all"
  ]);
  for (const [key, value] of Object.entries(options)) {
    if (!excluded.has(key)) setIfDefined(params, key, value);
  }
  if (method === "get-app-state") {
    if (options.includeScreenshot === void 0) {
      params.includeScreenshot = true;
    }
    if (options.includeText === void 0) {
      params.includeText = false;
    }
  } else if (typeof options.observe === "string") {
    if (options.includeScreenshot === void 0) {
      params.includeScreenshot = options.observe === "screenshot" || options.observe === "both";
    }
    if (options.includeText === void 0) {
      params.includeText = options.observe === "text" || options.observe === "both";
    }
  }
  return params;
}
async function readStdinText() {
  const chunks = [];
  for await (const chunk of stdin) chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk)));
  return Buffer.concat(chunks).toString("utf8");
}
function toggleMethod(action, options) {
  if (action === "status") return "toggle.status";
  if (action !== "on" && action !== "off") {
    throw new DcuError("invalid_argument", "Use `toggle on --key K`, `toggle off --key K`, `toggle off --all`, or `toggle status`");
  }
  if (options.all === true) {
    if (action === "on" || options.key !== void 0) {
      throw new DcuError("invalid_argument", "--all is only valid as `toggle off --all` without --key");
    }
    return "toggle.release-all";
  }
  if (typeof options.key !== "string") {
    throw new DcuError("invalid_argument", `toggle ${action} requires --key shift|ctrl|alt|win|space`);
  }
  if (!TOGGLE_KEY_ALIASES.includes(options.key.toLowerCase())) {
    throw new DcuError("invalid_argument", "--key must be shift, ctrl, alt, win, or space");
  }
  return "toggle.set";
}
function methodFor(command, options = {}) {
  if (command[0] === "toggle") return toggleMethod(command[1], options);
  if (command.length === 1) return command[0];
  if (command[0] === "session" && ["start", "status", "stop"].includes(command[1])) return `session.${command[1]}`;
  if (command[0] === "daemon" && command[1] === "shutdown") return "daemon.shutdown";
  throw new DcuError("invalid_argument", `Unknown command ${command.join(" ")}`);
}
function helpText() {
  return [
    "desktop-computer-use \u2014 standalone native desktop control",
    "",
    "Usage: dcu <command> [options]",
    "",
    "Commands: setup, doctor, capabilities, session start|status|stop, daemon shutdown,",
    "  list-apps, list-windows, get-app-state, get-full-screenshot,",
    "  click, drag, scroll, type-text, press-key, hotkey, set-value, paste-text,",
    "  toggle on|off --key shift|ctrl|alt|win|space, toggle off --all, toggle status, mcp serve",
    "",
    "Actions use a saved session from `session start`; pass --session-id to override it.",
    "Observations are returned as JSON with a reduced screenshot path (0.5x above 1280x720).",
    "x/y coordinates are pixels of the window's latest reduced screenshot (or --observation-id).",
    "`get-full-screenshot --observation-id ID` returns the original image; click from it with --coords full.",
    "Drag defaults: 240ms, 12 steps, 50ms hold before and after. Use --duration-ms/--steps to tune them.",
    "click and drag accept --modifiers shift+ctrl to hold keys for that one action.",
    "`toggle on --key K` holds a key across actions; every result then lists `toggles` with a notice.",
    "`session stop` fails with toggles_active while a toggle is on; run `toggle off --all` first.",
    "Active sessions show a top banner, high-contrast cursor ring, and blue inward-fading screen-edge border.",
    "Output is compact JSON (element/window records as one line each); --raw prints the native response.",
    "Emergency stop: press Esc twice within 1 second or run `dcu session stop`."
  ].join("\n");
}
function sessionFileAction(method, response) {
  if (!response.ok) return void 0;
  if (method === "session.start") return "write";
  if (method === "session.stop") return "clear";
  return void 0;
}
function sessionRequired(method) {
  return !SESSIONLESS_METHODS.has(method);
}
function appRequired(method) {
  return WINDOW_TARGET_METHODS.has(method);
}
function validateMethodParams(method, params) {
  validateCommon(params, method);
  if (sessionRequired(method) && typeof params.sessionId !== "string") {
    throw new DcuError("session_required", "Start a computer-use session first or pass --session-id");
  }
  if (appRequired(method) && typeof params.app !== "string" && typeof params.windowId !== "string") {
    throw new DcuError("invalid_argument", `${method} requires --app or --window-id`);
  }
  if (method === "get-full-screenshot" && typeof params.observationId !== "string") {
    throw new DcuError("invalid_argument", "get-full-screenshot requires --observation-id");
  }
  if (method === "drag") {
    const byElements = params.fromElementIndex !== void 0 || params.toElementIndex !== void 0;
    const byCoordinates = ["fromX", "fromY", "toX", "toY"].some((key) => params[key] !== void 0);
    if (byElements && (params.fromElementIndex === void 0 || params.toElementIndex === void 0)) {
      throw new DcuError("invalid_argument", "drag requires both from/to element indexes");
    }
    if (byCoordinates && ["fromX", "fromY", "toX", "toY"].some((key) => params[key] === void 0)) {
      throw new DcuError("invalid_argument", "drag requires from-x, from-y, to-x, and to-y");
    }
    if (!byElements && !byCoordinates) {
      throw new DcuError("invalid_argument", "drag requires element indexes or coordinates");
    }
    params.durationMs ??= 240;
    params.steps ??= 12;
    params.holdBeforeMs ??= 50;
    params.holdAfterMs ??= 50;
  }
  if (method === "scroll") {
    if (params.direction === void 0) {
      throw new DcuError("invalid_argument", "scroll requires --direction");
    }
    if (params.x === void 0 || params.y === void 0) {
      throw new DcuError("invalid_argument", "scroll requires --x and --y");
    }
    params.amount ??= 3;
  }
  if ((method === "type-text" || method === "paste-text") && params.text === void 0) {
    throw new DcuError("invalid_argument", `${method} requires --text or --text-stdin`);
  }
  if ((method === "press-key" || method === "hotkey") && params.key === void 0) {
    throw new DcuError("invalid_argument", `${method} requires --key`);
  }
  if (method === "set-value") {
    if (params.elementIndex === void 0) throw new DcuError("invalid_argument", "set-value requires --element-index");
    if (params.value === void 0) {
      throw new DcuError("invalid_argument", "set-value requires --value or --value-stdin");
    }
  }
}
function output(value, options) {
  const shaped = options.raw ? value : compactResponse(value);
  stdout.write(`${JSON.stringify(shaped, null, options.pretty ? 2 : 0)}
`);
}
async function runCli(argv = process.argv.slice(2)) {
  try {
    const parsed = parseArgs(argv);
    if (parsed.options.help || parsed.command.length === 0) {
      stdout.write(`${helpText()}
`);
      return 0;
    }
    if (parsed.command[0] === "mcp" && parsed.command[1] === "serve") {
      const { runMcpServer } = await import("./dcu-mcp-EGWL7J33.mjs");
      await runMcpServer(new DcuClient());
      return 0;
    }
    if (parsed.command[0] === "setup") {
      output({ ok: true, result: await runLocalSetup() }, parsed.options);
      return 0;
    }
    const method = methodFor(parsed.command, parsed.options);
    const params = paramsFromOptions(parsed.options, method);
    if (method === "toggle.set") params.on = parsed.command[1] === "on";
    const acceptsPositionalApp = method === "get-app-state" || method === "list-windows";
    if (acceptsPositionalApp && parsed.positional.length > 0 && params.app === void 0) {
      params.app = parsed.positional[0];
    }
    if (parsed.options.textStdin) {
      params.text = await readStdinText();
    }
    if (parsed.options.valueStdin) {
      params.value = await readStdinText();
    }
    const paths = resolveRuntimePaths();
    const saved = await readSession(paths);
    const canUseSavedSession = sessionRequired(method) || method === "session.status";
    if (canUseSavedSession && params.sessionId === void 0 && saved) {
      params.sessionId = saved.sessionId;
    }
    validateMethodParams(method, params);
    const client = new DcuClient(paths);
    const timeoutMs = typeof parsed.options.timeoutMs === "number" ? parsed.options.timeoutMs : void 0;
    let response;
    try {
      response = await client.request(method, params, { timeoutMs });
    } catch (error) {
      output(errorToResponse(error), parsed.options);
      return 1;
    }
    const sessionFile = sessionFileAction(method, response);
    if (sessionFile === "write") {
      const result = asRecord(response.result);
      if (typeof result.sessionId === "string") await writeSession(result.sessionId, paths);
    } else if (sessionFile === "clear") {
      await clearSession(paths);
    }
    output(response, parsed.options);
    return response.ok ? 0 : 1;
  } catch (error) {
    output(errorToResponse(error), {});
    return 1;
  }
}
function isMainModule() {
  if (!process.argv[1]) {
    return false;
  }
  const entryPath = resolve2(process.argv[1]);
  const entryUrl = pathToFileURL(entryPath).href;
  if (import.meta.url === entryUrl) {
    return true;
  }
  try {
    const realModulePath = realpathSync(fileURLToPath(import.meta.url));
    const realEntryPath = realpathSync(entryPath);
    return realModulePath === realEntryPath;
  } catch {
    return false;
  }
}
if (isMainModule()) {
  runCli().then((code) => {
    process.exitCode = code;
  });
}
export {
  methodFor,
  parseArgs,
  runCli,
  sessionFileAction
};

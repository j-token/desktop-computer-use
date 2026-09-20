import { execFile } from "node:child_process";
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { DcuError } from "./errors.js";
import {
  ensureRuntime,
  ensureToken,
  resolveNativePath,
  resolveRuntimePaths,
  resolveSkillRoot,
  usageInstructions
} from "./runtime.js";
import type { JsonObject } from "./types.js";

const EXTENSION_UUID = "desktop-computer-use@local";
const COMMAND_TIMEOUT_MS = 8_000;
const APT_TIMEOUT_MS = 60_000;

type GnomeVariant = "legacy" | "modern";

interface CommandResult {
  command: string;
  args: string[];
  ok: boolean;
  notFound: boolean;
  timedOut: boolean;
  stdout: string;
  stderr: string;
  exitCode: number | null;
}

interface PrerequisiteState {
  shellVersion: string | undefined;
  shellMajor: number | undefined;
  userExtensionsDisabled: boolean | null;
  commandResults: Map<string, CommandResult>;
  missingCommands: string[];
  missingLibraries: string[];
  unmappedLibraries: string[];
  aptPackages: string[];
  linkedRuntimeReady: boolean | null;
}

interface ExtensionListState {
  available: boolean;
  contains: boolean | null;
  error?: string;
}

interface InstallState {
  attempted: boolean;
  method: "gnome-extensions" | "unzip" | null;
  installed: boolean;
  error?: string;
}

interface SchemaState {
  attempted: boolean;
  compiled: boolean | null;
  error?: string;
}

interface EnableState {
  attempted: boolean;
  requested: boolean;
  enabled: boolean | null;
  error?: string;
}

const COMMAND_PACKAGES: Record<string, string> = {
  "gnome-shell": "gnome-shell",
  "gnome-extensions": "gnome-shell",
  gsettings: "libglib2.0-bin",
  "glib-compile-schemas": "libglib2.0-bin",
  unzip: "unzip",
  ldd: "libc-bin",
  "wl-copy": "wl-clipboard",
  xclip: "xclip"
};

const LIBRARY_PACKAGES: Array<{ pattern: RegExp; packageName: string }> = [
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

export function parseGnomeShellMajor(shellVersion: string | undefined): number | undefined {
  if (!shellVersion) return undefined;
  const match = shellVersion.match(/(?:GNOME\s+Shell\s+)?(\d+)(?:\.\d+)?/i);
  if (!match) return undefined;
  const major = Number(match[1]);
  return Number.isSafeInteger(major) ? major : undefined;
}

export function selectGnomeVariant(shellVersion: string | undefined): GnomeVariant | undefined {
  const major = parseGnomeShellMajor(shellVersion);
  if (major === undefined) return undefined;
  if (major > 50) return undefined;
  if (major >= 45) return "modern";
  if (major >= 42) return "legacy";
  return undefined;
}

function commandOutput(result: CommandResult): string {
  return [result.stderr.trim(), result.stdout.trim()]
    .filter(Boolean)
    .join(" ")
    .slice(0, 2_000);
}

function commandFailure(result: CommandResult, fallback: string): string {
  if (result.notFound) return `${result.command} is not installed`;
  if (result.timedOut) return `${result.command} timed out`;
  return commandOutput(result) || `${fallback} (exit code ${result.exitCode ?? "unknown"})`;
}

function runCommand(command: string, args: string[], timeoutMs = COMMAND_TIMEOUT_MS): Promise<CommandResult> {
  return new Promise(resolveResult => {
    execFile(
      command,
      args,
      { encoding: "utf8", maxBuffer: 1_000_000, timeout: timeoutMs, windowsHide: true },
      (error, stdoutText, stderrText) => {
        const processError = error as (NodeJS.ErrnoException & { killed?: boolean }) | null;
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

function uniqueStrings(values: string[]): string[] {
  return [...new Set(values)];
}

function packageForLibrary(libraryName: string): string | undefined {
  return LIBRARY_PACKAGES.find(entry => entry.pattern.test(libraryName))?.packageName;
}

function parseMissingLibraries(output: string): string[] {
  const missing: string[] = [];
  for (const line of output.split(/\r?\n/)) {
    const match = line.match(/^\s*(\S+)\s*=>\s*not found\s*$/i);
    if (match) missing.push(match[1]);
  }
  return uniqueStrings(missing);
}

function commandProbeDefinitions(): Array<{ name: string; args: string[] }> {
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

async function collectPrerequisites(nativePath: string | undefined): Promise<PrerequisiteState> {
  const commandResults = new Map<string, CommandResult>();
  const probes = await Promise.all(
    commandProbeDefinitions().map(async definition => {
      const result = await runCommand(definition.name, definition.args);
      commandResults.set(definition.name, result);
      return result;
    })
  );

  const shellProbe = commandResults.get("gnome-shell");
  const shellOutput = shellProbe && shellProbe.ok
    ? `${shellProbe.stdout} ${shellProbe.stderr}`.trim()
    : undefined;
  const shellVersion = shellOutput || undefined;
  const shellMajor = parseGnomeShellMajor(shellVersion);

  const gsettingsProbe = commandResults.get("gsettings");
  const disabledSetting = gsettingsProbe?.ok
    ? await runCommand("gsettings", ["get", "org.gnome.shell", "disable-user-extensions"])
    : undefined;
  let userExtensionsDisabled: boolean | null = null;
  if (disabledSetting?.ok) {
    if (disabledSetting.stdout.trim() === "true") userExtensionsDisabled = true;
    if (disabledSetting.stdout.trim() === "false") userExtensionsDisabled = false;
  }

  const missingCommands = probes
    .filter(result => result.notFound)
    .map(result => result.command);
  const missingLibraries: string[] = [];
  if (nativePath && commandResults.get("ldd")?.notFound !== true) {
    const lddResult = await runCommand("ldd", [nativePath]);
    commandResults.set("ldd:native", lddResult);
    missingLibraries.push(...parseMissingLibraries(`${lddResult.stdout}\n${lddResult.stderr}`));
  }

  const unmappedLibraries = missingLibraries.filter(name => !packageForLibrary(name));
  const aptPackages = uniqueStrings([
    ...missingCommands.map(command => COMMAND_PACKAGES[command]).filter(Boolean),
    ...missingLibraries.map(packageForLibrary).filter((name): name is string => Boolean(name))
  ]);
  const lddResult = commandResults.get("ldd:native");
  const nativeDependenciesReady = nativePath === undefined
    ? null
    : lddResult?.ok === true && missingLibraries.length === 0;
  const linkedRuntimeReady = missingCommands.length === 0 && nativeDependenciesReady !== false
    ? nativeDependenciesReady
    : false;

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

function runningAsRoot(): boolean {
  return typeof process.getuid === "function" && process.getuid() === 0;
}

function aptInstallCommand(packages: string[], privileged: boolean): string | null {
  if (packages.length === 0) return null;
  const packageList = packages.join(" ");
  const prefix = runningAsRoot() ? "" : privileged ? "sudo " : "";
  const command = `${prefix}apt-get update && ${prefix}apt-get install -y ${packageList}`;
  if (!privileged && !runningAsRoot()) {
    return `${command}  # run as root; passwordless sudo is unavailable`;
  }
  return command;
}

async function prepareRuntimePackages(
  state: PrerequisiteState,
  nativePath: string | undefined
): Promise<{
  attempted: boolean;
  installed: boolean;
  command: string | null;
  error?: string;
  state: PrerequisiteState;
}> {
  if (state.aptPackages.length === 0) {
    return { attempted: false, installed: true, command: null, state };
  }

  const sudoProbe = runningAsRoot() ? undefined : await runCommand("sudo", ["-n", "true"]);
  const privileged = runningAsRoot() || sudoProbe?.ok === true;
  const command = aptInstallCommand(state.aptPackages, privileged);
  const aptProbe = await runCommand("apt-get", ["--version"]);
  if (!privileged || aptProbe.notFound) {
    const reason = aptProbe.notFound
      ? "apt-get is not installed"
      : "passwordless sudo is unavailable; run the reported apt command as root";
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
    prefix.length > 0
      ? ["-n", "apt-get", "install", "-y", ...state.aptPackages]
      : ["install", "-y", ...state.aptPackages],
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

function extensionArchiveRoots(skillRoot: string): string[] {
  return uniqueStrings([
    join(skillRoot, "extensions"),
    join(skillRoot, "extensions", "dist"),
    join(skillRoot, "..", "..", "extensions", "dist"),
    join(skillRoot, "..", "..", "extensions"),
    join(process.cwd(), "extensions", "dist"),
    join(process.cwd(), "extensions")
  ].map(path => resolve(path)));
}

async function findArchive(skillRoot: string, variant: GnomeVariant): Promise<string | undefined> {
  const archiveName = `desktop-computer-use-gnome-${variant}.zip`;
  for (const root of extensionArchiveRoots(skillRoot)) {
    const candidate = join(root, archiveName);
    try {
      if ((await stat(candidate)).isFile()) return candidate;
    } catch {
      // Continue through optional source and packaged resource locations.
    }
  }
  return undefined;
}

async function findArchives(skillRoot: string): Promise<string[]> {
  const archives: string[] = [];
  for (const variant of ["modern", "legacy"] as const) {
    const archive = await findArchive(skillRoot, variant);
    if (archive) archives.push(archive);
  }
  return archives;
}

function userExtensionPaths(): { root: string; destination: string } {
  const dataHome = process.env.XDG_DATA_HOME
    ? resolve(process.env.XDG_DATA_HOME)
    : join(homedir(), ".local", "share");
  const root = resolve(dataHome, "gnome-shell", "extensions");
  const destination = resolve(root, EXTENSION_UUID);
  if (destination === root || !destination.startsWith(`${root}${sep}`)) {
    throw new DcuError("invalid_environment", "GNOME extension destination escaped the user extension directory");
  }
  return { root, destination };
}

async function isDirectory(path: string): Promise<boolean> {
  try {
    return (await lstat(path)).isDirectory();
  } catch {
    return false;
  }
}

async function assertNoSymlinks(root: string): Promise<void> {
  const entries = await readdir(root, { withFileTypes: true });
  for (const entry of entries) {
    const entryPath = join(root, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error(`Refusing symbolic link in extension archive: ${entry.name}`);
    }
    if (entry.isDirectory()) await assertNoSymlinks(entryPath);
  }
}

function safeArchiveEntry(entry: string): boolean {
  const normalized = entry.replaceAll("\\", "/");
  if (!normalized || normalized.startsWith("/") || /^[A-Za-z]:\//.test(normalized)) return false;
  return !normalized.split("/").includes("..");
}

async function extractedExtensionRoot(stage: string): Promise<string> {
  const directMetadata = join(stage, "metadata.json");
  if (await isRegularFile(directMetadata)) return stage;

  const nestedDestination = join(stage, EXTENSION_UUID);
  if (await isRegularFile(join(nestedDestination, "metadata.json"))) return nestedDestination;

  const children = await readdir(stage, { withFileTypes: true });
  const directories = children.filter(entry => entry.isDirectory());
  if (directories.length === 1) {
    const candidate = join(stage, directories[0].name);
    if (await isRegularFile(join(candidate, "metadata.json"))) return candidate;
  }
  throw new Error("Extension archive does not contain metadata.json at its root");
}

async function isRegularFile(path: string): Promise<boolean> {
  try {
    return (await lstat(path)).isFile();
  } catch {
    return false;
  }
}

async function validateExtensionMetadata(root: string): Promise<void> {
  const metadataPath = join(root, "metadata.json");
  const metadata = JSON.parse(await readFile(metadataPath, "utf8")) as { uuid?: unknown };
  if (metadata.uuid !== EXTENSION_UUID) {
    throw new Error(`Extension metadata uuid must be ${EXTENSION_UUID}`);
  }
}

async function installFromZip(archive: string, destination: string): Promise<void> {
  const listing = await runCommand("unzip", ["-Z1", archive], 15_000);
  if (!listing.ok) throw new Error(commandFailure(listing, "Unable to inspect the GNOME extension archive"));
  const entries = listing.stdout.split(/\r?\n/).map(entry => entry.trim()).filter(Boolean);
  if (entries.length === 0 || entries.some(entry => !safeArchiveEntry(entry))) {
    throw new Error("Refusing an unsafe or empty GNOME extension archive");
  }

  const stage = await mkdtemp(join(tmpdir(), "dcu-extension-"));
  try {
    const extracted = await runCommand("unzip", ["-q", archive, "-d", stage], 15_000);
    if (!extracted.ok) throw new Error(commandFailure(extracted, "Unable to extract the GNOME extension archive"));

    const extensionRoot = await extractedExtensionRoot(stage);
    await assertNoSymlinks(extensionRoot);
    await validateExtensionMetadata(extensionRoot);

    const existing = await lstat(destination).catch(error => {
      const code = (error as NodeJS.ErrnoException).code;
      if (code === "ENOENT") return undefined;
      throw error;
    });
    if (existing?.isSymbolicLink()) {
      throw new Error("Refusing to replace a symbolic-link GNOME extension destination");
    }
    await mkdir(dirname(destination), { recursive: true, mode: 0o700 });
    await rm(destination, { recursive: true, force: true });
    await cp(extensionRoot, destination, { recursive: true, force: true });
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}

async function installExtension(
  archive: string | undefined,
  variant: GnomeVariant | undefined,
  prerequisites: PrerequisiteState,
  destination: string
): Promise<InstallState> {
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
    const result = await runCommand("gnome-extensions", ["install", "--force", archive], 20_000);
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

async function queryExtensionList(enabled: boolean): Promise<ExtensionListState> {
  const result = await runCommand("gnome-extensions", enabled ? ["list", "--enabled"] : ["list"]);
  if (result.notFound) return { available: false, contains: null, error: "gnome-extensions is not installed" };
  if (!result.ok) {
    return {
      available: false,
      contains: null,
      error: commandFailure(result, `Unable to query ${enabled ? "enabled " : ""}GNOME extensions`)
    };
  }
  const contains = result.stdout.split(/\r?\n/).some(line => line.trim() === EXTENSION_UUID);
  return { available: true, contains };
}

async function compileExtensionSchemas(destination: string): Promise<SchemaState> {
  const schemasPath = join(destination, "schemas");
  if (!(await isDirectory(schemasPath))) {
    return { attempted: false, compiled: null };
  }
  const result = await runCommand("glib-compile-schemas", [schemasPath], 10_000);
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

async function enableExtension(
  recognizedByShell: boolean | null,
  prerequisites: PrerequisiteState
): Promise<EnableState> {
  const enabledState = await queryExtensionList(true);
  if (recognizedByShell !== true) {
    return {
      attempted: false,
      requested: false,
      enabled: enabledState.contains,
      ...(enabledState.error ? { error: enabledState.error } : {})
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

  const result = await runCommand("gnome-extensions", ["enable", EXTENSION_UUID], 10_000);
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
    ...(finalState.error ? { error: finalState.error } : {})
  };
}

function setupRuntimeResult(
  prerequisites: PrerequisiteState,
  runtimeInstall: {
    attempted: boolean;
    installed: boolean;
    command: string | null;
    error?: string;
  },
  nativePath: string | undefined,
  sudoAvailable: boolean
): JsonObject {
  const installResult: JsonObject = {
    attempted: runtimeInstall.attempted,
    installed: runtimeInstall.installed,
    command: runtimeInstall.command,
    ...(runtimeInstall.error ? { error: runtimeInstall.error } : {})
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

export async function runLocalSetup(): Promise<JsonObject> {
  const paths = resolveRuntimePaths();
  await ensureRuntime(paths);
  await ensureToken(paths);

  const skillRoot = resolveSkillRoot();
  let nativePath: string | undefined;
  let nativeError: string | undefined;
  try {
    nativePath = await resolveNativePath(skillRoot);
  } catch (error) {
    nativeError = error instanceof Error ? error.message : String(error);
  }

  const result: JsonObject = {
    platform: process.platform,
    runtimeDirectory: paths.directory,
    endpoint: paths.endpoint,
    tokenFile: paths.tokenFile,
    nativePath: nativePath ?? null,
    nativeAvailable: Boolean(nativePath),
    ...(nativeError ? { nativeError } : {}),
    instructions: usageInstructions()
  };

  if (process.platform !== "linux") return result;

  let prerequisites = await collectPrerequisites(nativePath);
  const sudoProbe = runningAsRoot() ? undefined : await runCommand("sudo", ["-n", "true"]);
  const sudoAvailable = runningAsRoot() || sudoProbe?.ok === true;
  const runtimeInstall = await prepareRuntimePackages(prerequisites, nativePath);
  if (runtimeInstall.attempted && runtimeInstall.installed) {
    prerequisites = runtimeInstall.state;
  }

  const selectedVariant = selectGnomeVariant(prerequisites.shellVersion);
  const archives = await findArchives(skillRoot);
  const selectedArchive = selectedVariant ? await findArchive(skillRoot, selectedVariant) : undefined;
  const extensionPaths = userExtensionPaths();
  const install = await installExtension(selectedArchive, selectedVariant, prerequisites, extensionPaths.destination);
  const installedOnDisk = await isDirectory(extensionPaths.destination);
  const listState = await queryExtensionList(false);
  const extensionRecognizedByShell = listState.contains;
  // A newly installed extension may not be known to the running Shell until
  // the next login. Its on-disk schemas still need compilation immediately.
  const schema = installedOnDisk
    ? await compileExtensionSchemas(extensionPaths.destination)
    : { attempted: false, compiled: null } satisfies SchemaState;
  let userExtensionsError: string | undefined;
  if (installedOnDisk && prerequisites.userExtensionsDisabled === true) {
    const settingResult = await runCommand("gsettings", [
      "set", "org.gnome.shell", "disable-user-extensions", "false"
    ]);
    if (settingResult.ok) prerequisites.userExtensionsDisabled = false;
    else userExtensionsError = commandFailure(settingResult, "Cannot enable user extensions");
  }
  const enable = await enableExtension(extensionRecognizedByShell, prerequisites);
  const extensionEnabled = enable.enabled;
  const activationRequired =
    prerequisites.userExtensionsDisabled === true || extensionRecognizedByShell !== true || extensionEnabled !== true;
  const runtime = setupRuntimeResult(prerequisites, runtimeInstall, nativePath, sudoAvailable);

  result.gnome = {
    shellVersion: prerequisites.shellVersion ?? null,
    shellMajor: prerequisites.shellMajor ?? null,
    selectedVariant: selectedVariant ?? null,
    selectedArchive: selectedArchive ?? null,
    archives,
    userExtensionsDisabled: prerequisites.userExtensionsDisabled,
    ...(userExtensionsError ? { userExtensionsError } : {}),
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
    note:
      "setup installs the user-local GNOME extension, compiles its schemas when possible, and requests enablement. " +
      "It never logs out or restarts GNOME Shell; session.start remains blocked until doctor and the " +
      "indicator report ready."
  } as unknown as JsonObject;
  return result;
}

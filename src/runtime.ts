import { chmod, lstat, mkdir, open, readFile, stat, unlink, writeFile, type FileHandle } from "node:fs/promises";
import { createHash, randomBytes } from "node:crypto";
import { tmpdir, userInfo } from "node:os";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import { createConnection } from "node:net";
import type { RuntimePaths, SessionState } from "./types.js";
import { DcuError, asError } from "./errors.js";

const TOKEN_MIN_LENGTH = 32;
let startPromise: Promise<void> | undefined;

function isWindows(platform = process.platform): boolean {
  return platform === "win32";
}

function safeUserName(): string {
  const value = process.env.USERNAME ?? process.env.USER ?? userInfo().username ?? "user";
  return value.replace(/[^a-zA-Z0-9_.-]/g, "_").slice(0, 48) || "user";
}

export function resolveRuntimePaths(env: NodeJS.ProcessEnv = process.env, platform = process.platform): RuntimePaths {
  const explicitDirectory = env.DCU_RUNTIME_DIR;
  const defaultDirectory = isWindows(platform)
    ? join(env.LOCALAPPDATA || env.TEMP || tmpdir(), "desktop-computer-use")
    : join(env.XDG_RUNTIME_DIR || join(tmpdir(), `desktop-computer-use-${safeUserName()}`), "desktop-computer-use");
  const directory = resolve(explicitDirectory || defaultDirectory);
  const explicitEndpoint = env.DCU_ENDPOINT;
  const endpointSuffix = createHash("sha256").update(directory, "utf8").digest("hex").slice(0, 12);
  const endpoint = explicitEndpoint || (isWindows(platform)
    ? `\\\\.\\pipe\\desktop-computer-use-${safeUserName()}-${endpointSuffix}`
    : join(directory, "daemon.sock"));
  return {
    directory,
    endpoint,
    tokenFile: join(directory, "token"),
    sessionFile: join(directory, "session.json")
  };
}

export function resolveSkillRoot(): string {
  if (process.env.DCU_SKILL_ROOT) {
    return resolve(process.env.DCU_SKILL_ROOT);
  }
  if (process.argv[1]) {
    return resolve(dirname(process.argv[1]), "..");
  }
  return process.cwd();
}

async function ensurePrivateDirectory(directory: string): Promise<void> {
  if (process.platform === "win32") {
    const privateRoots = [process.env.LOCALAPPDATA, process.env.TEMP]
      .filter((value): value is string => Boolean(value))
      .map(value => resolve(value));
    const normalizedDirectory = resolve(directory).toLowerCase();
    const isUserPrivatePath = privateRoots.some(root => {
      const normalizedRoot = root.toLowerCase();
      return normalizedDirectory === normalizedRoot || normalizedDirectory.startsWith(`${normalizedRoot}${sep}`);
    });
    if (!isUserPrivatePath) {
      throw new DcuError(
        "invalid_environment",
        `Windows runtime directory must be inside LOCALAPPDATA or TEMP: ${directory}`
      );
    }
  }
  await mkdir(directory, { recursive: true, mode: 0o700 });
  if (process.platform !== "win32") {
    await chmod(directory, 0o700);
  }
}

function wait(delayMs: number): Promise<void> {
  return new Promise(resolvePromise => setTimeout(resolvePromise, delayMs));
}

async function endpointReachable(endpoint: string): Promise<boolean> {
  return new Promise(resolvePromise => {
    const socket = createConnection(endpoint);
    let done = false;
    const finish = (reachable: boolean): void => {
      if (done) {
        return;
      }
      done = true;
      socket.destroy();
      resolvePromise(reachable);
    };
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(150, () => finish(false));
  });
}

interface DaemonStartupLock {
  handle?: FileHandle;
  existing: boolean;
}

async function acquireDaemonLock(paths: RuntimePaths): Promise<DaemonStartupLock> {
  const lockPath = join(paths.directory, "daemon.lock");
  for (let attempt = 0; attempt < 240; attempt += 1) {
    if (await endpointReachable(paths.endpoint)) {
      return { existing: true };
    }
    try {
      const handle = await open(lockPath, "wx", 0o600);
      return { handle, existing: false };
    } catch (error) {
      const lockError = error as NodeJS.ErrnoException;
      if (lockError.code !== "EEXIST") {
        throw new DcuError("runtime_error", `Unable to acquire daemon startup lock: ${asError(error).message}`);
      }
      try {
        const lockInfo = await stat(lockPath);
        if (Date.now() - lockInfo.mtimeMs > 15_000) {
          await unlink(lockPath);
        }
      } catch (statError) {
        const statErr = statError as NodeJS.ErrnoException;
        if (statErr.code !== "ENOENT") {
          throw new DcuError("runtime_error", `Unable to inspect daemon startup lock: ${asError(statError).message}`);
        }
      }
      await wait(50);
    }
  }
  throw new DcuError("transport_timeout", "Timed out waiting for another desktop computer-use daemon to start");
}

async function releaseDaemonLock(paths: RuntimePaths, handle: FileHandle): Promise<void> {
  try {
    await handle.close();
  } finally {
    try {
      await unlink(join(paths.directory, "daemon.lock"));
    } catch (error) {
      const err = error as NodeJS.ErrnoException;
      if (err.code !== "ENOENT") {
        throw error;
      }
    }
  }
}

export async function ensureRuntime(paths = resolveRuntimePaths()): Promise<RuntimePaths> {
  await ensurePrivateDirectory(paths.directory);
  return paths;
}

function validToken(value: string): boolean {
  return value.length >= TOKEN_MIN_LENGTH && value.length <= 256 && /^[A-Za-z0-9_=-]+$/.test(value);
}

export async function ensureToken(paths = resolveRuntimePaths()): Promise<string> {
  await ensureRuntime(paths);
  try {
    const current = (await readFile(paths.tokenFile, "utf8")).trim();
    if (!validToken(current)) {
      throw new DcuError("invalid_environment", `Token file is invalid: ${paths.tokenFile}`);
    }
    return current;
  } catch (error) {
    if (error instanceof DcuError) throw error;
    const err = error as NodeJS.ErrnoException;
    if (err.code !== "ENOENT") {
      throw new DcuError(
        "runtime_error",
        `Unable to read token file: ${asError(error).message}`
      );
    }
    const token = randomBytes(48).toString("base64url");
    try {
      // Exclusive creation prevents two CLI processes from replacing the token
      // after a daemon has already read the winner's credentials.
      await writeFile(paths.tokenFile, `${token}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
      if (process.platform !== "win32") await chmod(paths.tokenFile, 0o600);
      return token;
    } catch (createError) {
      const createErr = createError as NodeJS.ErrnoException;
      if (createErr.code !== "EEXIST") {
        throw new DcuError("runtime_error", `Unable to create token file: ${asError(createError).message}`);
      }
      // Another process may have created the file but not finished its write yet.
      // Read it a few times before treating it as an invalid environment.
      for (let attempt = 0; attempt < 8; attempt += 1) {
        try {
          const winner = (await readFile(paths.tokenFile, "utf8")).trim();
          if (validToken(winner)) return winner;
        } catch {
          // The creator may not have closed the file yet.
        }
        await new Promise(resolvePromise => setTimeout(resolvePromise, 5));
      }
      throw new DcuError(
        "invalid_environment",
        `Token file was concurrently created but is invalid: ${paths.tokenFile}`
      );
    }
  }
}

export async function readSession(paths = resolveRuntimePaths()): Promise<SessionState | undefined> {
  try {
    const raw = await readFile(paths.sessionFile, "utf8");
    const parsed = JSON.parse(raw) as Partial<SessionState>;
    if (typeof parsed.sessionId !== "string" || !parsed.sessionId) return undefined;
    return { sessionId: parsed.sessionId, startedAt: typeof parsed.startedAt === "string" ? parsed.startedAt : "" };
  } catch (error) {
    const err = error as NodeJS.ErrnoException;
    if (err.code === "ENOENT") return undefined;
    throw new DcuError("runtime_error", `Unable to read session state: ${asError(error).message}`);
  }
}

export async function writeSession(sessionId: string, paths = resolveRuntimePaths()): Promise<void> {
  await ensureRuntime(paths);
  const payload = JSON.stringify({ sessionId, startedAt: new Date().toISOString() }) + "\n";
  await writeFile(paths.sessionFile, payload, { encoding: "utf8", mode: 0o600 });
  if (process.platform !== "win32") await chmod(paths.sessionFile, 0o600);
}

export async function clearSession(paths = resolveRuntimePaths()): Promise<void> {
  const { unlink } = await import("node:fs/promises");
  try {
    await unlink(paths.sessionFile);
  } catch (error) {
    const err = error as NodeJS.ErrnoException;
    if (err.code !== "ENOENT") {
      throw new DcuError("runtime_error", `Unable to clear session state: ${asError(error).message}`);
    }
  }
}

function nativeCandidates(skillRoot: string, platform = process.platform): string[] {
  const win = isWindows(platform);
  const binary = win ? "desktop-computer-use-native.exe" : "desktop-computer-use-native";
  const triplet = win ? "win32-x64" : "linux-x64";
  return [
    join(skillRoot, "bin", triplet, binary),
    join(skillRoot, "..", "..", "bin", triplet, binary),
    join(process.cwd(), "bin", triplet, binary),
    join(process.cwd(), "build", binary),
    join(process.cwd(), "out", binary),
    join(process.cwd(), "build", "native", binary)
  ];
}

export async function resolveNativePath(skillRoot = resolveSkillRoot()): Promise<string> {
  const explicit = process.env.DCU_NATIVE_PATH;
  const candidates = explicit ? [explicit] : nativeCandidates(skillRoot);
  for (const candidate of candidates) {
    const resolved = isAbsolute(candidate) ? candidate : resolve(candidate);
    try {
      const info = await stat(resolved);
      if (!info.isFile()) continue;
      // Archives assembled on Windows can lose Unix executable bits. Restore
      // only the current owner's execute permission on the installed binary.
      if (process.platform !== "win32" && info.uid === process.getuid?.() && !(info.mode & 0o100)) {
        await chmod(resolved, info.mode | 0o100);
      }
      return resolved;
    } catch { /* next candidate */ }
  }
  throw new DcuError(
    "native_not_found",
    "Native executable not found. Set DCU_NATIVE_PATH or install the platform binary. " +
    `Searched: ${candidates.join(", ")}`
  );
}

export async function startNativeDaemon(paths = resolveRuntimePaths(), skillRoot = resolveSkillRoot()): Promise<void> {
  if (startPromise) {
    return startPromise;
  }
  startPromise = (async () => {
    await ensureRuntime(paths);
    await ensureToken(paths);
    const lock = await acquireDaemonLock(paths);
    if (lock.existing) {
      return;
    }
    if (!lock.handle) {
      throw new DcuError("runtime_error", "Daemon startup lock was not acquired");
    }
    try {
      if (await endpointReachable(paths.endpoint)) return;
      if (process.platform !== "win32") {
        const endpointDirectory = resolve(dirname(paths.endpoint));
        if (endpointDirectory === resolve(paths.directory)) {
          try {
            const endpointInfo = await lstat(paths.endpoint);
            const ownedByCurrentUser =
              typeof process.getuid !== "function" || endpointInfo.uid === process.getuid();
            if (endpointInfo.isSocket() && ownedByCurrentUser) {
              await unlink(paths.endpoint);
            }
          } catch (error) {
            const endpointError = error as NodeJS.ErrnoException;
            if (endpointError.code !== "ENOENT") {
              throw new DcuError(
                "runtime_error",
                `Unable to inspect the daemon endpoint: ${asError(error).message}`
              );
            }
          }
        }
      }
      const nativePath = await resolveNativePath(skillRoot);
      const daemonLogPath = join(paths.directory, "daemon.log");
      let daemonLog: FileHandle;
      try {
        daemonLog = await open(daemonLogPath, "a", 0o600);
        if (process.platform !== "win32") {
          await chmod(daemonLogPath, 0o600);
        }
      } catch (error) {
        throw new DcuError(
          "runtime_error",
          `Unable to open the private daemon log ${daemonLogPath}: ${asError(error).message}`
        );
      }

      try {
        const child = spawn(
          nativePath,
          ["--serve", "--endpoint", paths.endpoint, "--token-file", paths.tokenFile],
          {
            detached: true,
            stdio: ["ignore", daemonLog.fd, daemonLog.fd],
            windowsHide: true,
            cwd: dirname(nativePath)
          }
        );
        let childExited = false;
        child.once("exit", () => {
          childExited = true;
        });
        await new Promise<void>((resolvePromise, reject) => {
          child.once("spawn", () => resolvePromise());
          child.once("error", reject);
        });
        child.unref();

        // Hold the cross-process lock until the listener is reachable. This keeps
        // a second CLI from unlinking the new Unix socket during startup.
        const deadline = Date.now() + 10_000;
        while (Date.now() < deadline) {
          if (childExited) {
            throw new DcuError(
              "runtime_error",
              `Native daemon exited before opening ${paths.endpoint}. See ${daemonLogPath}`
            );
          }
          if (await endpointReachable(paths.endpoint)) {
            return;
          }
          await wait(50);
        }
        throw new DcuError(
          "transport_timeout",
          `Timed out waiting for native daemon at ${paths.endpoint}. See ${daemonLogPath}`
        );
      } finally {
        await daemonLog.close();
      }
    } finally {
      await releaseDaemonLock(paths, lock.handle);
    }
  })().finally(() => {
    startPromise = undefined;
  });
  return startPromise;
}

export function usageInstructions(platform = process.platform): string[] {
  const indicatorInstructions = [
    "Active sessions show a noninteractive top banner, high-contrast cursor ring, " +
    "and blue inward-fading screen-edge border.",
    "Press Esc or run `dcu session stop` to stop the session and release held input."
  ];
  if (isWindows(platform)) {
    return [
      ...indicatorInstructions,
      "Keep the target window visible on the active desktop.",
      "If Windows blocks input or capture, grant permissions to the native executable."
    ];
  }
  return [
    ...indicatorInstructions,
    "Run setup to install and enable the matching user-local GNOME extension, compile its schemas,",
    "and inspect Wayland portal requirements.",
    "Setup never logs out or restarts GNOME Shell; doctor remains the readiness gate.",
    "A graphical login session is required; SSH alone is not a desktop session.",
    "On Wayland, approve the RemoteDesktop and ScreenCast portal prompts when requested."
  ];
}

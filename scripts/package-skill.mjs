import { chmod, cp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { basename, join, relative, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const args = process.argv.slice(2);

function arg(name) {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : undefined;
}

const output = resolve(arg("--out") || join(root, "dist", "desktop-computer-use-skill"));
const buildOutputRoot = resolve(root, "dist");
const nativeDirArgument = arg("--native-dir");
const extensionDirArgument = arg("--extension-dir");
const nativeDir = nativeDirArgument ? resolve(nativeDirArgument) : undefined;
const extensionDir = extensionDirArgument ? resolve(extensionDirArgument) : undefined;
const adaptersOnly = args.includes("--adapters-only");

if (adaptersOnly && (nativeDir || extensionDir)) {
  throw new Error("--adapters-only cannot be combined with --native-dir or --extension-dir");
}

const requiredBinaries = {
  "win32-x64": "desktop-computer-use-native.exe",
  "linux-x64": "desktop-computer-use-native"
};
const requiredExtensions = [
  "desktop-computer-use-gnome-modern.zip",
  "desktop-computer-use-gnome-legacy.zip"
];

function isAncestor(parent, child) {
  return child === parent || child.startsWith(`${parent}${sep}`);
}
for (const forbidden of [root, homedir(), resolve(root, "skills"), resolve(root, "skills", "desktop-computer-use")]) {
  if (isAncestor(output, forbidden)) {
    throw new Error(`Refusing unsafe package output path: ${output}`);
  }
}
if (isAncestor(root, output) && !isAncestor(buildOutputRoot, output)) {
  throw new Error(`Package output inside the repository must stay below ${buildOutputRoot}: ${output}`);
}

async function runCommand(command, commandArgs) {
  await new Promise((resolvePromise, reject) => {
    const child = spawn(command, commandArgs, { cwd: root, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", code => {
      if (code === 0) {
        resolvePromise();
        return;
      }
      reject(new Error(`${command} exited with ${code}`));
    });
  });
}

const nativeSearchRoots = nativeDir ? [nativeDir] : [
  join(root, ".cache", "package-bin"),
  join(root, "dist", "native"),
  join(root, "artifacts", "native"),
  join(root, "build", "package-bin"),
  join(root, "build"),
  join(root, "build-win"),
  join(root, "build-linux")
];
const extensionSearchRoots = extensionDir ? [extensionDir] : [
  join(root, "extensions", "dist"),
  join(root, "dist", "extensions"),
  join(root, "artifacts", "extensions")
];

async function findFile(candidates) {
  for (const candidate of candidates) {
    try {
      if ((await stat(candidate)).isFile()) {
        return candidate;
      }
    } catch {
      // Continue searching the remaining artifact roots.
    }
  }
  return undefined;
}

async function findNativeBinary(triplet, binary) {
  const candidates = [];
  for (const searchRoot of nativeSearchRoots) {
    candidates.push(
      join(searchRoot, triplet, binary),
      join(searchRoot, triplet, "bin", binary),
      join(searchRoot, "bin", triplet, binary),
      join(searchRoot, binary),
      join(searchRoot, "bin", binary),
      join(searchRoot, "Release", binary)
    );
  }
  return findFile(candidates);
}

async function findExtensionArchive(name) {
  return findFile(extensionSearchRoots.map(searchRoot => join(searchRoot, name)));
}

const resolvedBinaries = new Map();
const resolvedExtensions = new Map();
if (!adaptersOnly) {
  for (const triplet of ["win32-x64", "linux-x64"]) {
    const binary = requiredBinaries[triplet];
    const source = await findNativeBinary(triplet, binary);
    if (!source) {
      throw new Error(
        `Missing required ${triplet} native binary. Searched ${nativeSearchRoots.join(", ")}. ` +
        "Use --native-dir <artifact-root> or --adapters-only for adapter tests."
      );
    }
    resolvedBinaries.set(triplet, { binary, source });
  }
  for (const name of requiredExtensions) {
    const source = await findExtensionArchive(name);
    if (!source) {
      throw new Error(
        `Missing required GNOME extension archive ${name}. Searched ${extensionSearchRoots.join(", ")}. ` +
        "Use --extension-dir <artifact-root> or --adapters-only for adapter tests."
      );
    }
    resolvedExtensions.set(name, source);
  }
}

await runCommand(process.execPath, [join(root, "scripts", "build-skill.mjs")]);
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
// One self-contained skill directory serves both installers: `npx skills add`
// copies only skills/desktop-computer-use/, and the Claude Code plugin at the
// package root discovers the same skills/*/SKILL.md.
const skillOutput = join(output, "skills", "desktop-computer-use");
const skillSource = join(root, "skills", "desktop-computer-use");
await cp(skillSource, skillOutput, {
  recursive: true,
  filter(source) {
    const topLevel = relative(skillSource, source).split(sep)[0];
    if (topLevel === "bin" || topLevel === "extensions") return false;
    const name = basename(source).toLowerCase();
    return name !== ".env" && !name.startsWith(".env.");
  }
});
await cp(join(root, "docs", "release-readme.md"), join(output, "README.md"));
const { version, description } = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
await mkdir(join(output, ".claude-plugin"), { recursive: true });
await writeFile(join(output, ".claude-plugin", "plugin.json"), `${JSON.stringify({
  name: "desktop-computer-use",
  version,
  description,
  author: { name: "j-token" },
  repository: "https://github.com/j-token/desktop-computer-use"
}, null, 2)}
`);
await writeFile(join(output, ".mcp.json"), `${JSON.stringify({
  mcpServers: {
    "desktop-computer-use": {
      command: "node",
      args: ["${CLAUDE_PLUGIN_ROOT}/skills/desktop-computer-use/scripts/dcu.mjs", "mcp", "serve"]
    }
  }
}, null, 2)}
`);
if (!adaptersOnly) {
  for (const triplet of ["win32-x64", "linux-x64"]) {
    const { binary, source } = resolvedBinaries.get(triplet);
    const destinationDirectory = join(skillOutput, "bin", triplet);
    await mkdir(destinationDirectory, { recursive: true });
    const destination = join(destinationDirectory, binary);
    await cp(source, destination);
    if (triplet === "linux-x64") {
      await chmod(destination, 0o755);
    }
  }
}
const packagedReferences = join(skillOutput, "references");
await mkdir(packagedReferences, { recursive: true });
await cp(join(root, "THIRD_PARTY_NOTICES.md"), join(packagedReferences, "THIRD_PARTY_NOTICES.md"));
await cp(
  join(root, "skills", "desktop-computer-use", "references", "THIRD_PARTY_LICENSES.md"),
  join(packagedReferences, "THIRD_PARTY_LICENSES.md")
);
if (!adaptersOnly) {
  await mkdir(join(skillOutput, "extensions"), { recursive: true });
  for (const name of requiredExtensions) {
    const source = resolvedExtensions.get(name);
    await cp(source, join(skillOutput, "extensions", name));
  }
}
console.log(`${adaptersOnly ? "Packaged adapter-only skill" : "Packaged complete skill"} at ${output}`);

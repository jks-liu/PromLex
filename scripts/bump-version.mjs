import { readFile, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const files = {
  packageJson: path.join(root, "package.json"),
  packageLock: path.join(root, "package-lock.json"),
  cargoToml: path.join(root, "src-tauri", "Cargo.toml"),
  cargoLock: path.join(root, "src-tauri", "Cargo.lock"),
  tauriConfig: path.join(root, "src-tauri", "tauri.conf.json"),
};

const argumentsList = process.argv.slice(2);
const buildRequested = argumentsList.includes("--build");
const noBuildRequested = argumentsList.includes("--no-build");
const currentRequested = argumentsList.includes("current");
if (argumentsList.includes("--current")) throw new Error("Use current without dashes to display the current version");
if (buildRequested && noBuildRequested) throw new Error("Use either --build or --no-build, not both");
if (currentRequested && (buildRequested || noBuildRequested)) throw new Error("current cannot be combined with --build or --no-build");
const versionArguments = argumentsList.filter((value) => !value.startsWith("--"));
if (currentRequested && versionArguments.some((value) => value !== "current")) throw new Error("Current version mode cannot be combined with a bump version");
if (versionArguments.length > 1) throw new Error("Only one version or release level can be provided");
const argument = versionArguments[0] || "patch";
const semverPattern = /^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$/;

function parseVersion(value) {
  const match = value.match(semverPattern);
  if (!match) throw new Error(`Invalid version: ${value}`);
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

function nextVersion(current) {
  if (semverPattern.test(argument)) return argument;
  const parsed = parseVersion(current);
  if (argument === "major") return `${parsed.major + 1}.0.0`;
  if (argument === "minor") return `${parsed.major}.${parsed.minor + 1}.0`;
  if (argument === "patch") return `${parsed.major}.${parsed.minor}.${parsed.patch + 1}`;
  throw new Error(`Expected patch, minor, major or a version such as 0.1.2; received: ${argument}`);
}

function replaceJsonVersions(text, current, next, count) {
  const pattern = new RegExp(`(\\"version\\"\\s*:\\s*)\\"${escapeRegExp(current)}\\"`, "g");
  let replacements = 0;
  const updated = text.replace(pattern, (_match, prefix) => {
    if (replacements >= count) return _match;
    replacements += 1;
    return `${prefix}\"${next}\"`;
  });
  if (replacements !== count) throw new Error(`Could not update ${count} version fields`);
  return updated;
}

function replaceOne(text, pattern, replacement, file) {
  if (!pattern.test(text)) throw new Error(`Could not find the PromLex version in ${file}`);
  return text.replace(pattern, replacement);
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

async function read(file) {
  return readFile(file, "utf8");
}

const packageText = await read(files.packageJson);
const packageLockText = await read(files.packageLock);
const cargoTomlText = await read(files.cargoToml);
const cargoLockText = await read(files.cargoLock);
const tauriConfigText = await read(files.tauriConfig);

const packageData = JSON.parse(packageText);
const packageLockData = JSON.parse(packageLockText);
const tauriConfigData = JSON.parse(tauriConfigText);
const current = packageData.version;
if (currentRequested) {
  console.log(current);
  process.exit(0);
}
const versions = [
  ["package-lock.json", packageLockData.version],
  ["package-lock.json packages root", packageLockData.packages?.["" ]?.version],
  ["Cargo.toml", cargoTomlText.match(/^version\s*=\s*\"([^\"]+)\"/m)?.[1]],
  ["Cargo.lock", cargoLockText.match(/name\s*=\s*\"promlex\"\r?\nversion\s*=\s*\"([^\"]+)\"/)?.[1]],
  ["tauri.conf.json", tauriConfigData.version],
];

for (const [name, version] of versions) {
  if (version !== current) throw new Error(`Version mismatch: ${name} is ${version || "missing"}, expected ${current}`);
}

const next = nextVersion(current);
const updatedCargoToml = replaceOne(
  cargoTomlText,
  /(^\[package\][\s\S]*?^version\s*=\s*\")([^\"]+)(\")/m,
  `$1${next}$3`,
  "Cargo.toml",
);
const updatedCargoLock = replaceOne(
  cargoLockText,
  /(name\s*=\s*\"promlex\"\r?\nversion\s*=\s*\")([^\"]+)(\")/,
  `$1${next}$3`,
  "Cargo.lock",
);

await writeFile(files.packageJson, replaceJsonVersions(packageText, current, next, 1));
await writeFile(files.packageLock, replaceJsonVersions(packageLockText, current, next, 2));
await writeFile(files.cargoToml, updatedCargoToml);
await writeFile(files.cargoLock, updatedCargoLock);
await writeFile(files.tauriConfig, replaceJsonVersions(tauriConfigText, current, next, 1));

console.log(`Bumped PromLex from ${current} to ${next}`);

if (buildRequested) {
  console.log("Building the Tauri application...");
  await runCommand(process.platform === "win32" ? "npm.cmd" : "npm", ["run", "tauri", "build"]);
}

function runCommand(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: root, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${command} exited with ${signal ? `signal ${signal}` : `code ${code}`}`));
      }
    });
  });
}

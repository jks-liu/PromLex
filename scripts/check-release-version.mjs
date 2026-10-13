import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const tag = process.argv[2] || process.env.GITHUB_REF_NAME || "";
const match = /^releases\/v((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$/.exec(tag);
if (!match) throw new Error(`Expected a stable release tag such as releases/v1.0.2; received: ${tag}`);
const expected = match[1];

async function read(relativePath) {
  return readFile(path.join(root, relativePath), "utf8");
}

const [packageText, packageLockText, cargoText, cargoLockText, tauriText] = await Promise.all([
  read("package.json"),
  read("package-lock.json"),
  read("src-tauri/Cargo.toml"),
  read("src-tauri/Cargo.lock"),
  read("src-tauri/tauri.conf.json"),
]);
const packageLock = JSON.parse(packageLockText);
const cargoPackage = cargoText.split(/^\[package\]\s*$/m)[1]?.split(/^\[/m)[0];
const cargoLockPackage = cargoLockText.split(/^\[\[package\]\]\s*$/m)
  .find((section) => /^name\s*=\s*"promlex"\s*$/m.test(section));
const versions = [
  ["package.json", JSON.parse(packageText).version],
  ["package-lock.json", packageLock.version],
  ["package-lock.json packages root", packageLock.packages?.[""]?.version],
  ["src-tauri/Cargo.toml", cargoPackage?.match(/^version\s*=\s*"([^"]+)"/m)?.[1]],
  ["src-tauri/Cargo.lock", cargoLockPackage?.match(/^version\s*=\s*"([^"]+)"/m)?.[1]],
  ["src-tauri/tauri.conf.json", JSON.parse(tauriText).version],
];
for (const [file, version] of versions) {
  if (version !== expected) {
    throw new Error(`Version mismatch: ${file} is ${version || "missing"}, but ${tag} requires ${expected}`);
  }
}
console.log(`Release versions match ${tag}`);

#!/usr/bin/env node
// Bring the package-manager metadata in this repo in line with one GitHub
// release: the Homebrew cask in Casks/ and the winget manifests under
// packaging/winget/. Checksums are taken from the release's own asset
// digests (downloading and hashing only as a fallback); the MSI ProductCode
// winget needs is read from the installer itself via msitools.
//
//   node scripts/update-packaging.mjs v0.1.0            # rewrite files
//   node scripts/update-packaging.mjs v0.1.0 --check    # exit 1 if stale
//   node scripts/update-packaging.mjs v0.1.0 --product-code '{...}'
//   node scripts/update-packaging.mjs v0.1.0 --release-json release.json
//
// Runs on Node 20+ with no dependencies. GITHUB_TOKEN is used if set (the
// unauthenticated API limit is low on shared runners). --release-json reads
// the release object from a file instead of the API — for working offline,
// and for checking the renderers against known metadata.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = "Q01P/MCPanel";
const WINGET_ID = "Q01P.MCPanel";
const WINGET_SCHEMA = "1.9.0";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function usage(message) {
  console.error(`error: ${message}\n\nusage: update-packaging.mjs <tag> [--check] [--product-code '{GUID}']`);
  process.exit(2);
}

const args = process.argv.slice(2);
const tag = args.find((a) => !a.startsWith("--"));
if (!tag || !/^v\d+\.\d+\.\d+$/.test(tag)) usage("expected a tag like v0.1.0");
const version = tag.slice(1);
const check = args.includes("--check");
const flag = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const productCodeArg = flag("--product-code");
const releaseJsonArg = flag("--release-json");

// --- release metadata ------------------------------------------------------

async function fetchRelease() {
  if (releaseJsonArg) {
    const release = JSON.parse(await readFile(releaseJsonArg, "utf8"));
    if (release.tag_name !== tag) {
      throw new Error(`${releaseJsonArg} describes ${release.tag_name}, not ${tag}`);
    }
    return release;
  }
  const url = `https://api.github.com/repos/${REPO}/releases/tags/${tag}`;
  const headers = { Accept: "application/vnd.github+json", "User-Agent": "mcpanel-packaging" };
  const token = process.env.GITHUB_TOKEN;
  let res = await fetch(url, {
    headers: token ? { ...headers, Authorization: `Bearer ${token}` } : headers,
  });
  // Releases are public: a stale or foreign token in the environment must
  // not block reading one. Retry anonymously and say so.
  if (res.status === 401 && token) {
    console.error("note: GITHUB_TOKEN was rejected; retrying without it");
    res = await fetch(url, { headers });
  }
  if (!res.ok) throw new Error(`release ${tag}: HTTP ${res.status} ${await res.text()}`);
  return res.json();
}

function asset(release, name) {
  const found = release.assets.find((a) => a.name === name);
  if (!found) {
    const have = release.assets.map((a) => a.name).join(", ");
    throw new Error(`release ${tag} has no asset "${name}" (have: ${have})`);
  }
  return found;
}

async function download(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`download ${url}: HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

const sha256 = (buffer) => createHash("sha256").update(buffer).digest("hex");

/** The release API reports a digest per asset; trust it, but fall back to
 * hashing the download for releases that predate that field. */
async function digestOf(a) {
  const match = /^sha256:([0-9a-f]{64})$/.exec(a.digest ?? "");
  if (match) return match[1];
  console.error(`note: no digest for ${a.name}; downloading to hash`);
  return sha256(await download(a.browser_download_url));
}

// --- MSI ProductCode -------------------------------------------------------

async function productCodeOf(msi, expectedDigest) {
  if (productCodeArg) {
    if (!/^\{[0-9A-F-]{36}\}$/i.test(productCodeArg)) usage("--product-code must be a braced GUID");
    return productCodeArg.toUpperCase();
  }
  const bytes = await download(msi.browser_download_url);
  const actual = sha256(bytes);
  if (actual !== expectedDigest) {
    throw new Error(`${msi.name}: downloaded sha256 ${actual} != release digest ${expectedDigest}`);
  }
  const dir = await mkdtemp(path.join(tmpdir(), "mcpanel-msi-"));
  try {
    const file = path.join(dir, msi.name);
    await writeFile(file, bytes);
    let table;
    try {
      table = execFileSync("msiinfo", ["export", file, "Property"], { encoding: "utf8" });
    } catch (error) {
      throw new Error(
        `msiinfo (msitools) is needed to read the MSI ProductCode; install it or pass --product-code. (${error.message})`,
      );
    }
    const line = table.split("\n").find((l) => l.startsWith("ProductCode\t"));
    if (!line) throw new Error("ProductCode not found in the MSI Property table");
    return line.split("\t")[1].trim().toUpperCase();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

// --- renderers -------------------------------------------------------------

function renderCask(existing, digests) {
  let out = existing;
  const replace = (pattern, replacement, what) => {
    if (!pattern.test(out)) throw new Error(`cask: could not find the ${what} line to rewrite`);
    out = out.replace(pattern, replacement);
  };
  replace(/^(\s*version )"[^"]*"/m, `$1"${version}"`, "version");
  replace(/^(\s*sha256 arm:\s+)"[0-9a-f]{64}"/m, `$1"${digests.arm}"`, "arm sha256");
  replace(/^(\s*intel:\s+)"[0-9a-f]{64}"/m, `$1"${digests.intel}"`, "intel sha256");
  return out;
}

const generated = "# Generated by scripts/update-packaging.mjs — edit the script, not this file.";

function renderWingetVersion() {
  return `# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.${WINGET_SCHEMA}.schema.json
${generated}
PackageIdentifier: ${WINGET_ID}
PackageVersion: ${version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: ${WINGET_SCHEMA}
`;
}

function renderWingetInstaller(msiName, msiDigest, productCode) {
  return `# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.${WINGET_SCHEMA}.schema.json
${generated}
PackageIdentifier: ${WINGET_ID}
PackageVersion: ${version}
InstallerLocale: en-US
InstallerType: wix
Scope: machine
UpgradeBehavior: install
Installers:
  - Architecture: x64
    InstallerUrl: https://github.com/${REPO}/releases/download/${tag}/${msiName}
    InstallerSha256: ${msiDigest.toUpperCase()}
    ProductCode: '${productCode}'
ManifestType: installer
ManifestVersion: ${WINGET_SCHEMA}
`;
}

function renderWingetLocale() {
  return `# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.${WINGET_SCHEMA}.schema.json
${generated}
PackageIdentifier: ${WINGET_ID}
PackageVersion: ${version}
PackageLocale: en-US
Publisher: Q01P
PublisherUrl: https://github.com/Q01P
PublisherSupportUrl: https://github.com/${REPO}/issues
PackageName: MCPanel
PackageUrl: https://github.com/${REPO}
License: MIT
LicenseUrl: https://github.com/${REPO}/blob/main/LICENSE
ShortDescription: Control panel for local MCP servers
Description: |-
  A lightweight desktop app for managing local MCP (Model Context Protocol) servers.
  Toggle servers on and off like services, watch their logs stream live, browse a
  running server's tools and call them from a generated form, or hand-craft raw
  JSON-RPC requests. Servers already configured in other MCP clients can be imported,
  with credentials moved into the Windows Credential Manager.
Tags:
  - developer-tools
  - mcp
  - model-context-protocol
ReleaseNotesUrl: https://github.com/${REPO}/releases/tag/${tag}
ManifestType: defaultLocale
ManifestVersion: ${WINGET_SCHEMA}
`;
}

// --- main ------------------------------------------------------------------

const release = await fetchRelease();
const dmgArm = asset(release, `MCPanel_${version}_aarch64.dmg`);
const dmgIntel = asset(release, `MCPanel_${version}_x64.dmg`);
const msi = asset(release, `MCPanel_${version}_x64_en-US.msi`);

const [arm, intel, msiDigest] = await Promise.all([digestOf(dmgArm), digestOf(dmgIntel), digestOf(msi)]);
const productCode = await productCodeOf(msi, msiDigest);

const caskPath = path.join(root, "Casks", "mcpanel.rb");
const wingetDir = path.join(root, "packaging", "winget", "manifests", "q", "Q01P", "MCPanel", version);
const files = new Map([
  [caskPath, renderCask(await readFile(caskPath, "utf8"), { arm, intel })],
  [path.join(wingetDir, `${WINGET_ID}.yaml`), renderWingetVersion()],
  [path.join(wingetDir, `${WINGET_ID}.installer.yaml`), renderWingetInstaller(msi.name, msiDigest, productCode)],
  [path.join(wingetDir, `${WINGET_ID}.locale.en-US.yaml`), renderWingetLocale()],
]);

let stale = 0;
for (const [file, content] of files) {
  const current = await readFile(file, "utf8").catch(() => null);
  const rel = path.relative(root, file);
  if (current === content) {
    console.log(`up to date  ${rel}`);
    continue;
  }
  stale++;
  if (check) {
    console.log(`STALE       ${rel}`);
  } else {
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, content);
    console.log(`wrote       ${rel}`);
  }
}

console.log(`\n${tag}: dmg arm ${arm.slice(0, 12)}… intel ${intel.slice(0, 12)}… msi ${msiDigest.slice(0, 12)}… ProductCode ${productCode}`);
if (check && stale > 0) {
  console.error(`\n${stale} packaging file(s) are out of date for ${tag}; run without --check to rewrite them.`);
  process.exit(1);
}

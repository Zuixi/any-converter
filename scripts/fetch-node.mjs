#!/usr/bin/env node
// Downloads the Node runtime for the desktop app's plugin hosts and puts
// it where Tauri's `bundle.externalBin` expects it:
// apps/desktop/src-tauri/binaries/node-<target-triple>(.exe).
//
// The server spawns plugin hosts through this binary (plugins/host.rs
// `default_launch` reads ANY_CONVERTER_NODE, which the desktop app points
// at the bundled copy) — the same job magpie gives its bundled Bun.
//
// Usage:  node scripts/fetch-node.mjs [version]   (default: the LTS below)
// Run it before `pnpm tauri build`; CI does the same.

import { execFileSync } from "node:child_process";
import { createWriteStream } from "node:fs";
import { statSync } from "node:fs";
import { mkdir, rename, rm, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";

const DEFAULT_VERSION = "22.14.0";
const DIST = "https://nodejs.org/dist";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "apps", "desktop", "src-tauri", "binaries");

/** Tauri's target triple for the platform this script runs on. */
function targetTriple() {
  const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : null;
  if (!arch) throw new Error(`unsupported arch ${process.arch}`);
  if (process.platform === "win32") return `${arch}-pc-windows-msvc`;
  if (process.platform === "darwin") return `${arch}-apple-darwin`;
  return `${arch}-unknown-linux-gnu`;
}

async function download(url, file) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} → HTTP ${response.status}`);
  await pipeline(Readable.fromWeb(response.body), createWriteStream(file));
}

/** Windows' bundled bsdtar, by absolute path (PATH's `tar` may be GNU's). */
function systemTar() {
  const root = process.env.SystemRoot ?? "C:\\Windows";
  const candidate = join(root, "System32", "tar.exe");
  try {
    statSync(candidate);
    return candidate;
  } catch {
    return null;
  }
}

async function main() {
  const version = process.argv[2] ?? DEFAULT_VERSION;
  const triple = targetTriple();
  const isWindows = process.platform === "win32";
  const binary = isWindows ? "node.exe" : "node";
  const archive = isWindows
    ? `node-v${version}-win-${process.arch === "arm64" ? "arm64" : "x64"}.zip`
    : `node-v${version}-${process.platform}-${process.arch}.tar.gz`;
  const destination = join(outDir, `node-${triple}${isWindows ? ".exe" : ""}`);

  try {
    if ((await stat(destination)).isFile()) {
      console.log(`node ${version} already at ${destination} — nothing to do`);
      return;
    }
  } catch {
    // not there yet
  }

  await mkdir(join(root, "tmp-node"), { recursive: true });
  const archivePath = join(root, "tmp-node", archive);
  console.log(`downloading ${DIST}/v${version}/${archive} …`);
  await download(`${DIST}/v${version}/${archive}`, archivePath);

  console.log("extracting…");
  const extractDir = join(root, "tmp-node", "extracted");
  await rm(extractDir, { recursive: true, force: true });
  await mkdir(extractDir, { recursive: true });
  // bsdtar reads zip and tar.gz alike and understands --strip-components;
  // GNU tar (Git Bash's PATH default) does neither for zip. On Windows
  // call the system bsdtar by its absolute name — PATH's `tar` is GNU's —
  // and fall back to PowerShell's Expand-Archive + a manual move when the
  // machine is older than the bundled tar.
  const archivePosix = archive.split("\\").join("/");
  if (process.platform === "win32" && systemTar()) {
    execFileSync(systemTar(), ["-xf", archivePosix, "-C", "extracted", "--strip-components=1"], {
      cwd: join(root, "tmp-node"),
    });
  } else if (process.platform === "win32") {
    execFileSync("powershell", [
      "-NoProfile",
      "-Command",
      `Expand-Archive -LiteralPath '${archivePosix}' -DestinationPath 'extracted' -Force`,
    ], { cwd: join(root, "tmp-node") });
    const inner = join(extractDir, archive.replace(/\.zip$/, ""));
    await rename(join(inner, "node.exe"), join(extractDir, "node.exe"));
    await rm(inner, { recursive: true, force: true });
  } else {
    execFileSync("tar", ["-xf", archive, "-C", "extracted", "--strip-components=1"], {
      cwd: join(root, "tmp-node"),
    });
  }
  await mkdir(outDir, { recursive: true });
  await rename(join(extractDir, binary), destination);
  await rm(join(root, "tmp-node"), { recursive: true, force: true });
  console.log(`node ${version} → ${destination}`);
}

main().catch((error) => {
  console.error(String(error));
  process.exit(1);
});

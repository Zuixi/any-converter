#!/usr/bin/env node
/**
 * Cross-platform build script for any-converter (macOS / Linux / Windows).
 *
 * Features:
 * - Zero external dependencies (pure Node.js >= 20 ESM)
 * - Automatic CPU / RAM budget detection to avoid OOM during Rust + Vite builds
 * - Process-group / process-tree tracking and reliable cleanup on Ctrl+C,
 *   SIGTERM, errors, or step timeouts (no orphan rustc / ld / node / tauri)
 * - Supports building: ui, cli (with embedded SPA), desktop (Tauri), workspace, all
 *
 * Usage:
 *   node scripts/build.mjs [target] [options]
 *   node scripts/build.mjs --help
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const IS_WINDOWS = process.platform === "win32";
const ROOT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DESKTOP_DIR = path.join(ROOT_DIR, "apps", "desktop");
const UI_DIST_DIR = path.join(DESKTOP_DIR, "dist");
const TARGET_DIR = path.join(ROOT_DIR, "target");

const VALID_TARGETS = new Set(["dev", "ui", "cli", "desktop", "workspace", "all"]);

// ---------------------------------------------------------------------------
// Active child process registry & signal-safe process-tree reclamation
// ---------------------------------------------------------------------------

/** @type {Set<import("node:child_process").ChildProcess>} */
const activeChildren = new Set();
let shuttingDown = false;
let shutdownExitCode = 0;

/**
 * Kill a child process and its entire descendant tree across platforms.
 * - Unix: negative PID targets the detached process group.
 * - Windows: `taskkill /PID <pid> /T /F` terminates the whole process tree.
 *
 * @param {import("node:child_process").ChildProcess} child
 * @param {boolean} force
 */
function killProcessTree(child, force = false) {
  const pid = child.pid;
  if (!pid || child.exitCode !== null || child.signalCode !== null) {
    return;
  }

  if (IS_WINDOWS) {
    const args = ["/PID", String(pid), "/T"];
    if (force) args.push("/F");
    spawnSync("taskkill", args, { stdio: "ignore", windowsHide: true });
    return;
  }

  const sig = force ? "SIGKILL" : "SIGTERM";
  try {
    // Negative PID kills the entire process group created by `detached: true`.
    process.kill(-pid, sig);
  } catch (err) {
    if (err && err.code !== "ESRCH") {
      try {
        process.kill(pid, sig);
      } catch {
        // Already exited.
      }
    }
  }
}

/**
 * Gracefully terminate all active child process trees, then force-kill any
 * stragglers after `graceMs` milliseconds.
 *
 * @param {string} reason
 * @param {number} exitCode
 * @param {number} graceMs
 */
async function reclaimAllProcesses(reason, exitCode = 130, graceMs = 3000) {
  if (shuttingDown) return;
  shuttingDown = true;
  shutdownExitCode = exitCode;

  if (activeChildren.size === 0) {
    process.exit(exitCode);
  }

  logWarn(`Received ${reason}; reclaiming ${activeChildren.size} active process tree(s)...`);

  // Phase 1: graceful termination (SIGTERM on Unix, taskkill /T on Windows)
  for (const child of activeChildren) {
    killProcessTree(child, false);
  }

  const deadline = Date.now() + graceMs;
  while (activeChildren.size > 0 && Date.now() < deadline) {
    await sleep(100);
  }

  // Phase 2: force kill any survivors
  if (activeChildren.size > 0) {
    logWarn(`Force-killing ${activeChildren.size} remaining process tree(s)...`);
    for (const child of activeChildren) {
      killProcessTree(child, true);
    }
    await sleep(200);
  }

  process.exit(shutdownExitCode);
}

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    const code = signal === "SIGINT" ? 130 : signal === "SIGTERM" ? 143 : 129;
    void reclaimAllProcesses(signal, code);
  });
}

process.on("uncaughtException", (err) => {
  logError(`Uncaught exception: ${err.stack || err.message}`);
  void reclaimAllProcesses("uncaughtException", 1);
});

process.on("unhandledRejection", (reason) => {
  logError(`Unhandled rejection: ${reason instanceof Error ? reason.stack : String(reason)}`);
  void reclaimAllProcesses("unhandledRejection", 1);
});

// ---------------------------------------------------------------------------
// Logging helpers
// ---------------------------------------------------------------------------

function timestamp() {
  return new Date().toTimeString().slice(0, 8);
}

function logInfo(msg) {
  console.log(`[${timestamp()}] [build] ${msg}`);
}

function logWarn(msg) {
  console.warn(`[${timestamp()}] [warn]  ${msg}`);
}

function logError(msg) {
  console.error(`[${timestamp()}] [error] ${msg}`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatDuration(ms) {
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  const m = Math.floor(sec / 60);
  const s = (sec % 60).toFixed(0).padStart(2, "0");
  return `${m}m${s}s`;
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

function printHelp() {
  console.log(`any-converter cross-platform build script

Usage:
  node scripts/build.mjs [target] [options]

Targets (default: cli):
  dev         Start Rust gateway + Vite dev server together (one-command local dev)
  ui          Install deps (unless --skip-install) and build Vite SPA (apps/desktop/dist)
  cli         Build SPA + compile 'any-converter' CLI/gateway binary with embedded UI
  desktop     Fetch Node sidecar + build SPA + package Tauri desktop app
  workspace   Compile all Rust workspace crates
  all         Build ui, cli, and desktop

Options:
  --release              Release build (default; dev mode defaults to debug)
  --debug                Debug build
  --port <n>             Gateway port in 'dev' mode (default: 3028)
  --ui-port <n>          Vite UI port in 'dev' mode (default: 8420)
  --desktop              In 'dev' mode, launch Tauri desktop window instead of browser mode
  --jobs <n>             Override Cargo parallel jobs (default: auto by CPU & RAM)
  --node-mem <mb>        Override Node.js --max-old-space-size in MB (default: auto)
  --low-memory           Conservative memory profile (fewer parallel rustc/linker jobs)
  --rust-target <triple> Pass --target <triple> to cargo build (e.g. x86_64-apple-darwin)
  --skip-install         Skip 'pnpm install'
  --skip-node            Skip 'scripts/fetch-node.mjs' before desktop build
  --no-ui                Skip SPA build when building 'cli' or 'workspace'
  --check                Run fmt, clippy, and pnpm typecheck before building
  --test                 Run 'cargo test --workspace' before building
  --clean                Clean target/ and apps/desktop/dist before building
  --clean-after          Remove intermediate target/debug or incremental artifacts after build
  --timeout <minutes>    Max execution time per step in minutes (0 = unlimited, default: 0)
  --dry-run              Print computed resource profile and commands without running
  -h, --help             Show this help message

Examples:
  node scripts/build.mjs dev                    # Start backend gateway + Vite UI (browser HMR)
  node scripts/build.mjs dev --desktop          # Start Tauri desktop app in dev mode
  node scripts/build.mjs                        # Build embedded-UI release CLI/gateway
  node scripts/build.mjs desktop --low-memory   # Memory-safe Tauri desktop build
  node scripts/build.mjs all --check --test     # Full verification + CLI + Desktop build
  node scripts/build.mjs cli --rust-target x86_64-apple-darwin
`);
}

function parseArgs(argv) {
  const opts = {
    target: "cli",
    targetExplicit: false,
    profile: null,
    port: 3028,
    uiPort: 8420,
    desktopDev: false,
    jobs: null,
    nodeMemMb: null,
    lowMemory: false,
    rustTarget: null,
    skipInstall: false,
    skipNode: false,
    noUi: false,
    check: false,
    test: false,
    clean: false,
    cleanAfter: false,
    timeoutMin: 0,
    dryRun: false,
    help: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "-h" || arg === "--help") {
      opts.help = true;
    } else if (arg === "--release") {
      opts.profile = "release";
    } else if (arg === "--debug") {
      opts.profile = "debug";
    } else if (arg === "--desktop") {
      opts.desktopDev = true;
    } else if (arg === "--port") {
      const val = Number.parseInt(argv[++i], 10);
      if (!Number.isFinite(val) || val < 1 || val > 65535) {
        throw new Error("--port must be an integer between 1 and 65535");
      }
      opts.port = val;
    } else if (arg === "--ui-port") {
      const val = Number.parseInt(argv[++i], 10);
      if (!Number.isFinite(val) || val < 1 || val > 65535) {
        throw new Error("--ui-port must be an integer between 1 and 65535");
      }
      opts.uiPort = val;
    } else if (arg === "--low-memory") {
      opts.lowMemory = true;
    } else if (arg === "--skip-install") {
      opts.skipInstall = true;
    } else if (arg === "--skip-node") {
      opts.skipNode = true;
    } else if (arg === "--no-ui") {
      opts.noUi = true;
    } else if (arg === "--check") {
      opts.check = true;
    } else if (arg === "--test") {
      opts.test = true;
    } else if (arg === "--clean") {
      opts.clean = true;
    } else if (arg === "--clean-after") {
      opts.cleanAfter = true;
    } else if (arg === "--dry-run") {
      opts.dryRun = true;
    } else if (arg === "--jobs") {
      const val = Number.parseInt(argv[++i], 10);
      if (!Number.isFinite(val) || val < 1) {
        throw new Error("--jobs must be a positive integer");
      }
      opts.jobs = val;
    } else if (arg === "--node-mem") {
      const val = Number.parseInt(argv[++i], 10);
      if (!Number.isFinite(val) || val < 256) {
        throw new Error("--node-mem must be an integer >= 256 (MB)");
      }
      opts.nodeMemMb = val;
    } else if (arg === "--rust-target") {
      const val = argv[++i];
      if (!val || val.startsWith("-")) {
        throw new Error("--rust-target requires a target triple");
      }
      opts.rustTarget = val;
    } else if (arg === "--timeout") {
      const val = Number.parseFloat(argv[++i]);
      if (!Number.isFinite(val) || val < 0) {
        throw new Error("--timeout must be a non-negative number (minutes)");
      }
      opts.timeoutMin = val;
    } else if (!arg.startsWith("-")) {
      if (opts.targetExplicit) {
        throw new Error(`Unexpected extra positional argument: ${arg}`);
      }
      if (!VALID_TARGETS.has(arg)) {
        throw new Error(
          `Unknown target '${arg}'. Expected one of: ${[...VALID_TARGETS].join(", ")}`
        );
      }
      opts.target = arg;
      opts.targetExplicit = true;
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }

  if (opts.profile === null) {
    opts.profile = opts.target === "dev" ? "debug" : "release";
  }

  return opts;
}

// ---------------------------------------------------------------------------
// Hardware & memory-aware resource planning
// ---------------------------------------------------------------------------

function computeResourcePlan(opts) {
  const cpuCount =
    typeof os.availableParallelism === "function"
      ? os.availableParallelism()
      : Math.max(1, os.cpus().length);
  const totalMemGb = os.totalmem() / 1024 ** 3;
  const freeMemGb = os.freemem() / 1024 ** 3;

  // Rust release/LTO + linker jobs can easily spike to ~1.5–2.5 GiB each.
  // Budget conservatively by RAM so local machines don't swap-thrash or OOM.
  const gbPerJob = opts.lowMemory ? 3.0 : opts.profile === "release" ? 2.0 : 1.25;
  const ramCappedJobs = Math.max(1, Math.floor((totalMemGb * 0.75) / gbPerJob));
  const cpuCappedJobs = opts.lowMemory
    ? Math.max(1, Math.min(2, Math.floor(cpuCount / 2)))
    : Math.max(1, cpuCount > 2 ? cpuCount - 1 : cpuCount);

  const cargoJobs = opts.jobs ?? Math.max(1, Math.min(cpuCappedJobs, ramCappedJobs));

  // Node heap limit for tsc / vite: 25% of RAM, clamped to [1024, 4096] MB
  // (or [1024, 2048] MB in --low-memory mode).
  const maxNodeCap = opts.lowMemory ? 2048 : 4096;
  const autoNodeMemMb = Math.min(
    maxNodeCap,
    Math.max(1024, Math.floor((totalMemGb * 1024) * 0.25))
  );
  const nodeMemMb = opts.nodeMemMb ?? autoNodeMemMb;

  return {
    platform: `${process.platform}/${process.arch}`,
    cpuCount,
    totalMemGb: Number(totalMemGb.toFixed(1)),
    freeMemGb: Number(freeMemGb.toFixed(1)),
    cargoJobs,
    nodeMemMb,
    incremental: opts.profile === "release" || opts.lowMemory ? "0" : "1",
  };
}

function buildStepEnv(plan, opts) {
  const env = { ...process.env };

  env.CARGO_BUILD_JOBS = String(plan.cargoJobs);
  env.CARGO_INCREMENTAL = plan.incremental;
  env.CARGO_TERM_COLOR = env.CARGO_TERM_COLOR || "always";

  if (opts.lowMemory && opts.profile === "release") {
    // Reduce peak LLVM codegen memory in low-memory mode.
    env.CARGO_PROFILE_RELEASE_CODEGEN_UNITS = env.CARGO_PROFILE_RELEASE_CODEGEN_UNITS || "1";
    env.CARGO_PROFILE_RELEASE_LTO = env.CARGO_PROFILE_RELEASE_LTO || "thin";
  }

  // Merge --max-old-space-size into NODE_OPTIONS without duplicating it.
  const existingNodeOpts = (env.NODE_OPTIONS || "")
    .replace(/--max-old-space-size=\d+/g, "")
    .trim();
  env.NODE_OPTIONS = `${existingNodeOpts} --max-old-space-size=${plan.nodeMemMb}`.trim();

  // Avoid Turbo background daemon lingering after one-shot builds.
  env.TURBO_DAEMON = "false";

  return env;
}

// ---------------------------------------------------------------------------
// Cross-platform command execution with timeout & tree cleanup
// ---------------------------------------------------------------------------

function resolveCommand(cmd) {
  if (!IS_WINDOWS) return { command: cmd, shell: false };
  // On Windows, pnpm is typically a .cmd shim and requires cmd.exe / shell.
  if (cmd === "pnpm") return { command: "pnpm.cmd", shell: true };
  return { command: cmd, shell: false };
}

function checkTool(cmd, args = ["--version"]) {
  const { command, shell } = resolveCommand(cmd);
  const res = spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    shell,
    windowsHide: true,
  });
  if (res.error || res.status !== 0) {
    return null;
  }
  return (res.stdout || res.stderr || "").trim().split(/\r?\n/)[0];
}

function verifyToolchain(opts) {
  const needsNode = opts.target !== "workspace" || opts.check;
  const needsRust = opts.target !== "ui" || opts.check || opts.test;

  const missing = [];
  if (needsNode) {
    const nodeVer = checkTool("node");
    const pnpmVer = checkTool("pnpm");
    if (!nodeVer) missing.push("node (>= 20)");
    if (!pnpmVer) missing.push("pnpm (>= 9)");
    if (nodeVer && pnpmVer) {
      logInfo(`Toolchain (JS):   node ${nodeVer}, pnpm ${pnpmVer}`);
    }
  }
  if (needsRust) {
    const rustcVer = checkTool("rustc");
    const cargoVer = checkTool("cargo");
    if (!rustcVer) missing.push("rustc (>= 1.85)");
    if (!cargoVer) missing.push("cargo (>= 1.85)");
    if (rustcVer && cargoVer) {
      logInfo(`Toolchain (Rust): ${rustcVer}`);
    }
  }

  if (missing.length > 0) {
    throw new Error(`Missing required build tools: ${missing.join(", ")}`);
  }
}

/**
 * Spawn a command as its own process group (Unix) or tracked tree (Windows),
 * streaming stdio and guaranteeing cleanup on abort or timeout.
 */
async function runStep(label, cmd, args, { env, cwd = ROOT_DIR, timeoutMin = 0, dryRun = false }) {
  const cmdLine = `${cmd} ${args.join(" ")}`.trim();
  logInfo(`▶ ${label}`);
  logInfo(`  $ ${cmdLine}`);

  if (dryRun) {
    return;
  }

  const startedAt = Date.now();
  const { command, shell } = resolveCommand(cmd);

  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: "inherit",
      // Detached on Unix creates a new process group (PGID = child.pid),
      // allowing `process.kill(-child.pid, sig)` to reclaim all grandchildren.
      detached: !IS_WINDOWS,
      shell,
      windowsHide: IS_WINDOWS,
    });

    activeChildren.add(child);

    let timer = null;
    let timedOut = false;
    if (timeoutMin > 0) {
      const timeoutMs = Math.round(timeoutMin * 60 * 1000);
      timer = setTimeout(() => {
        timedOut = true;
        logError(`Step '${label}' exceeded timeout of ${timeoutMin}m; terminating process tree...`);
        killProcessTree(child, false);
        setTimeout(() => killProcessTree(child, true), 3000).unref();
      }, timeoutMs);
      timer.unref();
    }

    child.on("error", (err) => {
      if (timer) clearTimeout(timer);
      activeChildren.delete(child);
      reject(new Error(`Failed to start '${cmd}': ${err.message}`));
    });

    child.on("close", (code, signal) => {
      if (timer) clearTimeout(timer);
      activeChildren.delete(child);

      if (timedOut) {
        reject(new Error(`Step '${label}' timed out after ${timeoutMin}m`));
        return;
      }
      if (signal) {
        reject(new Error(`Step '${label}' terminated by signal ${signal}`));
        return;
      }
      if (code !== 0) {
        reject(new Error(`Step '${label}' failed with exit code ${code}`));
        return;
      }
      logInfo(`✔ ${label} (${formatDuration(Date.now() - startedAt)})`);
      resolve();
    });
  });
}

// ---------------------------------------------------------------------------
// Clean & artifact summary helpers
// ---------------------------------------------------------------------------

async function cleanBeforeBuild(dryRun) {
  const pathsToRemove = [UI_DIST_DIR, TARGET_DIR];
  for (const p of pathsToRemove) {
    if (existsSync(p)) {
      logInfo(`Cleaning ${path.relative(ROOT_DIR, p)} ...`);
      if (!dryRun) {
        await rm(p, { recursive: true, force: true });
      }
    }
  }
}

async function reclaimBuildArtifacts(opts) {
  // Remove incremental compilation caches to reclaim disk space while keeping
  // the final release/debug binaries intact.
  const subdirs = [
    path.join(TARGET_DIR, opts.profile, "incremental"),
    ...(opts.rustTarget
      ? [path.join(TARGET_DIR, opts.rustTarget, opts.profile, "incremental")]
      : []),
  ];
  for (const dir of subdirs) {
    if (existsSync(dir)) {
      logInfo(`Reclaiming incremental cache: ${path.relative(ROOT_DIR, dir)}`);
      if (!opts.dryRun) {
        await rm(dir, { recursive: true, force: true });
      }
    }
  }
}

async function printArtifactSummary(opts) {
  if (opts.dryRun) return;

  const binExt = IS_WINDOWS ? ".exe" : "";
  const rustOutDir = opts.rustTarget
    ? path.join(TARGET_DIR, opts.rustTarget, opts.profile)
    : path.join(TARGET_DIR, opts.profile);

  const candidates = [];
  if (opts.target === "ui" || opts.target === "all" || (opts.target === "cli" && !opts.noUi)) {
    candidates.push({ label: "SPA dist", file: path.join(UI_DIST_DIR, "index.html") });
  }
  if (opts.target === "cli" || opts.target === "workspace" || opts.target === "all") {
    candidates.push({
      label: "CLI / Gateway binary",
      file: path.join(rustOutDir, `any-converter${binExt}`),
    });
  }
  if (opts.target === "desktop" || opts.target === "all") {
    candidates.push({
      label: "Desktop bundle dir",
      file: path.join(rustOutDir, "bundle"),
    });
  }

  logInfo("--- Build Artifacts ---");
  for (const item of candidates) {
    try {
      const info = await stat(item.file);
      const sizeStr = info.isFile() ? ` (${(info.size / 1024 / 1024).toFixed(1)} MiB)` : "";
      logInfo(`  ${item.label}: ${path.relative(ROOT_DIR, item.file)}${sizeStr}`);
    } catch {
      // Not produced in this mode.
    }
  }
}

// ---------------------------------------------------------------------------
// Dev mode: one-command frontend + backend runner with joint cleanup
// ---------------------------------------------------------------------------

/**
 * Spawn a long-lived child process in its own process group / tracked tree,
 * prefixing stdout/stderr lines with `[tag]`.
 */
function spawnTrackedService(tag, cmd, args, { env, cwd = ROOT_DIR }) {
  const { command, shell } = resolveCommand(cmd);
  const child = spawn(command, args, {
    cwd,
    env,
    stdio: ["ignore", "pipe", "pipe"],
    detached: !IS_WINDOWS,
    shell,
    windowsHide: IS_WINDOWS,
  });

  activeChildren.add(child);

  const forward = (stream, writer) => {
    let buffer = "";
    stream.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        writer.write(`[${tag}] ${line}\n`);
      }
    });
    stream.on("end", () => {
      if (buffer.length > 0) {
        writer.write(`[${tag}] ${buffer}\n`);
      }
    });
  };

  if (child.stdout) forward(child.stdout, process.stdout);
  if (child.stderr) forward(child.stderr, process.stderr);

  child.on("close", () => {
    activeChildren.delete(child);
  });

  return child;
}

async function waitForHealth(url, shouldStop) {
  while (!shouldStop()) {
    try {
      const res = await fetch(url, { signal: AbortSignal.timeout(1500) });
      if (res.ok) return true;
    } catch {
      // Gateway still compiling or starting up.
    }
    await sleep(1000);
  }
  return false;
}

async function runDevMode(opts, env) {
  const stepOpts = { env, cwd: ROOT_DIR, timeoutMin: opts.timeoutMin, dryRun: opts.dryRun };

  if (!opts.skipInstall) {
    await runStep("Install JS dependencies", "pnpm", ["install", "--frozen-lockfile"], stepOpts);
  }

  if (opts.desktopDev) {
    await runStep(
      "Start Tauri desktop dev mode",
      "pnpm",
      ["--filter", "@any-converter/desktop", "tauri", "dev"],
      stepOpts
    );
    return;
  }

  const gatewayUrl = `http://127.0.0.1:${opts.port}`;
  const uiUrl = `http://127.0.0.1:${opts.uiPort}`;
  const cargoArgs = ["run"];
  if (opts.profile === "release") cargoArgs.push("--release");
  cargoArgs.push("-p", "any-converter", "--", "serve", "--port", String(opts.port));

  const viteArgs = ["--filter", "@any-converter/desktop", "dev"];
  const viteEnv = {
    ...env,
    ANY_CONVERTER_API_TARGET: gatewayUrl,
    ANY_CONVERTER_UI_PORT: String(opts.uiPort),
  };

  logInfo(`▶ Starting full-stack dev mode (Ctrl+C to stop both)`);
  logInfo(`  gateway: cargo ${cargoArgs.join(" ")}`);
  logInfo(`  vite:    pnpm ${viteArgs.join(" ")} (proxy /v1 & /health -> ${gatewayUrl})`);

  if (opts.dryRun) {
    return;
  }

  let stopped = false;
  const gateway = spawnTrackedService("gateway", "cargo", cargoArgs, { env });
  const vite = spawnTrackedService("vite", "pnpm", viteArgs, { env: viteEnv });

  // Background health probe to announce when both ends are ready.
  void waitForHealth(`${gatewayUrl}/health`, () => stopped || shuttingDown).then((ok) => {
    if (ok && !stopped && !shuttingDown) {
      logInfo(`✔ Gateway is healthy at ${gatewayUrl}`);
      logInfo(`✔ Open Vite HMR UI at ${uiUrl} (paste the page key printed by [gateway] if prompted)`);
    }
  });

  // If either process exits, tear down the other cleanly.
  await new Promise((resolve) => {
    const onExit = (name) => (code, signal) => {
      if (stopped || shuttingDown) {
        resolve();
        return;
      }
      stopped = true;
      const reason = signal ? `${name} exited (${signal})` : `${name} exited (code ${code ?? 0})`;
      const exitCode = code && code !== 0 ? code : 0;
      void reclaimAllProcesses(reason, exitCode).then(resolve);
    };

    gateway.on("close", onExit("gateway"));
    vite.on("close", onExit("vite"));
    gateway.on("error", (err) => {
      logError(`Failed to start gateway: ${err.message}`);
      onExit("gateway")(1, null);
    });
    vite.on("error", (err) => {
      logError(`Failed to start vite: ${err.message}`);
      onExit("vite")(1, null);
    });
  });
}

// ---------------------------------------------------------------------------
// Main build pipeline
// ---------------------------------------------------------------------------

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    printHelp();
    return;
  }

  const totalStart = Date.now();
  const plan = computeResourcePlan(opts);
  const env = buildStepEnv(plan, opts);

  logInfo(
    `Platform=${plan.platform} | CPUs=${plan.cpuCount} | RAM=${plan.freeMemGb}/${plan.totalMemGb} GiB`
  );
  logInfo(
    `Target=${opts.target} | Profile=${opts.profile} | CARGO_BUILD_JOBS=${plan.cargoJobs} | NODE_MEM=${plan.nodeMemMb}MiB | INCREMENTAL=${plan.incremental}`
  );
  if (opts.lowMemory) {
    logInfo("Low-memory mode enabled (reduced parallelism & release codegen-units=1)");
  }

  verifyToolchain(opts);

  if (opts.target === "dev") {
    await runDevMode(opts, env);
    return;
  }

  if (opts.clean) {
    await cleanBeforeBuild(opts.dryRun);
  }

  const stepOpts = { env, cwd: ROOT_DIR, timeoutMin: opts.timeoutMin, dryRun: opts.dryRun };

  // 1. Optional pre-build checks & tests
  if (opts.check) {
    await runStep("Rust fmt check", "cargo", ["fmt", "--all", "--", "--check"], stepOpts);
    await runStep(
      "Rust clippy",
      "cargo",
      ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
      stepOpts
    );
    if (!opts.skipInstall) {
      await runStep("Install JS dependencies", "pnpm", ["install", "--frozen-lockfile"], stepOpts);
      opts.skipInstall = true; // avoid running pnpm install twice
    }
    await runStep("TypeScript typecheck", "pnpm", ["typecheck"], stepOpts);
  }

  if (opts.test) {
    await runStep("Rust workspace tests", "cargo", ["test", "--workspace"], stepOpts);
  }

  // 2. Frontend SPA build (needed for ui, desktop, all, and cli unless --no-ui)
  const needsUi =
    opts.target === "ui" ||
    opts.target === "desktop" ||
    opts.target === "all" ||
    (opts.target === "cli" && !opts.noUi);

  if (needsUi) {
    if (!opts.skipInstall) {
      await runStep("Install JS dependencies", "pnpm", ["install", "--frozen-lockfile"], stepOpts);
    }
    await runStep(
      "Build Vite SPA (apps/desktop/dist)",
      "pnpm",
      ["--filter", "@any-converter/desktop", "build"],
      stepOpts
    );
  }

  // 3. Rust CLI / workspace builds
  const cargoProfileArgs = opts.profile === "release" ? ["--release"] : [];
  const cargoTargetArgs = opts.rustTarget ? ["--target", opts.rustTarget] : [];

  if (opts.target === "cli" || opts.target === "all") {
    await runStep(
      `Build CLI/gateway binary (${opts.profile})`,
      "cargo",
      ["build", ...cargoProfileArgs, ...cargoTargetArgs, "-p", "any-converter"],
      stepOpts
    );
  } else if (opts.target === "workspace") {
    await runStep(
      `Build Rust workspace (${opts.profile})`,
      "cargo",
      ["build", ...cargoProfileArgs, ...cargoTargetArgs, "--workspace"],
      stepOpts
    );
  }

  // 4. Desktop (Tauri) build
  if (opts.target === "desktop" || opts.target === "all") {
    if (!opts.skipNode) {
      await runStep("Fetch bundled Node sidecar", "node", ["scripts/fetch-node.mjs"], stepOpts);
    }
    const tauriArgs = ["--filter", "@any-converter/desktop", "tauri", "build"];
    if (opts.profile === "debug") {
      tauriArgs.push("--debug");
    }
    if (opts.rustTarget) {
      tauriArgs.push("--target", opts.rustTarget);
    }
    await runStep(`Build Tauri desktop app (${opts.profile})`, "pnpm", tauriArgs, stepOpts);
  }

  if (opts.cleanAfter) {
    await reclaimBuildArtifacts(opts);
  }

  await printArtifactSummary(opts);
  logInfo(`All done in ${formatDuration(Date.now() - totalStart)}.`);
}

main().catch((err) => {
  logError(err instanceof Error ? err.message : String(err));
  void reclaimAllProcesses("build failure", 1);
});

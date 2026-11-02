//! Integration tests for `any-converter plugin` (install/list) and the
//! `accounts add --plugin` argument surface.
//!
//! Real plugin authorize flows (a Node fixture) are covered by the server
//! e2e suite (P6.1/P6.3); these tests exercise the CLI-level parsing, the
//! empty-plugin-directory listing, and the npm failure reporting. The npm
//! test is allowed to hit the network; it skips (with a printed note) when
//! npm is unavailable or the run times out.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::process::Stdio;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn binary() -> Command {
    Command::cargo_bin("any-converter").unwrap()
}

/// Temp plugins directory wired into the child via `ANY_CONVERTER_PLUGINS`.
struct PluginsEnv {
    #[allow(dead_code)] // keeps the temp dir alive for the test body
    dir: TempDir,
    plugins_path: std::path::PathBuf,
}

impl PluginsEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let plugins_path = dir.path().join("plugins");
        std::fs::create_dir(&plugins_path).unwrap();
        Self { dir, plugins_path }
    }

    /// A `Command` with `ANY_CONVERTER_PLUGINS` pointing at the temp dir.
    fn command(&self) -> Command {
        let mut cmd = binary();
        cmd.env("ANY_CONVERTER_PLUGINS", &self.plugins_path);
        cmd
    }
}

/// Whether a usable `npm` is on PATH (checked to skip the network test).
fn npm_available() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

// ── plugin list ─────────────────────────────────────────────────────────

#[test]
fn plugin_list_empty() {
    let env = PluginsEnv::new();

    env.command()
        .arg("plugin")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No plugins installed"));
}

// ── plugin install ──────────────────────────────────────────────────────

#[test]
fn plugin_install_unknown_package_fails_with_manual_hint() {
    if !npm_available() {
        eprintln!("skipping: npm is not available on PATH");
        return;
    }
    let env = PluginsEnv::new();

    // Allowed to hit the network (npm answers 404 for a made-up package);
    // when the run hangs past the timeout the child is killed and the test
    // skips instead of failing.
    let output = env
        .command()
        .arg("plugin")
        .arg("install")
        .arg("this-package-does-not-exist-xyz")
        .timeout(Duration::from_secs(180))
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => {
            eprintln!("skipping: could not run the command: {err}");
            return;
        }
    };
    if output.status.code().is_none() {
        eprintln!("skipping: the install was killed (timed out) in this environment");
        return;
    }
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("npm install"),
        "expected the manual npm hint, got stderr: {stderr}"
    );
}

// ── accounts add ────────────────────────────────────────────────────────

#[test]
fn accounts_add_plugin_without_spec_fails() {
    let env = PluginsEnv::new();

    // `--plugin ""` (and no provider) must not slip through to the sign-in
    // paths.
    env.command()
        .arg("accounts")
        .arg("add")
        .arg("--plugin")
        .arg("")
        .assert()
        .failure();
}

#[test]
fn accounts_add_builtin_still_works() {
    let env = PluginsEnv::new();

    // Unknown built-in providers still fail, and the message points at
    // `--plugin` for plugin-provided providers.
    env.command()
        .arg("accounts")
        .arg("add")
        .arg("bogus")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"))
        .stderr(predicate::str::contains("--plugin"));
}

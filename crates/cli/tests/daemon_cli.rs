//! CLI integration tests for `any-converter daemon …`.
//!
//! Every test points the child at a temp directory via `--home` (and sets
//! `HOME`/`XDG_CONFIG_HOME` there too), so no test ever reads — let alone
//! writes — the developer's real LaunchAgents or systemd user dirs, and
//! no real `launchctl`/`systemctl` ever runs.

#![allow(dead_code, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn binary() -> Command {
    Command::cargo_bin("any-converter").unwrap()
}

/// A command whose `HOME` and `XDG_CONFIG_HOME` are the temp dir, so the
/// child's service-manager discovery stays inside the test.
fn daemon_in(dir: &TempDir, args: &[&str]) -> Command {
    let mut command = binary();
    command
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path());
    command.args(args);
    command
}

#[test]
fn daemon_help_lists_the_subcommands() {
    daemon_in(&tempfile::tempdir().unwrap(), &["daemon", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("uninstall"))
        .stdout(predicate::str::contains("start"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("restart"))
        .stdout(predicate::str::contains("status"));
}

#[test]
fn status_on_a_clean_home_says_not_installed_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_string_lossy().into_owned();
    daemon_in(&dir, &["daemon", "status", "--home", &home])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed: no"));
}

#[test]
fn install_dry_run_prints_the_service_file_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_string_lossy().into_owned();
    daemon_in(&dir, &["daemon", "install", "--dry-run", "--home", &home])
        .assert()
        .success()
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("--home"))
        .stdout(predicate::str::contains(home.as_str()))
        .stdout(predicate::str::contains(
            "dry run: no files written, no commands run",
        ))
        .stdout(predicate::str::contains("sudo").not());

    // The dry run must not create the home, the key, or any directory.
    assert!(!dir.path().join("web-key").exists());
    assert!(!dir.path().join("config.toml").exists());
    assert!(!dir.path().join("Library").exists());
    assert!(!dir.path().join("config").exists());
}

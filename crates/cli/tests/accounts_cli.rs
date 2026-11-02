//! Integration tests for `any-converter accounts` (list/remove/validation).
//!
//! The OAuth paths (`accounts add gemini/copilot`) hit real network flows by
//! design and are therefore NOT exercised here; only the store-backed
//! surface is covered, with `ANY_CONVERTER_SECRETS` pointed at a temp file.

#![allow(clippy::unwrap_used)]

use std::io::Write;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn binary() -> Command {
    Command::cargo_bin("any-converter").unwrap()
}

/// Temp dir plus a secrets-file path wired into the child via env.
struct SecretsEnv {
    #[allow(dead_code)] // keeps the temp dir alive for the test body
    dir: TempDir,
    secrets_path: std::path::PathBuf,
}

impl SecretsEnv {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        Self { dir, secrets_path }
    }

    /// A `Command` with `ANY_CONVERTER_SECRETS` pointing at the temp file.
    fn command(&self) -> Command {
        let mut cmd = binary();
        cmd.env("ANY_CONVERTER_SECRETS", &self.secrets_path);
        cmd
    }

    /// Seed the store with one OAuth credential carrying a real-looking
    /// token, so tests can assert it never reaches stdout.
    fn seed_oauth(&self) {
        let payload = r#"{
  "copilot:wdjb****": {
    "kind": "oauth",
    "provider": "copilot",
    "account": "wdjb****",
    "access_token": "gho_secretDoNotPrint_12345",
    "refresh_token": null,
    "expires_at_unix": null
  }
}"#;
        let mut file = std::fs::File::create(&self.secrets_path).unwrap();
        write!(file, "{payload}").unwrap();
    }
}

// ── list ───────────────────────────────────────────────────────────────

#[test]
fn accounts_list_empty_store() {
    let env = SecretsEnv::new();

    env.command()
        .arg("accounts")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No accounts"));
}

#[test]
fn accounts_list_shows_ids_and_kind_without_tokens() {
    let env = SecretsEnv::new();
    env.seed_oauth();

    env.command()
        .arg("accounts")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("copilot:wdjb****"))
        .stdout(predicate::str::contains("[oauth]"))
        // The stored access token must never reach stdout (or stderr).
        .stdout(predicate::str::contains("gho_secretDoNotPrint_12345").not())
        .stderr(predicate::str::contains("gho_secretDoNotPrint_12345").not());
}

// ── remove ─────────────────────────────────────────────────────────────

#[test]
fn accounts_remove_nonexistent_fails() {
    let env = SecretsEnv::new();

    env.command()
        .arg("accounts")
        .arg("remove")
        .arg("nonexistent:account")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no such account"));
}

#[test]
fn accounts_remove_existing_succeeds_and_list_is_empty_again() {
    let env = SecretsEnv::new();
    env.seed_oauth();

    env.command()
        .arg("accounts")
        .arg("remove")
        .arg("copilot:wdjb****")
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed"));

    env.command()
        .arg("accounts")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No accounts"));
}

// ── add validation (no network) ────────────────────────────────────────

#[test]
fn accounts_add_unknown_provider_fails() {
    let env = SecretsEnv::new();

    env.command()
        .arg("accounts")
        .arg("add")
        .arg("bogus")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn accounts_add_known_providers_are_accepted_by_the_parser() {
    // Parsing only: `--help` proves both providers are valid values without
    // starting any network flow.
    let env = SecretsEnv::new();

    env.command()
        .arg("accounts")
        .arg("add")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("gemini"))
        .stdout(predicate::str::contains("copilot"));
}

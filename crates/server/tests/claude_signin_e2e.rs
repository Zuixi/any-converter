//! End-to-end tests for the Claude sign-in (P15), which delegates to
//! Claude Code's own `claude auth login` (magpie `claude_signin.go`).
//!
//! Every test runs the fake CLI fixture
//! (`tests/fixtures/fake-claude-cli.sh`) with an injected config root and
//! mode, so the real `claude` CLI is never executed — nothing here needs
//! Claude Code installed:
//!
//! 1. the browser route takes the link from the shim (`.opened-url`);
//! 2. the print route falls back to the link the CLI printed;
//! 3. a submitted `code#state` reaches the CLI, whose signed-in files the
//!    wait collects into the account;
//! 4. a submitted URL is refused unless its port and code are this
//!    sign-in's;
//! 5. a machine without the `claude` CLI gets the install hint;
//! 6. a CLI failure reports its last printed line;
//! 7. the temp config dir is removed after a failed sign-in;
//! 8. the account id is the Claude email.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use any_converter_server::credentials::claude_signin::{
    CLAUDE_INSTALL_HINT, ClaudeAccount, ClaudeSignIn, find_claude_cli,
};

/// The fake `claude` CLI fixture (also plays the browser shim).
fn fixture() -> PathBuf {
    let extension = if cfg!(windows) { "cmd" } else { "sh" };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("fake-claude-cli.{extension}"))
}

/// A sign-in running the fake CLI in `root` with the given mode.
fn test_sign_in(root: &std::path::Path, mode: &str) -> ClaudeSignIn {
    ClaudeSignIn::with_program(fixture(), root.to_path_buf()).with_env("FAKE_CLAUDE", mode)
}

/// The page URL the fake CLI opens/prints for callback port `port`.
fn fake_url(port: u16) -> String {
    format!(
        "https://claude.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcallback&code=abc&state=s1"
    )
}

/// Every `claude-signin-*` dir currently under `root`.
fn sign_in_dirs(root: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("claude-signin-")
        })
        .collect()
}

#[tokio::test]
async fn browser_route_gets_the_link_from_the_shim() {
    let root = tempfile::tempdir().unwrap();
    let sign_in = test_sign_in(root.path(), "browser");

    let started = sign_in.start().await.unwrap();
    assert_eq!(started.url, fake_url(34567));
    assert!(!started.paste_code);

    // The shim (the fake CLI playing BROWSER) wrote the page into the
    // sign-in dir's .opened-url.
    let dirs = sign_in_dirs(root.path());
    assert_eq!(dirs.len(), 1);
    let opened = std::fs::read_to_string(dirs[0].join(".opened-url")).unwrap();
    assert_eq!(opened, fake_url(34567));
}

#[tokio::test]
async fn print_route_falls_back_to_the_printed_link() {
    let root = tempfile::tempdir().unwrap();
    let sign_in = test_sign_in(root.path(), "print");

    let started = sign_in.start().await.unwrap();
    assert_eq!(started.url, fake_url(34567));
    assert!(started.paste_code);

    // No browser call happened: .opened-url was never written.
    let dirs = sign_in_dirs(root.path());
    assert_eq!(dirs.len(), 1);
    assert!(!dirs[0].join(".opened-url").exists());
}

#[tokio::test]
async fn submit_a_code_reaches_the_cli() {
    let root = tempfile::tempdir().unwrap();
    let sign_in = test_sign_in(root.path(), "browser");
    let started = sign_in.start().await.unwrap();
    assert!(!started.paste_code);

    sign_in.submit("abc#s1").await.unwrap();

    let account = sign_in.wait(Duration::from_secs(15)).await.unwrap();
    assert_eq!(account.user, "me@example.com");
    assert_eq!(account.plan, "pro");
    assert_eq!(account.org, "Acme");
    assert_eq!(account.expires_at_unix, Some(2_000_000_000));

    // The account collected, the sign-in dir went away with the flow.
    assert!(sign_in_dirs(root.path()).is_empty());
}

#[tokio::test]
async fn submit_a_url_is_rejected_when_the_state_or_port_is_wrong() {
    let root = tempfile::tempdir().unwrap();
    // A port where connections are accepted and closed: the callback GET
    // fails deterministically when it is attempted.
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = blocker.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            if let Ok((socket, _)) = blocker.accept().await {
                drop(socket);
            }
        }
    });

    let sign_in = ClaudeSignIn::with_program(fixture(), root.path().to_path_buf())
        .with_env("FAKE_CLAUDE", "browser")
        .with_env("FAKE_CLAUDE_PORT", port.to_string());
    let started = sign_in.start().await.unwrap();
    assert_eq!(started.url, fake_url(port));

    // The wrong port is refused before any request.
    let err = sign_in
        .submit("http://localhost:1/callback?code=x&state=s1")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("isn't from this sign-in"),
        "err: {err}"
    );

    // A state without a code is refused the same way.
    let err = sign_in
        .submit(&format!("http://localhost:{port}/callback?state=s1"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("isn't from this sign-in"),
        "err: {err}"
    );

    // The right port and code attempt the callback and report the CLI
    // didn't take it (nothing answers there).
    let err = sign_in
        .submit(&format!("http://localhost:{port}/callback?code=x&state=s1"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("didn't take that address"),
        "err: {err}"
    );
}

#[test]
fn missing_cli_says_to_install_claude_code() {
    let env = |_key: &str| None;
    assert!(find_claude_cli(env).is_none());
    let hint = CLAUDE_INSTALL_HINT.to_ascii_lowercase();
    assert!(hint.contains("claude"), "hint: {hint}");
    assert!(hint.contains("install"), "hint: {hint}");
}

#[tokio::test]
async fn cli_failure_reports_its_last_line() {
    let root = tempfile::tempdir().unwrap();
    let sign_in = test_sign_in(root.path(), "fail");

    // The CLI printed one line and died before handing out a link: start
    // reports it...
    let err = sign_in.start().await.unwrap_err();
    assert!(err.to_string().contains("claude exploded"), "err: {err}");

    // ...and wait, called after the flow already failed, reports it again.
    let err = sign_in.wait(Duration::from_secs(5)).await.unwrap_err();
    assert!(err.to_string().contains("claude exploded"), "err: {err}");
}

#[tokio::test]
async fn temp_dir_is_removed_after_a_failed_sign_in() {
    let root = tempfile::tempdir().unwrap();
    let sign_in = test_sign_in(root.path(), "fail");

    let err = sign_in.start().await.unwrap_err();
    assert!(err.to_string().contains("claude exploded"), "err: {err}");
    assert!(
        sign_in_dirs(root.path()).is_empty(),
        "sign-in dirs left behind"
    );
}

#[test]
fn account_id_is_the_claude_email() {
    let account = ClaudeAccount {
        user: "me@example.com".to_string(),
        plan: "pro".to_string(),
        org: "Acme".to_string(),
        access_token: "tok-access".to_string(),
        refresh_token: "tok-refresh".to_string(),
        expires_at_unix: None,
    };
    assert_eq!(account.account_id_key(), "claude:me@example.com");
}

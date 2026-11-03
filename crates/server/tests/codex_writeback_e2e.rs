//! End-to-end tests for the Codex token write-back (P11b): rotated tokens
//! written back to the `auth.json` a Codex sign-in lives in, with magpie's
//! three protections — never overwrite what could not be read, last-good
//! `.bak` backup plus temp-and-rename atomic commit, and a cross-process
//! `FileLock` so two processes never refresh (and write back) one sign-in
//! concurrently.
//!
//! Every test runs against a temp HOME/CODEX_HOME; no real Codex file is
//! ever touched.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use any_converter_server::credentials::{
    AccountResolver, CodexStore, Credential, FileSecretStore, SecretStore, TokenUpdate,
    WriteBackError, codex_store, mark_adopted, write_tokens, write_tokens_with_timeout,
};
use any_converter_server::lockfile::FileLock;
use axum::Json;
use serde_json::{Value, json};

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A temp directory holding one CODEX_HOME; removed on drop.
struct TempCodexHome {
    _dir: tempfile::TempDir,
    home: PathBuf,
}

impl TempCodexHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".codex");
        fs::create_dir_all(&home).unwrap();
        Self { _dir: dir, home }
    }

    fn auth_path(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    fn write_auth(&self, auth: &Value) {
        fs::write(self.auth_path(), serde_json::to_vec_pretty(auth).unwrap()).unwrap();
    }

    fn read_auth(&self) -> Value {
        serde_json::from_slice(&fs::read(self.auth_path()).unwrap()).unwrap()
    }

    fn read_auth_bytes(&self) -> Vec<u8> {
        fs::read(self.auth_path()).unwrap()
    }

    fn lock_path(&self) -> PathBuf {
        let mut path = self.auth_path().into_os_string();
        path.push(".any-converter.lock");
        PathBuf::from(path)
    }
}

/// The auth.json shape a real Codex sign-in has, with extra fields the
/// write-back must keep untouched.
fn codex_auth() -> Value {
    json!({
        "auth_mode": "tokens",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": "id-old",
            "access_token": "at-old",
            "refresh_token": "rt-old",
            "account_id": "acct-1"
        },
        "custom_extra": { "kept": [1, 2, 3] },
        "last_refresh": "2025-01-01T00:00:00Z"
    })
}

fn token_update() -> TokenUpdate {
    TokenUpdate {
        access_token: "at-new".to_string(),
        refresh_token: "rt-new".to_string(),
        id_token: Some("id-new".to_string()),
    }
}

// ── D1-D6: the write path ────────────────────────────────────────────────

/// D1: every field the write-back does not own survives byte-for-byte
/// (structurally); only the three token fields change (and the stamp the
/// write owns, `last_refresh`).
#[test]
fn writeback_keeps_every_other_field() {
    let home = TempCodexHome::new();
    home.write_auth(&codex_auth());

    write_tokens(&home.home, &token_update()).unwrap();

    let auth = home.read_auth();
    assert_eq!(auth["auth_mode"], "tokens");
    assert!(auth["OPENAI_API_KEY"].is_null());
    assert_eq!(auth["tokens"]["account_id"], "acct-1");
    assert_eq!(auth["custom_extra"], json!({ "kept": [1, 2, 3] }));
    assert_eq!(auth["tokens"]["access_token"], "at-new");
    assert_eq!(auth["tokens"]["refresh_token"], "rt-new");
    assert_eq!(auth["tokens"]["id_token"], "id-new");
    // The write's own stamp moves to now.
    let stamped = auth["last_refresh"].as_str().unwrap();
    assert_ne!(stamped, "2025-01-01T00:00:00Z");
}

/// An empty `id_token` update keeps the file's value (only non-empty new
/// values are written).
#[test]
fn writeback_keeps_the_file_id_token_when_the_update_has_none() {
    let home = TempCodexHome::new();
    home.write_auth(&codex_auth());

    let update = TokenUpdate {
        id_token: None,
        ..token_update()
    };
    write_tokens(&home.home, &update).unwrap();

    let auth = home.read_auth();
    assert_eq!(auth["tokens"]["id_token"], "id-old");
    assert_eq!(auth["tokens"]["access_token"], "at-new");
}

/// D2: a file that exists but cannot be read is refused, and its bytes are
/// never touched — a bad file is never treated as an empty one.
#[test]
fn writeback_refuses_when_auth_json_is_unreadable() {
    let home = TempCodexHome::new();
    fs::write(home.auth_path(), b"{ definitely not json").unwrap();
    let before = home.read_auth_bytes();

    let error = write_tokens(&home.home, &token_update()).unwrap_err();
    assert!(matches!(error, WriteBackError::Refused(_)));
    assert!(
        matches!(&error, WriteBackError::Refused(msg) if msg.contains("can't be read")),
        "error: {error}"
    );
    assert_eq!(home.read_auth_bytes(), before, "bytes must be untouched");
}

/// D3: a keyring store refuses with guidance, and no auth.json is created.
#[test]
fn writeback_refuses_when_the_store_is_keyring() {
    let home = TempCodexHome::new();
    fs::write(
        home.home.join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .unwrap();

    let error = write_tokens(&home.home, &token_update()).unwrap_err();
    let WriteBackError::Refused(msg) = &error else {
        panic!("keyring store must refuse: {error:?}");
    };
    assert!(
        msg.contains("keyring"),
        "guidance must name the store: {msg}"
    );
    assert!(
        msg.contains("cli_auth_credentials_store = \"file\""),
        "guidance must say how to fix it: {msg}"
    );
    assert!(!home.auth_path().exists(), "auth.json must not be created");
}

/// `auto` and `ephemeral` stores refuse the same way.
#[test]
fn writeback_refuses_when_the_store_is_auto_or_ephemeral() {
    for store in ["auto", "ephemeral"] {
        let home = TempCodexHome::new();
        fs::write(
            home.home.join("config.toml"),
            format!("cli_auth_credentials_store = \"{store}\"\n"),
        )
        .unwrap();
        let error = write_tokens(&home.home, &token_update()).unwrap_err();
        assert!(matches!(error, WriteBackError::Refused(_)));
        assert!(!home.auth_path().exists());
    }
}

/// An unparsable config.toml refuses the write too (never a guess).
#[test]
fn writeback_refuses_when_config_toml_cannot_be_parsed() {
    let home = TempCodexHome::new();
    fs::write(home.home.join("config.toml"), "= not toml").unwrap();
    assert!(matches!(codex_store(&home.home), CodexStore::Refused(_)));
}

/// D4: no auth.json means no sign-in to update — refused, and the file is
/// never created.
#[test]
fn writeback_refuses_when_there_is_no_auth_json() {
    let home = TempCodexHome::new();

    let error = write_tokens(&home.home, &token_update()).unwrap_err();
    let WriteBackError::Refused(msg) = &error else {
        panic!("missing auth.json must refuse: {error:?}");
    };
    assert!(msg.contains("no sign-in to update"), "message: {msg}");
    assert!(!home.auth_path().exists(), "auth.json must not be created");
}

/// D5: a successful write-back leaves the pre-write bytes in
/// `auth.json.bak`.
#[test]
fn writeback_makes_a_bak() {
    let home = TempCodexHome::new();
    home.write_auth(&codex_auth());
    let before = home.read_auth_bytes();

    write_tokens(&home.home, &token_update()).unwrap();

    let mut bak_path = home.auth_path().into_os_string();
    bak_path.push(".bak");
    let bak = PathBuf::from(bak_path);
    assert!(bak.is_file(), "auth.json.bak must exist");
    assert_eq!(fs::read(&bak).unwrap(), before, ".bak holds the old bytes");
    assert_ne!(home.read_auth_bytes(), before, "the file itself moved on");
}

/// D6a: a fresh lock held by someone else makes the write-back give up
/// within its timeout without touching the file.
#[test]
fn writeback_is_atomic_under_the_lock() {
    let home = TempCodexHome::new();
    home.write_auth(&codex_auth());
    let before = home.read_auth_bytes();

    // A lock file fresh enough to belong to a live holder.
    fs::write(home.lock_path(), b"pid=99999\n").unwrap();

    let error = write_tokens_with_timeout(&home.home, &token_update(), Duration::from_millis(250))
        .unwrap_err();
    assert!(matches!(error, WriteBackError::Io(_)), "error: {error}");
    assert_eq!(
        home.read_auth_bytes(),
        before,
        "a refused lock must not write anything"
    );
}

/// D6b: while another thread holds the lock, the write-back blocks — and
/// completes once the holder lets go.
#[test]
fn writeback_waits_for_a_held_lock() {
    let home = TempCodexHome::new();
    home.write_auth(&codex_auth());

    let guard = FileLock::new(home.lock_path())
        .acquire_timeout(Duration::from_secs(5))
        .unwrap();

    let entered = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let thread_home = home.home.clone();
    let thread_entered = Arc::clone(&entered);
    let thread_finished = Arc::clone(&finished);
    let writer = std::thread::spawn(move || {
        thread_entered.store(true, Ordering::SeqCst);
        let result = write_tokens(&thread_home, &token_update());
        thread_finished.store(true, Ordering::SeqCst);
        result
    });

    // Give the writer time to reach the lock; it must still be waiting.
    while !entered.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !finished.load(Ordering::SeqCst),
        "the writer must block while the lock is held"
    );
    assert_eq!(
        home.read_auth()["tokens"]["access_token"],
        "at-old",
        "nothing may be written while the lock is held"
    );

    drop(guard);
    writer.join().unwrap().unwrap();
    assert_eq!(home.read_auth()["tokens"]["access_token"], "at-new");
}

// ── D7-D8: the resolver's refresh path ────────────────────────────────────

/// A fake ChatGPT token endpoint that answers one refresh.
async fn spawn_fake_token_endpoint() -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = Arc::clone(&calls);
    let router = axum::Router::new().route(
        "/token",
        axum::routing::post(move |body: Json<Value>| {
            let state = Arc::clone(&state);
            async move {
                state.fetch_add(1, Ordering::SeqCst);
                assert_eq!(body["grant_type"], "refresh_token");
                assert_eq!(body["refresh_token"], "rt-old");
                Json(json!({
                    "access_token": "at-new",
                    "refresh_token": "rt-new",
                    "id_token": "id-new",
                    "expires_in": 3600
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), calls)
}

/// A stored, expired codex credential plus the codex home it came from.
fn setup_codex_account(dir: &Path, auth: &Value) -> (PathBuf, PathBuf, FileSecretStore) {
    let codex_home = dir.join(".codex");
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(
        codex_home.join("auth.json"),
        serde_json::to_vec_pretty(auth).unwrap(),
    )
    .unwrap();

    let secrets_path = dir.join("secrets.json");
    let store = FileSecretStore::new(secrets_path.clone());
    store
        .put(
            "codex:me@example.com",
            &Credential::OAuth {
                provider: "codex".into(),
                account: "me@example.com".into(),
                access_token: "at-old".into(),
                refresh_token: Some("rt-old".into()),
                expires_at_unix: Some(unix_now() - 60),
                siwc: None,
            },
        )
        .unwrap();
    (codex_home, secrets_path, store)
}

/// D7: refreshing an adopted codex account writes the rotated tokens back
/// to its auth.json (and the secret store moves on too).
#[tokio::test]
async fn adopted_account_refreshes_and_writes_back() {
    let dir = tempfile::tempdir().unwrap();
    let (codex_home, secrets_path, store) = setup_codex_account(dir.path(), &codex_auth());
    mark_adopted(&secrets_path, "codex:me@example.com", &codex_home).unwrap();

    let (base, calls) = spawn_fake_token_endpoint().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let accounts = AccountResolver::with_token_url(store, client, format!("{base}/token"));

    let resolved = accounts.resolve("codex:me@example.com").await.unwrap();
    assert_eq!(resolved.access_token, "at-new");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The secret store holds the rotated tokens.
    let check = FileSecretStore::new(secrets_path.clone());
    let stored = check.get("codex:me@example.com").unwrap().unwrap();
    let Credential::OAuth {
        access_token,
        refresh_token,
        ..
    } = stored
    else {
        panic!("codex credential must be OAuth");
    };
    assert_eq!(access_token, "at-new");
    assert_eq!(refresh_token.as_deref(), Some("rt-new"));

    // Codex's auth.json was written back where Codex reads it.
    let auth: Value =
        serde_json::from_slice(&fs::read(codex_home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(auth["tokens"]["access_token"], "at-new");
    assert_eq!(auth["tokens"]["refresh_token"], "rt-new");
    assert_eq!(auth["tokens"]["id_token"], "id-new");
    assert_eq!(auth["tokens"]["account_id"], "acct-1", "other fields kept");
    assert_eq!(auth["auth_mode"], "tokens");
}

/// D8: a refresh that is not adopted never touches Codex's auth.json, even
/// though the secret store moves on.
#[tokio::test]
async fn non_adopted_account_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (codex_home, _secrets_path, store) = setup_codex_account(dir.path(), &codex_auth());
    let auth_path = codex_home.join("auth.json");
    let before = fs::read(&auth_path).unwrap();

    let (base, calls) = spawn_fake_token_endpoint().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let accounts = AccountResolver::with_token_url(store, client, format!("{base}/token"));

    let resolved = accounts.resolve("codex:me@example.com").await.unwrap();
    assert_eq!(resolved.access_token, "at-new");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(&auth_path).unwrap(),
        before,
        "auth.json must be byte-identical for a non-adopted account"
    );
}

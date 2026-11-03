//! End-to-end tests for `accounts import` (P12): accounts from cswap /
//! Sub2API / CLIProxyAPI export files imported into a file secret store,
//! with magpie's newer-wins rule for claude-swap exports.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use any_converter_server::credentials::{
    Credential, FileSecretStore, ImportStatus, SecretStore, import_path,
};

/// `tests/fixtures/import/` under the server crate.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/import")
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// A store (and the temp dir that keeps it alive) per test.
fn temp_store() -> (FileSecretStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path().join("secrets.json"));
    (store, dir)
}

/// The stored OAuth credential under `id`, or `None`.
fn stored(store: &FileSecretStore, id: &str) -> Option<Credential> {
    store.get(id).unwrap()
}

/// Write a cswap file for `cswap@example.com` with the given access token
/// and (optional) `expiresAt` in milliseconds.
fn temp_cswap(dir: &Path, access: &str, expires_ms: Option<i64>) -> PathBuf {
    let path = dir.join(format!("cswap-{access}.json"));
    let oauth = match expires_ms {
        Some(ms) => format!(
            r#"{{"accessToken": "{access}", "refreshToken": "test-token-3-claude-refresh", "expiresAt": {ms}}}"#
        ),
        None => format!(
            r#"{{"accessToken": "{access}", "refreshToken": "test-token-3-claude-refresh"}}"#
        ),
    };
    let json = format!(
        r#"{{
  "format": 1,
  "accounts": [
    {{
      "credentials": {{ "claudeAiOauth": {oauth} }},
      "config": {{ "oauthAccount": {{ "emailAddress": "cswap@example.com", "organizationName": "Work Org" }} }},
      "alias": "work"
    }}
  ]
}}"#
    );
    std::fs::write(&path, json).unwrap();
    path
}

/// The access token stored under `id`.
fn stored_access(store: &FileSecretStore, id: &str) -> String {
    match stored(store, id).expect("credential must be stored") {
        Credential::OAuth { access_token, .. } => access_token,
        other => panic!("expected OAuth credential, got {other:?}"),
    }
}

#[test]
fn sub2api_claude_and_codex_are_imported() {
    let (store, _dir) = temp_store();
    let path = fixture("sub2api.json");
    let results = import_path(&store, &path);

    assert_eq!(results.len(), 2, "{results:?}");
    for result in &results {
        assert_eq!(result.status, ImportStatus::Added, "{result:?}");
        assert_eq!(result.file, path.display().to_string());
    }

    let claude = &results[0];
    assert_eq!(claude.account, "claude:claude@example.com");
    assert_eq!(claude.provider, "claude");
    assert_eq!(claude.email, "claude@example.com");
    assert_eq!(
        stored(&store, "claude:claude@example.com"),
        Some(Credential::OAuth {
            provider: "claude".into(),
            account: "claude@example.com".into(),
            access_token: "test-token-1-claude-access".into(),
            refresh_token: Some("test-token-1-claude-refresh".into()),
            expires_at_unix: Some(1_900_000_000),
            siwc: None,
        })
    );

    let codex = &results[1];
    assert_eq!(codex.account, "codex:codex@example.com");
    assert_eq!(codex.provider, "codex");
    assert_eq!(codex.email, "codex@example.com");
    assert_eq!(
        stored(&store, "codex:codex@example.com"),
        Some(Credential::OAuth {
            provider: "codex".into(),
            account: "codex@example.com".into(),
            access_token: "test-token-1-codex-access".into(),
            refresh_token: Some("test-token-1-codex-refresh".into()),
            expires_at_unix: None,
            siwc: None,
        })
    );
}

#[test]
fn cliproxy_map_is_imported() {
    let (store, _dir) = temp_store();
    let path = fixture("cliproxy.json");
    let results = import_path(&store, &path);

    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].status, ImportStatus::Added, "{results:?}");
    assert_eq!(results[0].account, "claude:map@example.com");
    assert_eq!(results[0].provider, "claude");
    assert_eq!(results[0].email, "map@example.com");
    assert_eq!(
        stored(&store, "claude:map@example.com"),
        Some(Credential::OAuth {
            provider: "claude".into(),
            account: "map@example.com".into(),
            access_token: "test-token-2-claude-access".into(),
            refresh_token: Some("test-token-2-claude-refresh".into()),
            expires_at_unix: Some(1_890_000_000),
            siwc: None,
        })
    );
}

#[test]
fn cswap_newer_wins() {
    let (store, dir) = temp_store();
    let results = import_path(&store, &fixture("cswap.json"));
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].status, ImportStatus::Added, "{results:?}");
    assert_eq!(results[0].account, "claude:cswap@example.com");
    assert_eq!(
        stored_access(&store, "claude:cswap@example.com"),
        "test-token-3-claude-access"
    );

    // A later expiresAt (1_890_000_000 > 1_880_000_000) replaces it.
    let newer = temp_cswap(
        dir.path(),
        "test-token-4-claude-access-newer",
        Some(1_890_000_000_000),
    );
    let results = import_path(&store, &newer);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].status, ImportStatus::Added, "{results:?}");
    assert_eq!(
        stored(&store, "claude:cswap@example.com"),
        Some(Credential::OAuth {
            provider: "claude".into(),
            account: "cswap@example.com".into(),
            access_token: "test-token-4-claude-access-newer".into(),
            refresh_token: Some("test-token-3-claude-refresh".into()),
            expires_at_unix: Some(1_890_000_000),
            siwc: None,
        })
    );
}

#[test]
fn cswap_older_does_not_overwrite() {
    let (store, dir) = temp_store();
    import_path(&store, &fixture("cswap.json"));

    // An earlier expiresAt (1_870_000_000 < 1_880_000_000) is not newer.
    let older = temp_cswap(
        dir.path(),
        "test-token-4-claude-access-older",
        Some(1_870_000_000_000),
    );
    let results = import_path(&store, &older);
    assert_eq!(results.len(), 1, "{results:?}");
    match &results[0].status {
        ImportStatus::Exists(why) => {
            assert!(why.contains("not newer"), "why: {why}");
        }
        other => panic!("expected Exists, got {other:?}"),
    }
    assert_eq!(results[0].account, "claude:cswap@example.com");
    // The stored sign-in is the old one still.
    assert_eq!(
        stored_access(&store, "claude:cswap@example.com"),
        "test-token-3-claude-access"
    );
}

#[test]
fn cswap_without_expiry_never_overwrites() {
    let (store, dir) = temp_store();
    import_path(&store, &fixture("cswap.json"));

    let no_expiry = temp_cswap(dir.path(), "test-token-4-claude-access-noexpiry", None);
    let results = import_path(&store, &no_expiry);
    assert_eq!(results.len(), 1, "{results:?}");
    match &results[0].status {
        ImportStatus::Exists(why) => {
            assert!(why.contains("no expiresAt"), "why: {why}");
        }
        other => panic!("expected Exists, got {other:?}"),
    }
    assert_eq!(
        stored_access(&store, "claude:cswap@example.com"),
        "test-token-3-claude-access"
    );
}

#[test]
fn encrypted_export_is_refused_with_guidance() {
    let (store, _dir) = temp_store();
    let path = fixture("cswap_encrypted.json");
    let results = import_path(&store, &path);

    assert_eq!(results.len(), 1, "{results:?}");
    match &results[0].status {
        ImportStatus::Refused(why) => {
            assert!(why.contains("decrypt"), "why: {why}");
            assert!(why.contains("without encryption"), "why: {why}");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(results[0].file, path.display().to_string());
    assert!(store.list().unwrap().is_empty(), "nothing may be written");
}

#[test]
fn unknown_shape_is_refused() {
    let (store, dir) = temp_store();
    let path = dir.path().join("random.json");
    std::fs::write(&path, r#"{"hello": "world", "numbers": [1, 2, 3]}"#).unwrap();
    let results = import_path(&store, &path);

    assert_eq!(results.len(), 1, "{results:?}");
    match &results[0].status {
        ImportStatus::Refused(why) => {
            assert!(why.contains("claude-swap"), "why: {why}");
            assert!(why.contains("Sub2API"), "why: {why}");
            assert!(why.contains("CLIProxyAPI"), "why: {why}");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(results[0].account, "");
    assert_eq!(results[0].file, path.display().to_string());
    assert!(store.list().unwrap().is_empty(), "nothing may be written");
}

#[test]
fn imported_file_is_only_read() {
    let (store, _dir) = temp_store();
    for name in [
        "sub2api.json",
        "cliproxy.json",
        "cswap.json",
        "cswap_encrypted.json",
    ] {
        let path = fixture(name);
        let before = std::fs::read(&path).unwrap();
        let _ = import_path(&store, &path);
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "import must not modify {name}");
    }
}

#[test]
fn directory_import_is_sorted_and_isolated() {
    let (store, dir) = temp_store();
    let files = dir.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("a-broken.json"), "{ not json").unwrap();
    std::fs::write(
        files.join("b-good.json"),
        r#"{"flat@example.com": {"type": "codex", "access_token": "test-token-9-codex-access", "refresh_token": "test-token-9-codex-refresh"}}"#,
    )
    .unwrap();
    std::fs::write(files.join("c-ignored.txt"), "not json").unwrap();

    let results = import_path(&store, &files);
    assert_eq!(results.len(), 2, "{results:?}");

    // Sorted by file name: the broken file's refusal comes first and does
    // not stop the good file from being imported.
    assert!(results[0].file.ends_with("a-broken.json"), "{results:?}");
    assert!(
        matches!(results[0].status, ImportStatus::Refused(_)),
        "{results:?}"
    );
    assert_eq!(results[1].status, ImportStatus::Added, "{results:?}");
    assert_eq!(results[1].account, "codex:flat@example.com");
    assert_eq!(
        stored(&store, "codex:flat@example.com"),
        Some(Credential::OAuth {
            provider: "codex".into(),
            account: "flat@example.com".into(),
            access_token: "test-token-9-codex-access".into(),
            refresh_token: Some("test-token-9-codex-refresh".into()),
            expires_at_unix: None,
            siwc: None,
        })
    );
}

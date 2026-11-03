//! End-to-end tests for the read-only agent account import (P11a): Codex's
//! `auth.json` sign-ins (tokens mode only), Claude Code's
//! `.credentials.json` sign-in, the multi-home scan magpie's
//! `appdir.CodexHomes` performs, corrupt-file tolerance, and that the
//! import path `accounts import-agent` runs writes `secrets.json` without
//! ever touching the agent files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use any_converter_server::credentials::{
    Credential, FileSecretStore, SecretStore, codex_homes, discover_with_env, read_claude,
    read_codex_with_env, to_credential,
};
use serde_json::json;

/// The environment lookup every test uses: `CODEX_HOME` never counts, so a
/// real one in the developer's shell cannot leak real credentials into the
/// assertions.
fn no_env(_name: &str) -> Option<String> {
    None
}

/// A fake JWT whose payload is `claims`: a real-looking header, the
/// base64url-encoded payload, and a bogus signature. Import decodes the
/// payload without verifying, exactly as magpie reads it.
fn jwt_with(claims: serde_json::Value) -> String {
    format!(
        "{}.{}.{}",
        b64url(b"{\"alg\":\"none\"}"),
        b64url(&serde_json::to_vec(&claims).unwrap()),
        b64url(b"sig")
    )
}

/// base64url (RFC 4648 §5) encoding without padding — test-side helper.
fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

/// Write an `auth.json` into `home/.codex` and return its path.
fn write_codex_auth(home: &Path, auth: serde_json::Value) -> PathBuf {
    let dir = home.join(".codex");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("auth.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&auth).unwrap()).unwrap();
    path
}

#[test]
fn codex_tokens_account_is_found() {
    let home = tempfile::tempdir().unwrap();
    let id_token = jwt_with(json!({
        "email": "me@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": "acct-from-claims"
        }
    }));
    let access_token = jwt_with(json!({ "exp": 1_800_000_000 }));
    write_codex_auth(
        home.path(),
        json!({
            "auth_mode": "tokens",
            "tokens": {
                "id_token": id_token,
                "access_token": access_token,
                "refresh_token": "rt-secret",
                "account_id": "acct-1"
            },
            "OPENAI_API_KEY": null
        }),
    );

    let found = read_codex_with_env(&no_env, home.path());
    assert_eq!(found.len(), 1);
    let account = &found[0];
    assert_eq!(account.provider, "codex");
    assert_eq!(account.account, "codex:me@example.com");
    assert_eq!(account.email, "me@example.com");
    assert_eq!(account.plan, "pro");
    assert_eq!(account.expires_at_unix, Some(1_800_000_000));
    assert_eq!(account.home, "", "the default codex home reports as empty");
    assert!(account.source.ends_with("auth.json"));
    assert_eq!(account.refresh_token, "rt-secret");
}

#[test]
fn codex_api_key_auth_mode_is_not_a_subscription() {
    // `auth_mode == "apiKey"` with OPENAI_API_KEY set: an API-key account,
    // not a subscription.
    let api_key_home = tempfile::tempdir().unwrap();
    write_codex_auth(
        api_key_home.path(),
        json!({
            "auth_mode": "apiKey",
            "tokens": {
                "id_token": null,
                "access_token": null,
                "refresh_token": "rt",
                "account_id": null
            },
            "OPENAI_API_KEY": "sk-key"
        }),
    );
    assert!(read_codex_with_env(&no_env, api_key_home.path()).is_empty());

    // `auth_mode == "tokens"` but OPENAI_API_KEY set: still an API-key
    // account.
    let key_home = tempfile::tempdir().unwrap();
    write_codex_auth(
        key_home.path(),
        json!({
            "auth_mode": "tokens",
            "tokens": {
                "id_token": null,
                "access_token": "at",
                "refresh_token": "rt",
                "account_id": "acct-1"
            },
            "OPENAI_API_KEY": "sk-key"
        }),
    );
    assert!(read_codex_with_env(&no_env, key_home.path()).is_empty());
}

#[test]
fn codex_homes_finds_siblings() {
    let home = tempfile::tempdir().unwrap();
    // The main home, a dot-prefixed sibling, and a one-level-down profile
    // (magpie: ~/.codex-profiles/work).
    for name in [".codex", ".codex-work"] {
        let dir = home.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), "{}").unwrap();
    }
    let nested = home.path().join(".codex-profiles").join("work");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("auth.json"), "{}").unwrap();

    let dirs = codex_homes(&no_env, home.path());
    assert_eq!(dirs.len(), 3, "homes: {dirs:?}");
    assert!(dirs.contains(&home.path().join(".codex")));
    assert!(dirs.contains(&home.path().join(".codex-work")));
    assert!(dirs.contains(&nested));
}

#[test]
fn claude_credentials_are_read() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join(".credentials.json"),
        serde_json::to_vec_pretty(&json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-x",
                "refreshToken": "sk-ant-ort01-y",
                "expiresAt": 1_900_000_000_999_i64,
                "scopes": ["claude:api"],
                "subscriptionType": "pro"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude.join(".claude.json"),
        serde_json::to_vec_pretty(&json!({
            "oauthAccount": {
                "emailAddress": "claude@example.com",
                "organizationName": "acme"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let found = read_claude(home.path());
    assert_eq!(found.len(), 1);
    let account = &found[0];
    assert_eq!(account.provider, "claude");
    assert_eq!(account.account, "claude:claude@example.com");
    assert_eq!(account.email, "claude@example.com");
    assert_eq!(account.plan, "pro");
    // `expiresAt` is milliseconds; the import stores unix seconds.
    assert_eq!(account.expires_at_unix, Some(1_900_000_000));
    assert!(account.source.ends_with(".credentials.json"));
}

#[test]
fn corrupt_file_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::fs::write(home.path().join(".codex/auth.json"), "{ not json").unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(home.path().join(".claude/.credentials.json"), "[1,2,3").unwrap();
    // A sibling home with a valid sign-in: a broken one never stops the
    // others from being read.
    let good = home.path().join(".codex-good");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(
        good.join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "auth_mode": "tokens",
            "tokens": {
                "id_token": jwt_with(json!({ "email": "good@example.com" })),
                "access_token": "at",
                "refresh_token": "rt",
                "account_id": null
            },
            "OPENAI_API_KEY": null
        }))
        .unwrap(),
    )
    .unwrap();

    let codex = read_codex_with_env(&no_env, home.path());
    assert_eq!(
        codex.len(),
        1,
        "the corrupt default home is skipped while the sibling is read: {codex:?}"
    );
    assert_eq!(codex[0].email, "good@example.com");
    assert!(read_claude(home.path()).is_empty());
    assert_eq!(discover_with_env(&no_env, home.path()).len(), 1);
}

#[test]
fn import_writes_secrets_and_never_touches_the_agent_file() {
    let home = tempfile::tempdir().unwrap();
    let codex_auth = write_codex_auth(
        home.path(),
        json!({
            "auth_mode": "tokens",
            "tokens": {
                "id_token": jwt_with(json!({ "email": "me@example.com" })),
                "access_token": "at-secret",
                "refresh_token": "rt-secret",
                "account_id": "acct-1"
            },
            "OPENAI_API_KEY": null
        }),
    );
    let before = std::fs::read(&codex_auth).unwrap();

    // The exact path `accounts import-agent` runs: discover → OAuth
    // credential → FileSecretStore.put. The agent file is only ever read.
    let found = discover_with_env(&no_env, home.path());
    assert_eq!(found.len(), 1);
    let secrets_path = home
        .path()
        .join(".config")
        .join("any-converter")
        .join("secrets.json");
    let store = FileSecretStore::new(&secrets_path);
    for account in &found {
        store
            .put(&account.account, &to_credential(account))
            .unwrap();
    }

    let Some(Credential::OAuth {
        provider,
        account,
        access_token,
        refresh_token,
        expires_at_unix,
        siwc: None,
    }) = store.get("codex:me@example.com").unwrap()
    else {
        panic!("expected an OAuth credential under codex:me@example.com");
    };
    assert_eq!(provider, "codex");
    assert_eq!(account, "me@example.com");
    assert_eq!(access_token, "at-secret");
    assert_eq!(refresh_token.as_deref(), Some("rt-secret"));
    assert!(expires_at_unix.is_none());

    // The agent file is byte-for-byte what it was, and nothing else was
    // written beside it.
    assert_eq!(std::fs::read(&codex_auth).unwrap(), before);
    let mut siblings: Vec<String> = std::fs::read_dir(codex_auth.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    siblings.sort();
    assert_eq!(siblings, vec!["auth.json"]);

    // secrets.json is written private, as the store always does.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secrets_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

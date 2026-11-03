//! The per-account part of a Sign in with ChatGPT credential, and this
//! machine's id for OpenAI.
//!
//! [`Credential::OAuth`](super::Credential) already holds the tokens, and
//! widening it would touch every construction site, so the SIWC-only fields
//! (the client id OpenAI issues at sign-in, the host id it was issued for,
//! the last ID token, the granted scopes) live beside the secrets file —
//! the same shape `revoked.json` and `adopted.json` already use.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a ChatGPT API sign-in leaves besides the tokens
/// (magpie `siwcCreds`, minus the tokens themselves).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiwcMeta {
    /// The client id OpenAI issued when the sign-in registered the app.
    #[serde(rename = "clientId")]
    pub client_id: String,
    /// This machine's id, the same every time (`urn:uuid:...`).
    #[serde(rename = "hostId")]
    pub host_id: String,
    /// The last ID token, whose claims say who this is and on what plan.
    #[serde(rename = "idToken", default)]
    pub id_token: String,
    /// The scopes OpenAI granted.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// `<config>/siwc-accounts.json`: account id → its SIWC-only fields.
pub fn default_path() -> PathBuf {
    crate::config::secrets_path()
        .parent()
        .map(|dir| dir.join("siwc-accounts.json"))
        .unwrap_or_else(|| PathBuf::from("siwc-accounts.json"))
}

/// Read the side file; a missing one is empty, an unreadable one is empty
/// too (a lost client id only means signing in again).
pub fn load(path: &Path) -> BTreeMap<String, SiwcMeta> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// One account's fields, when it has any.
pub fn get(path: &Path, account_id: &str) -> Option<SiwcMeta> {
    load(path).remove(account_id)
}

/// Keep `meta` for `account_id`, leaving the other accounts as they are.
pub fn put(path: &Path, account_id: &str, meta: &SiwcMeta) -> Result<(), String> {
    let mut all = load(path);
    all.insert(account_id.to_string(), meta.clone());
    write(path, &all)
}

/// Forget `account_id`'s fields.
pub fn remove(path: &Path, account_id: &str) -> Result<(), String> {
    let mut all = load(path);
    if all.remove(account_id).is_none() {
        return Ok(());
    }
    write(path, &all)
}

fn write(path: &Path, all: &BTreeMap<String, SiwcMeta>) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::credentials::store::write_private(path, &json).map_err(|e| e.to_string())
}

/// This machine's id for OpenAI, made once and kept (magpie `siwcHostID`).
///
/// The file is read when it holds a `urn:uuid:` id; anything else — a
/// missing file, a half-written one — is replaced by a fresh one.
pub fn host_id(path: &Path) -> Result<String, String> {
    if let Ok(bytes) = std::fs::read(path) {
        let existing = String::from_utf8_lossy(&bytes).trim().to_string();
        if existing.starts_with("urn:uuid:") {
            return Ok(existing);
        }
    }
    let mut bytes = crate::credentials::gemini::random_bytes(16);
    // A version 4 UUID: the version nibble is 4, the variant bits are 10.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::credentials::gemini::hex_encode(&bytes);
    let id = format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    crate::credentials::store::write_private(path, format!("{id}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// `<config>/chatgpt-api-host`: where [`host_id`] is kept.
pub fn host_id_path() -> PathBuf {
    default_path()
        .parent()
        .map(|dir| dir.join("chatgpt-api-host"))
        .unwrap_or_else(|| PathBuf::from("chatgpt-api-host"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "any-converter-siwc-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn meta_round_trips_and_leaves_other_accounts() {
        let dir = temp("meta");
        let path = dir.join("siwc-accounts.json");
        put(
            &path,
            "chatgpt-api:a@b.c",
            &SiwcMeta {
                client_id: "client-1".into(),
                host_id: "urn:uuid:x".into(),
                id_token: "id-1".into(),
                scopes: vec!["openid".into()],
            },
        )
        .unwrap();
        put(
            &path,
            "chatgpt-api:d@e.f",
            &SiwcMeta {
                client_id: "client-2".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let got = get(&path, "chatgpt-api:a@b.c").unwrap();
        assert_eq!(got.client_id, "client-1");
        assert_eq!(got.scopes, vec!["openid".to_string()]);
        assert_eq!(
            get(&path, "chatgpt-api:d@e.f").unwrap().client_id,
            "client-2"
        );

        remove(&path, "chatgpt-api:a@b.c").unwrap();
        assert!(get(&path, "chatgpt-api:a@b.c").is_none());
        assert!(get(&path, "chatgpt-api:d@e.f").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_side_file_is_private() {
        let dir = temp("perm");
        let path = dir.join("siwc-accounts.json");
        put(&path, "a", &SiwcMeta::default()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_id_is_made_once_and_kept() {
        let dir = temp("host");
        let path = dir.join("chatgpt-api-host");
        let first = host_id(&path).unwrap();
        assert!(first.starts_with("urn:uuid:"), "{first}");
        // A v4 UUID: 36 characters of id after the scheme.
        let uuid = first.trim_start_matches("urn:uuid:");
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.split('-').next().unwrap().len(), 8);
        assert_eq!(host_id(&path).unwrap(), first, "the same every time");

        // A file that isn't an id is replaced rather than trusted.
        std::fs::write(&path, b"garbage").unwrap();
        let again = host_id(&path).unwrap();
        assert!(again.starts_with("urn:uuid:"));
        assert_ne!(again, first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_side_file_reads_as_empty() {
        let dir = temp("missing");
        let path = dir.join("nope.json");
        assert!(load(&path).is_empty());
        assert!(get(&path, "x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

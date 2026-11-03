//! Read-only import of accounts other tools keep on this machine.
//!
//! Codex CLI keeps its ChatGPT sign-in in `auth.json` (one per CODEX_HOME;
//! `~/.codex` by default, magpie `appdir.CodexHomes`) and Claude Code keeps
//! its Anthropic sign-in in `~/.claude/.credentials.json`, with the user
//! identity (`oauthAccount.emailAddress`) in Claude Code's state file.
//! [`discover`] finds both kinds the way magpie reads them, without ever
//! writing back: writing the tools' files back is P11b (last-good
//! generations + file locks + "never overwrite what we could not read").
//!
//! The returned accounts carry no writable handle to the tools' files, and
//! nothing in this module calls `fs::write` — token values only ever move
//! into memory and into [`crate::credentials::store::FileSecretStore`]
//! entries through [`to_credential`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::Credential;

/// An account found in another tool's credential store.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundAccount {
    /// `"codex"` or `"claude"`.
    pub provider: String,
    /// The id an [`super::AccountResolver`] uses, e.g. `"codex:me@example.com"`.
    pub account: String,
    pub email: String,
    pub plan: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: Option<i64>,
    /// The file it came from.
    pub source: String,
    /// The CODEX_HOME it lives in, `""` for the default (P11b writes back
    /// here).
    pub home: String,
}

/// Codex homes: `$CODEX_HOME` (else `home/.codex`) plus siblings that look
/// like a codex profile and hold `auth.json` or `sessions/` (magpie
/// `appdir.CodexHomes`): in `home` and beside the main home, every folder
/// whose name starts with `codex` (after one leading dot), or one folder
/// down in such a folder (`~/.codex-profiles/work`).
pub fn codex_homes(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> Vec<PathBuf> {
    let main = match env("CODEX_HOME") {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => home.join(".codex"),
    };
    let mut out = vec![main.clone()];
    let mut seen = HashSet::from([dir_key(&main)]);
    let mut roots = vec![home.to_path_buf()];
    if let Some(parent) = main.parent() {
        if !parent.as_os_str().is_empty() && dir_key(parent) != dir_key(home) {
            roots.push(parent.to_path_buf());
        }
    }
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let name = name.strip_prefix('.').unwrap_or(&name);
            if !name.starts_with("codex") {
                continue;
            }
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if codex_home_at(&dir) {
                push_home(&mut out, &mut seen, dir);
                continue;
            }
            let Ok(subs) = std::fs::read_dir(&dir) else {
                continue;
            };
            for sub in subs.flatten() {
                if codex_home_at(&sub.path()) {
                    push_home(&mut out, &mut seen, sub.path());
                }
            }
        }
    }
    out
}

/// Add `dir` to `out` unless one of its links' names is already there.
fn push_home(out: &mut Vec<PathBuf>, seen: &mut HashSet<String>, dir: PathBuf) {
    if seen.insert(dir_key(&dir)) {
        out.push(dir);
    }
}

/// The key two names of one folder share: where its links lead (magpie
/// `sameDir`), or the path itself when the folder does not exist yet.
fn dir_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Whether Codex has run in `dir`: it signed in there (`auth.json`) or kept
/// a session there (`sessions/`) (magpie `codexHomeAt`).
fn codex_home_at(dir: &Path) -> bool {
    dir.join("auth.json").is_file() || dir.join("sessions").is_dir()
}

/// Every ChatGPT sign-in Codex keeps on this machine (one per codex home).
///
/// Only `auth_mode == "tokens"` with a non-empty refresh token counts as a
/// usable ChatGPT subscription account; an `apiKey` mode or a set
/// `OPENAI_API_KEY` is an API-key account, not a subscription (magpie
/// `codexAccount`). The `id_token` is decoded without checking its
/// signature — the tokens are the user's own, only their claims matter
/// here (magpie `jwtClaims`).
pub fn read_codex(home: &Path) -> Vec<FoundAccount> {
    read_codex_with_env(&env_var, home)
}

/// [`read_codex`] with an injected environment lookup, so tests can pass a
/// closure that never sees a real `CODEX_HOME`.
pub fn read_codex_with_env(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> Vec<FoundAccount> {
    let homes = codex_homes(env, home);
    let main = homes
        .first()
        .cloned()
        .unwrap_or_else(|| home.join(".codex"));
    let mut out = Vec::new();
    for dir in homes {
        out.extend(read_codex_dir(&dir, &main));
    }
    out
}

/// One codex home's sign-in, when it is a ChatGPT subscription account.
fn read_codex_dir(dir: &Path, main: &Path) -> Vec<FoundAccount> {
    let auth_path = dir.join("auth.json");
    let Ok(bytes) = std::fs::read(&auth_path) else {
        return Vec::new();
    };
    let Ok(auth) = serde_json::from_slice::<CodexAuth>(&bytes) else {
        return Vec::new();
    };
    // Only `auth_mode == "tokens"` with a refresh token is a usable
    // ChatGPT subscription account; an API key is not a subscription.
    let subscription = auth.auth_mode.as_deref() == Some("tokens")
        && auth
            .openai_api_key
            .as_ref()
            .is_none_or(serde_json::Value::is_null);
    let Some(tokens) = auth.tokens else {
        return Vec::new();
    };
    let refresh_token = tokens.refresh_token.unwrap_or_default();
    if !subscription || refresh_token.is_empty() {
        return Vec::new();
    }

    // Identity comes from the id_token's claims, decoded unverified: the
    // email, the ChatGPT account id, and the plan (magpie `codexUser`).
    let id_claims = tokens
        .id_token
        .as_deref()
        .and_then(jwt_payload)
        .unwrap_or(serde_json::Value::Null);
    let email = claim_str(&id_claims, &["email"]).trim().to_string();
    let plan = claim_str(
        &id_claims,
        &["https://api.openai.com/auth", "chatgpt_plan_type"],
    )
    .to_string();
    let mut account_id = tokens.account_id.unwrap_or_default();
    if account_id.is_empty() {
        account_id = claim_str(
            &id_claims,
            &["https://api.openai.com/auth", "chatgpt_account_id"],
        )
        .to_string();
    }

    // The account id AccountResolver uses; when the token claims give no
    // email, the account id or the home folder's name stands in.
    let account = if !email.is_empty() {
        format!("codex:{email}")
    } else if !account_id.is_empty() {
        format!("codex:{account_id}")
    } else {
        format!(
            "codex:{}",
            dir.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("codex")
        )
    };

    // The access token's own `exp` claim, as magpie reads it.
    let expires_at_unix = tokens
        .access_token
        .as_deref()
        .and_then(jwt_payload)
        .and_then(|claims| claims.get("exp").and_then(serde_json::Value::as_i64));

    vec![FoundAccount {
        provider: "codex".to_string(),
        account,
        email,
        plan,
        access_token: tokens.access_token.unwrap_or_default(),
        refresh_token,
        expires_at_unix,
        source: auth_path.to_string_lossy().into_owned(),
        home: if dir_key(dir) == dir_key(main) {
            String::new()
        } else {
            dir.to_string_lossy().into_owned()
        },
    }]
}

/// Claude Code's sign-in: `home/.claude/.credentials.json`, with the user
/// identity (`oauthAccount.emailAddress`) read from Claude Code's state
/// file.
pub fn read_claude(home: &Path) -> Vec<FoundAccount> {
    let creds_path = home.join(".claude").join(".credentials.json");
    let Ok(bytes) = std::fs::read(&creds_path) else {
        return Vec::new();
    };
    let Ok(creds) = serde_json::from_slice::<ClaudeCredentials>(&bytes) else {
        return Vec::new();
    };
    let Some(oauth) = creds.claude_ai_oauth else {
        return Vec::new();
    };
    let access_token = oauth.access_token.unwrap_or_default();
    if access_token.is_empty() {
        return Vec::new();
    }

    let email = claude_email(home);
    let plan = oauth.subscription_type.unwrap_or_default();
    let account = if !email.is_empty() {
        format!("claude:{email}")
    } else if !plan.is_empty() {
        format!("claude:{plan}")
    } else {
        "claude:default".to_string()
    };

    vec![FoundAccount {
        provider: "claude".to_string(),
        account,
        email,
        plan,
        access_token,
        refresh_token: oauth.refresh_token.unwrap_or_default(),
        // `expiresAt` is milliseconds since the Unix epoch.
        expires_at_unix: oauth.expires_at.map(|ms| ms / 1000),
        source: creds_path.to_string_lossy().into_owned(),
        home: String::new(),
    }]
}

/// Both kinds of agent sign-ins, as [`super::AccountResolver`] would store
/// them: every Codex home first, then Claude Code's.
pub fn discover(home: &Path) -> Vec<FoundAccount> {
    discover_with_env(&env_var, home)
}

/// [`discover`] with an injected environment lookup (see
/// [`read_codex_with_env`]).
pub fn discover_with_env(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> Vec<FoundAccount> {
    let mut out = read_codex_with_env(env, home);
    out.extend(read_claude(home));
    out
}

/// The OAuth credential `any-converter accounts import-agent` stores for
/// an account (provider `"codex"`/`"claude"`, written `0600` through
/// [`crate::credentials::store::FileSecretStore`]). Like the gemini and
/// copilot sign-ins, `account` holds the bare identity the id is made
/// from (the email, or the fallback that named it).
pub fn to_credential(account: &FoundAccount) -> Credential {
    Credential::OAuth {
        provider: account.provider.clone(),
        account: account
            .account
            .split_once(':')
            .map(|(_, bare)| bare)
            .unwrap_or(&account.account)
            .to_string(),
        access_token: account.access_token.clone(),
        refresh_token: (!account.refresh_token.is_empty()).then(|| account.refresh_token.clone()),
        expires_at_unix: account.expires_at_unix,
        siwc: None,
    }
}

/// The email of Claude Code's sign-in: `oauthAccount.emailAddress` from
/// Claude Code's state file (`~/.claude.json`; the config-dir copy
/// `~/.claude/.claude.json` counts too). Empty when unreadable or absent —
/// the identity is not part of the credential blob.
fn claude_email(home: &Path) -> String {
    for path in [
        home.join(".claude.json"),
        home.join(".claude").join(".claude.json"),
    ] {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(state) = serde_json::from_slice::<ClaudeState>(&bytes) {
            if let Some(email) = state
                .oauth_account
                .and_then(|account| account.email_address)
            {
                if !email.trim().is_empty() {
                    return email;
                }
            }
        }
    }
    String::new()
}

/// The payload of a JWT, decoded without checking its signature (magpie
/// `jwtClaims`: the tokens are the user's own, only their claims matter).
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// base64url (RFC 4648 §5) decode, padding optional — hand-rolled because
/// the workspace adds no base64 dependency.
fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a' + 26),
            b'0'..=b'9' => u32::from(c - b'0' + 52),
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// The string a claim path reads, `""` when any step is missing or not a
/// string (magpie `claimString`).
fn claim_str<'a>(mut claims: &'a serde_json::Value, path: &[&str]) -> &'a str {
    for key in path {
        let Some(next) = claims.get(*key) else {
            return "";
        };
        claims = next;
    }
    claims.as_str().unwrap_or("")
}

/// `auth.json` as Codex keeps it (magpie `codexAuth`).
#[derive(serde::Deserialize)]
struct CodexAuth {
    auth_mode: Option<String>,
    tokens: Option<CodexTokens>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct CodexTokens {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

/// `~/.claude/.credentials.json` (magpie `claudeCredentials`).
#[derive(serde::Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOAuth>,
}

#[derive(serde::Deserialize)]
struct ClaudeOAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    /// Milliseconds since the Unix epoch.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

/// `oauthAccount` of Claude Code's state file (`~/.claude.json`).
#[derive(serde::Deserialize)]
struct ClaudeState {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<ClaudeOAuthAccount>,
}

#[derive(serde::Deserialize)]
struct ClaudeOAuthAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
}

/// The environment lookup the production entrypoints use: the process
/// environment, blank values counting as unset (magpie reads `CODEX_HOME`
/// through `LookupEnv`).
fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn b64url_decodes_padded_and_unpadded() {
        assert_eq!(b64url_decode("TWE"), Some(b"Ma".to_vec()));
        assert_eq!(b64url_decode("TWE="), Some(b"Ma".to_vec()));
        assert_eq!(b64url_decode("TQ"), Some(b"M".to_vec()));
        assert_eq!(b64url_decode(""), Some(Vec::new()));
    }

    #[test]
    fn b64url_rejects_invalid_characters() {
        assert_eq!(b64url_decode("TWE+"), None);
        assert_eq!(b64url_decode("T!E"), None);
    }

    #[test]
    fn claim_str_reads_nested_claims() {
        let claims: serde_json::Value = serde_json::from_str(
            r#"{"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}"#,
        )
        .unwrap();
        assert_eq!(claim_str(&claims, &["email"]), "a@b.c");
        assert_eq!(
            claim_str(
                &claims,
                &["https://api.openai.com/auth", "chatgpt_plan_type"]
            ),
            "pro"
        );
        assert_eq!(claim_str(&claims, &["missing"]), "");
        assert_eq!(claim_str(&claims, &["email", "deep"]), "");
    }

    #[test]
    fn jwt_payload_decodes_without_verifying() {
        // header.payload.sig; payload {"email":"me@example.com"}
        let token = "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6Im1lQGV4YW1wbGUuY29tIn0.sig";
        let claims = jwt_payload(token).unwrap();
        assert_eq!(claims["email"], "me@example.com");
        assert_eq!(jwt_payload("not-a-jwt"), None);
        assert_eq!(jwt_payload("a.b"), None);
    }
}

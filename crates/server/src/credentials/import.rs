//! Import accounts from other tools' export files into a
//! [`FileSecretStore`] (magpie `internal/provider/login_import.go`).
//!
//! Three export shapes are recognised, in this priority order:
//!
//! 1. **claude-swap (`cswap`) exports** — `{"format": 1, "accounts":
//!    [{"credentials": {"claudeAiOauth": {...}}, "config": {"oauthAccount":
//!    {...}}, "alias": "..."}]}`. A Claude account whose email (or alias)
//!    is already stored is only replaced when the export's `expiresAt` is
//!    strictly later than the stored sign-in's (magpie `claudeNewerKept`).
//!    An export without `expiresAt` never replaces anything.
//! 2. **Sub2API exports** — a top-level array or `{"accounts": [...]}` of
//!    `{"platform", "credentials", "extra"}` entries; Claude and Codex
//!    entries are both imported.
//! 3. **CLIProxyAPI files** — `{"<email>": {"type": "claude"|"codex",
//!    "access_token", "refresh_token", "expires", ...}}`, or one flat
//!    account object.
//!
//! Everything else is refused with guidance. One unreadable or
//! unrecognised file never stops the other files from being imported, and
//! the files are only ever read, never modified.

use std::path::Path;

use serde_json::{Map, Value};

use super::Credential;
use super::store::{FileSecretStore, SecretStore};

/// What became of one account in the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportStatus {
    /// Written to the store.
    Added,
    /// The store already has this account (or a newer sign-in of it); why
    /// the file's entry was kept instead of written.
    Exists(String),
    /// The file's entry can't be imported; why.
    Refused(String),
}

/// One account read from a file, and what importing it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAccount {
    /// `<provider>:<email-or-alias>`; empty when the row is a whole-file
    /// refusal with no account behind it.
    pub account: String,
    /// `claude` or `codex` (empty for whole-file refusals).
    pub provider: String,
    /// The account's email (empty when only an alias is known).
    pub email: String,
    pub status: ImportStatus,
    /// The file the row came from (the path as given).
    pub file: String,
}

/// Read a file (or every `*.json` under a directory, by file-name order)
/// and import the accounts it holds into `store`, honouring magpie's
/// newer-wins rule for claude-swap exports. Never fails: every row's
/// outcome is reported through an [`ImportStatus`].
pub fn import_path(store: &FileSecretStore, path: &Path) -> Vec<ImportedAccount> {
    let mut out = Vec::new();
    if path.is_dir() {
        let mut files = match std::fs::read_dir(path) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|file| {
                    file.is_file()
                        && file
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                })
                .collect::<Vec<_>>(),
            Err(err) => {
                out.push(refused(
                    "",
                    String::new(),
                    String::new(),
                    format!("cannot read directory: {err}"),
                    path.display().to_string(),
                ));
                return out;
            }
        };
        files.sort();
        if files.is_empty() {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!("no *.json files under {} to import", path.display()),
                path.display().to_string(),
            ));
            return out;
        }
        for file in files {
            import_file(store, &file, &mut out);
        }
    } else {
        import_file(store, path, &mut out);
    }
    out
}

/// Import one file, pushing one row per account (or one whole-file refusal).
fn import_file(store: &FileSecretStore, path: &Path, out: &mut Vec<ImportedAccount>) {
    let file = path.display().to_string();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!("cannot read: {err}"),
                file,
            ));
            return;
        }
    };
    let root: Value = match serde_json::from_slice(&bytes) {
        Ok(root) => root,
        Err(err) => {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!("not valid JSON: {err}"),
                file,
            ));
            return;
        }
    };
    match &root {
        Value::Object(map) => dispatch_object(store, &file, map, &root, out),
        Value::Array(items) => {
            if items.iter().any(has_platform) {
                for item in items {
                    import_sub2api_entry(store, &file, item, out);
                }
            } else {
                out.push(unknown_shape(&file));
            }
        }
        _ => out.push(unknown_shape(&file)),
    }
}

/// Shape detection for a top-level object, in priority order: cswap
/// (`format`/`version` + `accounts`) > Sub2API (`platform`) > CLIProxyAPI
/// (email → credential map, or one flat account object).
fn dispatch_object(
    store: &FileSecretStore,
    file: &str,
    map: &Map<String, Value>,
    root: &Value,
    out: &mut Vec<ImportedAccount>,
) {
    // Encrypted claude-swap exports first: there is nothing readable.
    if map.get("encrypted").and_then(Value::as_bool) == Some(true) {
        out.push(refused(
            "",
            String::new(),
            String::new(),
            ENCRYPTED_CSWAP_MSG.to_string(),
            file.to_string(),
        ));
        return;
    }
    if let Some(accounts) = map.get("accounts").and_then(Value::as_array) {
        if accounts.is_empty() {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                "the file holds no accounts".to_string(),
                file.to_string(),
            ));
            return;
        }
        if map.contains_key("format")
            || map.contains_key("version")
            || map.contains_key("swapVersion")
        {
            import_cswap(store, file, map, accounts, out);
            return;
        }
        if accounts.iter().any(has_platform) {
            for item in accounts {
                import_sub2api_entry(store, file, item, out);
            }
            return;
        }
        out.push(unknown_shape(file));
        return;
    }
    if map.contains_key("platform") {
        import_sub2api_entry(store, file, root, out);
        return;
    }
    if is_cliproxy_map(map) {
        import_cliproxy_map(store, file, map, out);
        return;
    }
    if is_flat_account(map) {
        import_cliproxy_entry(store, file, None, map, out);
        return;
    }
    out.push(unknown_shape(file));
}

/// Why an encrypted claude-swap export is refused, including how to get a
/// plaintext one.
const ENCRYPTED_CSWAP_MSG: &str = "an encrypted claude-swap export: decrypt it first \
     (e.g. `gpg -d`), or export it again from claude-swap without encryption \
     (`cswap --export`), then import what that gives";

/// Why an API-key entry is refused, including what to do with it instead.
const API_KEY_MSG: &str = "an API key, not an OAuth sign-in; add it as a provider \
     API key in config.toml instead";

/// Why a Codex entry inside a claude-swap export is refused.
const CODEX_CSWAP_MSG: &str = "a Codex account in a claude-swap export: claude-swap \
     files hold Claude sign-ins; import Codex accounts from a Codex tool's export \
     (CLIProxyAPI or Sub2API) instead";

/// Import the accounts of a claude-swap export (magpie `cswapEntry`).
///
/// Refuses the whole export when `format`/`version` is not 1, and refuses
/// API-key and Codex entries individually. Claude entries follow
/// [`Entry::newer_wins`]: the export only replaces a stored sign-in whose
/// `expiresAt` is older.
fn import_cswap(
    store: &FileSecretStore,
    file: &str,
    map: &Map<String, Value>,
    accounts: &[Value],
    out: &mut Vec<ImportedAccount>,
) {
    if let Some(format) = map.get("format").or_else(|| map.get("version")) {
        let is_v1 = matches!(format.as_i64(), Some(1))
            || matches!(format.as_u64(), Some(1))
            || matches!(format.as_f64(), Some(f) if f == 1.0);
        if !is_v1 {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!(
                    "a claude-swap export of format {format}: any-converter reads \
                     format 1 (claude-swap 0.29); re-export the accounts with \
                     `cswap --export`"
                ),
                file.to_string(),
            ));
            return;
        }
    }
    for account in accounts {
        let Some(entry_map) = account.as_object() else {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                "not an account object".to_string(),
                file.to_string(),
            ));
            continue;
        };
        let kind = str_of(Some(entry_map), &["kind"]).to_ascii_lowercase();
        let entry_type = str_of(Some(entry_map), &["type"]).to_ascii_lowercase();
        if kind == "api_key" || entry_map.get("credentials").is_some_and(Value::is_string) {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                API_KEY_MSG.to_string(),
                file.to_string(),
            ));
            continue;
        }
        if kind == "codex" || entry_type == "codex" {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                CODEX_CSWAP_MSG.to_string(),
                file.to_string(),
            ));
            continue;
        }
        let Some(credentials) = entry_map.get("credentials").and_then(Value::as_object) else {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                "no credentials object in the account".to_string(),
                file.to_string(),
            ));
            continue;
        };
        let src = credentials
            .get("claudeAiOauth")
            .and_then(Value::as_object)
            .unwrap_or(credentials);
        let oauth_account = entry_map
            .get("config")
            .and_then(Value::as_object)
            .and_then(|config| config.get("oauthAccount"))
            .and_then(Value::as_object);
        let email = first_nonempty(str_of(oauth_account, &["emailAddress"]), || {
            str_of(Some(entry_map), &["email"])
        });
        let alias = str_of(Some(entry_map), &["alias"]);
        let name = if !email.is_empty() {
            email.clone()
        } else {
            alias
        };
        import_entry(
            store,
            file,
            Entry {
                provider: "claude".to_string(),
                name,
                email,
                access_token: str_of(Some(src), &["accessToken", "access_token"]),
                refresh_token: opt_str(Some(src), &["refreshToken", "refresh_token"]),
                expires_at_unix: i64_of(Some(src), &["expiresAt"]).map(normalize_expiry),
                newer_wins: true,
            },
            out,
        );
    }
}

/// Whether a Sub2API entry carries a `platform` field.
fn has_platform(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.contains_key("platform"))
}

/// Import one account of a Sub2API export (magpie `sub2apiEntry`).
fn import_sub2api_entry(
    store: &FileSecretStore,
    file: &str,
    entry: &Value,
    out: &mut Vec<ImportedAccount>,
) {
    let Some(entry_map) = entry.as_object() else {
        out.push(refused(
            "",
            String::new(),
            String::new(),
            "not an account object".to_string(),
            file.to_string(),
        ));
        return;
    };
    let platform = str_of(Some(entry_map), &["platform"]);
    if platform.is_empty() {
        out.push(refused(
            "",
            String::new(),
            String::new(),
            "no platform field in the account: Sub2API entries say which vendor \
             the account is for"
                .to_string(),
            file.to_string(),
        ));
        return;
    }
    let provider = match platform.to_ascii_lowercase().as_str() {
        "openai" => "codex",
        "claude" | "anthropic" => "claude",
        _ => {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!(
                    "platform \"{platform}\" is not supported: Sub2API entries for \
                     the claude/anthropic and openai platforms can be imported"
                ),
                file.to_string(),
            ));
            return;
        }
    };
    if str_of(Some(entry_map), &["type"]).eq_ignore_ascii_case("apikey") {
        out.push(refused(
            provider,
            String::new(),
            String::new(),
            API_KEY_MSG.to_string(),
            file.to_string(),
        ));
        return;
    }
    let Some(credentials) = entry_map.get("credentials").and_then(Value::as_object) else {
        out.push(refused(
            provider,
            String::new(),
            String::new(),
            "no credentials object in the account".to_string(),
            file.to_string(),
        ));
        return;
    };
    let extra = entry_map.get("extra").and_then(Value::as_object);
    // Claude entries keep the token under `credentials.claudeAiOauth`, with
    // Anthropic's own names; Codex entries keep `access_token` /
    // `refresh_token` in `credentials` directly.
    let src = credentials
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .unwrap_or(credentials);
    let email = first_nonempty(
        str_of(Some(credentials), &["email_address", "email"]),
        || {
            first_nonempty(str_of(extra, &["email_address", "email"]), || {
                name_if_email(Some(entry_map))
            })
        },
    );
    if email.is_empty() {
        out.push(refused(
            provider,
            String::new(),
            String::new(),
            format!(
                "the file doesn't say which {provider} account this is (no email): \
                 sign in to it with `any-converter accounts add` instead"
            ),
            file.to_string(),
        ));
        return;
    }
    import_entry(
        store,
        file,
        Entry {
            provider: provider.to_string(),
            name: email.clone(),
            email,
            access_token: str_of(Some(src), &["accessToken", "access_token"]),
            refresh_token: opt_str(Some(src), &["refreshToken", "refresh_token"]),
            expires_at_unix: i64_of(Some(src), &["expiresAt", "expires", "expires_at"])
                .map(normalize_expiry),
            newer_wins: false,
        },
        out,
    );
}

/// Whether a top-level object is a CLIProxyAPI map: every value is an
/// account object (token fields or a `type`).
fn is_cliproxy_map(map: &Map<String, Value>) -> bool {
    !map.is_empty()
        && map.values().all(|value| {
            value
                .as_object()
                .is_some_and(|entry| has_token_fields(entry) || entry.contains_key("type"))
        })
}

/// Whether a top-level object is one flat CLIProxyAPI account.
fn is_flat_account(map: &Map<String, Value>) -> bool {
    map.contains_key("type") && has_token_fields(map)
}

/// Does the entry hold any credential fields at all?
fn has_token_fields(entry: &Map<String, Value>) -> bool {
    [
        "access_token",
        "refresh_token",
        "accessToken",
        "refreshToken",
        "claudeAiOauth",
    ]
    .iter()
    .any(|key| entry.contains_key(*key))
}

/// Import every account of a CLIProxyAPI map file.
fn import_cliproxy_map(
    store: &FileSecretStore,
    file: &str,
    map: &Map<String, Value>,
    out: &mut Vec<ImportedAccount>,
) {
    for (key, value) in map {
        let Some(entry) = value.as_object() else {
            out.push(refused(
                "",
                String::new(),
                String::new(),
                format!("the entry for {key} is not an account object"),
                file.to_string(),
            ));
            continue;
        };
        import_cliproxy_entry(store, file, Some(key), entry, out);
    }
}

/// Import one CLIProxyAPI account (map entry or flat object).
fn import_cliproxy_entry(
    store: &FileSecretStore,
    file: &str,
    key: Option<&str>,
    entry: &Map<String, Value>,
    out: &mut Vec<ImportedAccount>,
) {
    let entry_type = str_of(Some(entry), &["type"]).to_ascii_lowercase();
    let provider = match entry_type.as_str() {
        "claude" => "claude",
        "codex" => "codex",
        "" => {
            // No `type`: Claude Code's own shape vs Codex's id_token decide.
            if entry.contains_key("claudeAiOauth") || entry.contains_key("expiresAt") {
                "claude"
            } else if entry.contains_key("id_token") || entry.contains_key("account_id") {
                "codex"
            } else {
                out.push(refused(
                    "",
                    String::new(),
                    String::new(),
                    "no type field in the account: CLIProxyAPI entries are \"claude\" \
                     or \"codex\""
                        .to_string(),
                    file.to_string(),
                ));
                return;
            }
        }
        _ => {
            if entry.contains_key("api_key") || entry.contains_key("OPENAI_API_KEY") {
                out.push(refused(
                    "",
                    String::new(),
                    String::new(),
                    API_KEY_MSG.to_string(),
                    file.to_string(),
                ));
            } else {
                out.push(refused(
                    "",
                    String::new(),
                    String::new(),
                    format!(
                        "entry type \"{entry_type}\" is not supported: CLIProxyAPI \
                         entries are \"claude\" or \"codex\" accounts"
                    ),
                    file.to_string(),
                ));
            }
            return;
        }
    };
    let src = entry
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .unwrap_or(entry);
    let entry_email = str_of(Some(entry), &["email"]);
    let email = if !entry_email.is_empty() {
        entry_email
    } else {
        key.unwrap_or("").to_string()
    };
    import_entry(
        store,
        file,
        Entry {
            provider: provider.to_string(),
            name: email.clone(),
            email,
            access_token: str_of(Some(src), &["access_token", "accessToken"]),
            refresh_token: opt_str(Some(src), &["refresh_token", "refreshToken"]),
            expires_at_unix: i64_of(Some(src), &["expires", "expiresAt"]).map(normalize_expiry),
            newer_wins: false,
        },
        out,
    );
}

/// One account read from a file, in a shape-independent form.
struct Entry {
    provider: String,
    /// Email, or the alias when the file gives no email.
    name: String,
    email: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at_unix: Option<i64>,
    /// Only claude-swap entries may replace a stored sign-in, and only
    /// with a strictly later `expiresAt` (magpie `claudeNewerKept`).
    newer_wins: bool,
}

/// Validate one account and store it, applying the newer-wins rule for
/// claude-swap entries and never overwriting otherwise.
fn import_entry(store: &FileSecretStore, file: &str, entry: Entry, out: &mut Vec<ImportedAccount>) {
    if entry.name.is_empty() {
        out.push(refused(
            &entry.provider,
            String::new(),
            String::new(),
            "no email and no alias in the account, so there is no account name to \
             store it under"
                .to_string(),
            file.to_string(),
        ));
        return;
    }
    if entry.access_token.is_empty() && entry.refresh_token.is_none() {
        out.push(refused(
            &entry.provider,
            entry.name.clone(),
            entry.email.clone(),
            "no tokens in the entry (no access_token/refresh_token)".to_string(),
            file.to_string(),
        ));
        return;
    }
    let id = format!("{}:{}", entry.provider, entry.name);
    match stored_expiry(store, &id) {
        None => write_entry(store, file, &id, &entry, out),
        Some(_) if !entry.newer_wins => out.push(ImportedAccount {
            account: id,
            provider: entry.provider.clone(),
            email: entry.email.clone(),
            status: ImportStatus::Exists(
                "an account is already stored under this id: kept (accounts are \
                 never overwritten, except a claude-swap export whose expiresAt \
                 is newer)"
                    .to_string(),
            ),
            file: file.to_string(),
        }),
        // A stored sign-in whose expiry is unknown counts as oldest (magpie
        // treats a missing expiry as 0), so a dated export replaces it.
        Some(None) => match entry.expires_at_unix {
            None => out.push(ImportedAccount {
                account: id,
                provider: entry.provider.clone(),
                email: entry.email.clone(),
                status: ImportStatus::Exists(
                    "the export gives no expiresAt for this account, so it never \
                     replaces the stored sign-in: kept"
                        .to_string(),
                ),
                file: file.to_string(),
            }),
            Some(_) => write_entry(store, file, &id, &entry, out),
        },
        Some(Some(local)) => match entry.expires_at_unix {
            None => out.push(ImportedAccount {
                account: id,
                provider: entry.provider.clone(),
                email: entry.email.clone(),
                status: ImportStatus::Exists(
                    "the export gives no expiresAt for this account, so it never \
                     replaces the stored sign-in: kept"
                        .to_string(),
                ),
                file: file.to_string(),
            }),
            Some(file_exp) if file_exp > local => write_entry(store, file, &id, &entry, out),
            Some(file_exp) => out.push(ImportedAccount {
                account: id,
                provider: entry.provider.clone(),
                email: entry.email.clone(),
                status: ImportStatus::Exists(format!(
                    "expiresAt {file_exp} is not newer than the stored sign-in's \
                     {local}: kept"
                )),
                file: file.to_string(),
            }),
        },
    }
}

/// The stored sign-in's expiry under `id`: `None` when nothing is stored,
/// `Some(expiry)` otherwise — `Some(None)` for a stored sign-in that does
/// not say when it expires.
fn stored_expiry(store: &FileSecretStore, id: &str) -> Option<Option<i64>> {
    match store.get(id) {
        Ok(Some(Credential::OAuth {
            expires_at_unix, ..
        })) => Some(expires_at_unix),
        Ok(Some(_)) => Some(None),
        Ok(None) | Err(_) => None,
    }
}

/// Persist the entry under `id`; a store failure becomes a `Refused` row.
fn write_entry(
    store: &FileSecretStore,
    file: &str,
    id: &str,
    entry: &Entry,
    out: &mut Vec<ImportedAccount>,
) {
    let credential = Credential::OAuth {
        provider: entry.provider.clone(),
        account: entry.name.clone(),
        access_token: entry.access_token.clone(),
        refresh_token: entry.refresh_token.clone(),
        expires_at_unix: entry.expires_at_unix,
    };
    match store.put(id, &credential) {
        Ok(()) => out.push(ImportedAccount {
            account: id.to_string(),
            provider: entry.provider.clone(),
            email: entry.email.clone(),
            status: ImportStatus::Added,
            file: file.to_string(),
        }),
        Err(err) => out.push(ImportedAccount {
            account: id.to_string(),
            provider: entry.provider.clone(),
            email: entry.email.clone(),
            status: ImportStatus::Refused(format!(
                "could not write to the credential store: {err}"
            )),
            file: file.to_string(),
        }),
    }
}

/// Whole-file refusal: the shape is none of the supported exports.
fn unknown_shape(file: &str) -> ImportedAccount {
    refused(
        "",
        String::new(),
        String::new(),
        "unrecognized export shape: supported exports are claude-swap (cswap) \
         files ({format: 1, accounts: [...]}), Sub2API exports \
         ([{platform, credentials, ...}] or {accounts: [...]}) and CLIProxyAPI \
         files ({email: {type, access_token, refresh_token, expires}})"
            .to_string(),
        file.to_string(),
    )
}

/// Build an [`ImportedAccount`] row (status `Refused(why)`).
fn refused(
    provider: &str,
    name: String,
    email: String,
    why: String,
    file: String,
) -> ImportedAccount {
    ImportedAccount {
        account: if name.is_empty() {
            String::new()
        } else {
            format!("{provider}:{name}")
        },
        provider: provider.to_string(),
        email,
        status: ImportStatus::Refused(why),
        file,
    }
}

/// The first non-empty string value under any of `keys`.
fn str_of(obj: Option<&Map<String, Value>>, keys: &[&str]) -> String {
    let Some(obj) = obj else {
        return String::new();
    };
    for key in keys {
        if let Some(value) = obj.get(*key) {
            if let Some(found) = value.as_str() {
                return found.to_string();
            }
        }
    }
    String::new()
}

/// The first string value under any of `keys`, as an optional.
fn opt_str(obj: Option<&Map<String, Value>>, keys: &[&str]) -> Option<String> {
    let found = str_of(obj, keys);
    if found.is_empty() { None } else { Some(found) }
}

/// `value` when it is non-empty, else `fallback()`.
fn first_nonempty(value: String, fallback: impl FnOnce() -> String) -> String {
    if value.is_empty() { fallback() } else { value }
}

/// The first integer value under any of `keys`.
fn i64_of(obj: Option<&Map<String, Value>>, keys: &[&str]) -> Option<i64> {
    let obj = obj?;
    for key in keys {
        if let Some(value) = obj.get(*key) {
            if let Some(found) = value.as_i64() {
                return Some(found);
            }
            if let Some(found) = value.as_u64() {
                return i64::try_from(found).ok();
            }
        }
    }
    None
}

/// `expiresAt` in claude-swap/Sub2API files is milliseconds since the
/// epoch; CLIProxyAPI's `expires` is seconds for Codex entries. A value
/// larger than 1e11 (the year 5138 in seconds) is milliseconds.
fn normalize_expiry(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

/// The entry's `name`, when it looks like an email address (magpie reads
/// the account's email from the name Cockpit Tools gives each account).
fn name_if_email(obj: Option<&Map<String, Value>>) -> String {
    let name = str_of(obj, &["name"]);
    if name.contains('@') {
        name
    } else {
        String::new()
    }
}

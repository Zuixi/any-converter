//! `any-converter agents …`: a thin HTTP client over the server's
//! `/v1/agents/*` API (design §4.2).
//!
//! The CLI owns no HTTP client crate: like `accounts quotas` it speaks
//! HTTP/1.1 over one `TcpStream` per request and parses the reply by hand.
//! Every rendering/error-mapping helper is pure so the tests never need a
//! server; only [`run`] touches the network.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use any_converter_server::config::DEFAULT_PORT;
use any_converter_server::home::Home;
use any_converter_server::secrets::ApiKeyStore;

use crate::AgentsCommand;

/// Exit code for "the server is not running": scripts branch on it.
const EXIT_UNREACHABLE: i32 = 2;
/// Exit code for every other failure (API error, unexpected reply).
const EXIT_FAILED: i32 = 1;
/// A hung server must never hang the CLI.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The agent table's column titles.
const TABLE_HEADER: [&str; 4] = ["AGENT", "STATUS", "MODEL", "KEY"];
/// Shown when the server answers with an error envelope.
const RESCUE_HINT: &str = "offline rescue: restore the backup beside the agent's config (<file>.before-any-converter) manually";

/// Entry point called from `main`: runs one `agents` subcommand, prints
/// everything itself, and returns the process exit code.
pub(crate) async fn run(command: AgentsCommand) -> i32 {
    // Only disconnect has an offline story (the backup is already on disk).
    let rescue = matches!(command, AgentsCommand::Disconnect { .. });
    match execute(command).await {
        Ok(()) => 0,
        Err(error) => {
            error.report(rescue);
            error.exit_code()
        }
    }
}

/// Whether the gateway at `base` answers `GET /health` (unauthenticated).
/// The daemon refuses to install beside a healthy gateway.
pub(crate) async fn server_reachable(base: &str) -> bool {
    call(base, None, "GET", "/health", None).await.is_ok()
}

async fn execute(command: AgentsCommand) -> Result<(), AgentCliError> {
    let base = server_base();
    let key = management_key()?;
    let key = Some(key.as_str());
    match command {
        AgentsCommand::List { json } => list(&base, key, json).await,
        AgentsCommand::Status => status(&base, key).await,
        AgentsCommand::Connect { id, model, home } => connect(&base, key, &id, model, home).await,
        AgentsCommand::Disconnect { id, home } => disconnect(&base, key, &id, home).await,
        AgentsCommand::Sync { id } => sync(&base, key, id).await,
        AgentsCommand::RotateKey { id, home } => rotate_key(&base, key, &id, home).await,
    }
}

/// Where the running server lives. `ANY_CONVERTER_URL` wins (full URL),
/// then the older `ANY_CONVERTER_ADDR` (host:port, as `accounts quotas`
/// reads it), then the server default.
fn server_base() -> String {
    if let Ok(url) = std::env::var("ANY_CONVERTER_URL") {
        let url = url.trim().trim_end_matches('/');
        if !url.is_empty() {
            return url.to_owned();
        }
    }
    match std::env::var("ANY_CONVERTER_ADDR") {
        Ok(addr) if !addr.trim().is_empty() => format!("http://{}", addr.trim()),
        _ => format!("http://127.0.0.1:{DEFAULT_PORT}"),
    }
}

/// The management key for `/v1/agents`. Remote installations set
/// `ANY_CONVERTER_MANAGEMENT_KEY`; the local installation reads its keychain.
fn management_key() -> Result<String, AgentCliError> {
    if let Ok(key) = std::env::var("ANY_CONVERTER_MANAGEMENT_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Ok(key.to_owned());
        }
    }
    let home = Home::discover().map_err(|error| AgentCliError::Plain(error.to_string()))?;
    ApiKeyStore::for_home(&home)
        .get_management()
        .map_err(|error| AgentCliError::Plain(error.to_string()))
}

// ── commands ─────────────────────────────────────────────────────────

async fn list(base: &str, key: Option<&str>, as_json: bool) -> Result<(), AgentCliError> {
    let list = call(base, key, "GET", "/v1/agents", None).await?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&list).map_err(AgentCliError::plain)?
        );
    } else {
        println!("{}", render_table(&list));
    }
    Ok(())
}

async fn status(base: &str, key: Option<&str>) -> Result<(), AgentCliError> {
    let list = call(base, key, "GET", "/v1/agents", None).await?;
    println!(
        "{}",
        serde_json::to_string(&list).map_err(AgentCliError::plain)?
    );
    Ok(())
}

async fn connect(
    base: &str,
    key: Option<&str>,
    id: &str,
    model: Option<String>,
    home: Option<String>,
) -> Result<(), AgentCliError> {
    let mut body = serde_json::Map::new();
    if let Some(model) = model {
        body.insert("model".to_owned(), json!(model));
    }
    if let Some(home) = home {
        body.insert("codexHome".to_owned(), json!(home));
    }
    let reply = call(
        base,
        key,
        "POST",
        &agent_path(id, "connect"),
        Some(&Value::Object(body)),
    )
    .await?;
    println!("connected {}", text(&reply, "agent", id));
    for file in strings(reply.get("files_written")) {
        println!("wrote {file}");
    }
    // Key count is metadata only; a failure here must not fail the connect.
    if let Ok(keys) = call(base, key, "GET", &agent_path(id, "keys"), None).await {
        let ids = key_ids(&keys);
        println!("gateway keys: {} ({})", ids.len(), ids.join(", "));
    }
    Ok(())
}

async fn disconnect(
    base: &str,
    key: Option<&str>,
    id: &str,
    home: Option<String>,
) -> Result<(), AgentCliError> {
    let _ = home; // the server owns the home it restores from
    let reply = call(base, key, "POST", &agent_path(id, "disconnect"), None).await?;
    println!("disconnected {}", text(&reply, "agent", id));
    for file in strings(reply.get("files_written")) {
        println!("restored {file}");
    }
    for file in strings(reply.get("kept_backup")) {
        println!("kept backup {file}");
    }
    for warning in strings(reply.get("warnings")) {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

async fn sync(base: &str, key: Option<&str>, id: Option<String>) -> Result<(), AgentCliError> {
    let ids = match id {
        Some(id) => vec![id],
        None => {
            let list = call(base, key, "GET", "/v1/agents", None).await?;
            connected_ids(&list)
        }
    };
    if ids.is_empty() {
        println!("no connected agents to sync");
        return Ok(());
    }
    let mut failed = false;
    for id in ids {
        match call(base, key, "POST", &agent_path(&id, "sync"), None).await {
            Ok(reply) => {
                let digest = text(&reply, "catalog_digest", "");
                if digest.is_empty() {
                    println!("synced {id}");
                } else {
                    println!("synced {id} (catalog {digest})");
                }
            }
            // A dead server ends the loop; per-agent API errors do not.
            Err(error) if error.is_unreachable() => return Err(error),
            Err(error) => {
                error.report(false);
                failed = true;
            }
        }
    }
    if failed {
        Err(AgentCliError::AlreadyReported)
    } else {
        Ok(())
    }
}

async fn rotate_key(
    base: &str,
    key: Option<&str>,
    id: &str,
    home: Option<String>,
) -> Result<(), AgentCliError> {
    let _ = home; // the server owns the home it rewrites
    let reply = call(base, key, "POST", &agent_path(id, "key/rotate"), None).await?;
    println!("rotated key for {id}: {}", text(&reply, "key_id", "?"));
    Ok(())
}

fn agent_path(id: &str, action: &str) -> String {
    format!("/v1/agents/{id}/{action}")
}

// ── rendering (pure) ─────────────────────────────────────────────────

/// Render `GET /v1/agents` as an aligned table (id, status, model, key hint).
pub(crate) fn render_table(list: &Value) -> String {
    let rows: Vec<Vec<String>> = agents(list)
        .iter()
        .map(|agent| {
            vec![
                dash(text(agent, "id", "")),
                dash(text(agent, "status", "")),
                dash(text(agent, "model", "")),
                key_hint(text(agent, "key_id", "")),
            ]
        })
        .collect();
    if rows.is_empty() {
        return "no agents detected".to_owned();
    }
    let mut widths: Vec<usize> = TABLE_HEADER.iter().map(|title| title.len()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let header: Vec<String> = TABLE_HEADER
        .iter()
        .map(|title| (*title).to_owned())
        .collect();
    let mut lines = vec![join_row(&header, &widths)];
    for row in &rows {
        lines.push(join_row(row, &widths));
    }
    lines.join("\n")
}

fn join_row(cells: &[String], widths: &[usize]) -> String {
    let padded: Vec<String> = cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| format!("{cell:<width$}"))
        .collect();
    padded.join("  ").trim_end().to_owned()
}

/// The last four characters of a key id — enough to tell keys apart,
/// never enough to use one.
pub(crate) fn key_hint(key_id: &str) -> String {
    if key_id.is_empty() {
        return "-".to_owned();
    }
    let chars: Vec<char> = key_id.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    if chars.len() > 4 {
        format!("…{tail}")
    } else {
        tail
    }
}

fn dash(value: &str) -> String {
    if value.is_empty() {
        "-".to_owned()
    } else {
        value.to_owned()
    }
}

fn agents(list: &Value) -> &[Value] {
    list.get("agents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn connected_ids(list: &Value) -> Vec<String> {
    agents(list)
        .iter()
        .filter(|agent| text(agent, "status", "") == "connected")
        .map(|agent| text(agent, "id", "").to_owned())
        .filter(|id| !id.is_empty())
        .collect()
}

fn key_ids(keys: &Value) -> Vec<String> {
    strings(keys.get("keys")).map(|id| id.to_owned()).collect()
}

fn text<'a>(value: &'a Value, name: &str, fallback: &'a str) -> &'a str {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or(fallback)
}

/// Each entry of `value` as a string; entries may be objects carrying `id`
/// or `path`, so both shapes render.
fn strings(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| match entry {
            Value::String(text) => Some(text.as_str()),
            other => other
                .get("id")
                .or_else(|| other.get("path"))
                .and_then(Value::as_str),
        })
}

// ── errors ───────────────────────────────────────────────────────────

/// The uniform `{ "error": { "code", "message", "hint" } }` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiErrorBody {
    /// Stable machine-readable code, e.g. `agent_absent`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Optional action hint.
    pub hint: Option<String>,
}

impl ApiErrorBody {
    /// Parse the error envelope; `None` when `value` carries no error.
    pub(crate) fn parse(value: &Value) -> Option<Self> {
        let error = value.get("error")?;
        if !error.is_object() {
            return None;
        }
        let code = error.get("code").and_then(Value::as_str).unwrap_or("");
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        if code.is_empty() && message.is_empty() {
            return None;
        }
        Some(Self {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: error
                .get("hint")
                .and_then(Value::as_str)
                .filter(|hint| !hint.is_empty())
                .map(str::to_owned),
        })
    }

    /// The message, or the code when the server sent no message.
    fn text(&self) -> &str {
        if self.message.is_empty() {
            &self.code
        } else {
            &self.message
        }
    }
}

/// Everything one `agents` invocation can fail with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentCliError {
    /// The server is not listening (or the connection broke).
    Unreachable {
        /// Base URL that was dialled (printed to the user).
        url: String,
        /// The underlying OS error, for debugging.
        detail: String,
    },
    /// The server answered with the API error envelope.
    Api(ApiErrorBody),
    /// Anything else worth one `error:` line.
    Plain(String),
    /// Per-agent failures already reported by [`sync`].
    AlreadyReported,
}

impl AgentCliError {
    fn plain(error: serde_json::Error) -> Self {
        Self::Plain(error.to_string())
    }

    /// `true` when the server could not be reached at all.
    fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }

    fn exit_code(&self) -> i32 {
        if self.is_unreachable() {
            EXIT_UNREACHABLE
        } else {
            EXIT_FAILED
        }
    }

    /// Print the error. `rescue` adds the offline disconnect instructions.
    fn report(&self, rescue: bool) {
        match self {
            Self::Api(error) => {
                eprintln!("error: {}", error.text());
                if let Some(hint) = &error.hint {
                    eprintln!("hint: {hint}");
                }
            }
            Self::Unreachable { url, detail } => {
                eprintln!("server unreachable at {url} — start it with: any-converter serve");
                if rescue {
                    eprintln!("{RESCUE_HINT}");
                }
                eprintln!("cause: {detail}");
            }
            Self::Plain(message) => eprintln!("error: {message}"),
            Self::AlreadyReported => {}
        }
    }
}

// ── HTTP/1.1 over a TcpStream ────────────────────────────────────────

async fn call(
    base: &str,
    key: Option<&str>,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, AgentCliError> {
    let authority = authority(base)?;
    let payload = match body {
        Some(body) => serde_json::to_vec(body).map_err(AgentCliError::plain)?,
        None => Vec::new(),
    };
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nAccept: application/json\r\n"
    );
    if let Some(key) = key {
        request.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    request.push_str("\r\n");

    let unreachable = |detail: String| AgentCliError::Unreachable {
        url: base.to_owned(),
        detail,
    };
    let exchange = async {
        let mut stream = TcpStream::connect(&authority)
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        stream
            .write_all(&payload)
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        let mut reply = Vec::new();
        stream
            .read_to_end(&mut reply)
            .await
            .map_err(|error| unreachable(error.to_string()))?;
        Ok::<Vec<u8>, AgentCliError>(reply)
    };
    let reply = match tokio::time::timeout(TIMEOUT, exchange).await {
        Ok(reply) => reply?,
        Err(_) => {
            return Err(unreachable(format!(
                "no reply within {}s",
                TIMEOUT.as_secs()
            )));
        }
    };
    let (status, body) = split_reply(&reply).ok_or_else(|| {
        AgentCliError::Plain(format!("the server at {base} sent no valid HTTP reply"))
    })?;
    let parsed: Value = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Err(match status {
                200..=299 => AgentCliError::Plain(format!("unexpected reply: {body}")),
                _ => AgentCliError::Plain(format!("HTTP {status}: {body}")),
            });
        }
    };
    if (200..300).contains(&status) {
        return Ok(parsed);
    }
    Err(match ApiErrorBody::parse(&parsed) {
        Some(error) => AgentCliError::Api(error),
        None => AgentCliError::Plain(format!("HTTP {status}: {body}")),
    })
}

/// `host:port` of an `http://` base URL.
fn authority(base: &str) -> Result<String, AgentCliError> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| AgentCliError::Plain(format!("{base} is not an http:// URL")))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(AgentCliError::Plain(format!("{base} names no server")));
    }
    Ok(authority.to_owned())
}

/// Status code and body of a raw HTTP/1.1 reply.
fn split_reply(reply: &[u8]) -> Option<(u16, &str)> {
    let text = std::str::from_utf8(reply).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split_whitespace().nth(1)?;
    Some((status.parse().ok()?, body.trim()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn agent(id: &str, status: &str, model: Option<&str>, key: Option<&str>) -> Value {
        let mut agent = json!({ "id": id, "status": status });
        if let Some(model) = model {
            agent["model"] = json!(model);
        }
        if let Some(key) = key {
            agent["key_id"] = json!(key);
        }
        agent
    }

    #[test]
    fn table_lists_agent_status_model_and_key_hint() {
        let list = json!({ "agents": [
            agent("codex", "connected", Some("gpt-5.1"), Some("ak_codex_0001")),
            agent("claude", "absent", None, None),
        ]});
        let table = render_table(&list);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("AGENT"), "{table}");
        assert!(lines[1].starts_with("codex"), "{table}");
        assert!(lines[1].contains("connected"));
        assert!(lines[1].contains("gpt-5.1"));
        assert!(lines[1].contains("…0001"), "{table}");
        // Missing model/key render as placeholders, never as empty cells.
        assert!(lines[2].contains("absent"));
        let placeholders = lines[2]
            .split_whitespace()
            .filter(|cell| *cell == "-")
            .count();
        assert_eq!(placeholders, 2, "{table}");
    }

    #[test]
    fn table_says_so_when_no_agents_are_known() {
        assert_eq!(render_table(&json!({ "agents": [] })), "no agents detected");
        assert_eq!(render_table(&json!({})), "no agents detected");
    }

    #[test]
    fn key_hint_keeps_only_the_tail() {
        assert_eq!(key_hint("ak_codex_0001"), "…0001");
        assert_eq!(key_hint("0001"), "0001");
        assert_eq!(key_hint("ab"), "ab");
        assert_eq!(key_hint(""), "-");
    }

    #[test]
    fn api_error_body_maps_message_and_hint() {
        let body = json!({ "error": {
            "code": "agent_absent",
            "message": "agent codex is not installed",
            "hint": "install codex first",
            "agent": "codex",
        }});
        let error = ApiErrorBody::parse(&body).unwrap();
        assert_eq!(error.code, "agent_absent");
        assert_eq!(error.text(), "agent codex is not installed");
        assert_eq!(error.hint.as_deref(), Some("install codex first"));
    }

    #[test]
    fn api_error_body_ignores_success_payloads() {
        assert!(ApiErrorBody::parse(&json!({ "agents": [] })).is_none());
        assert!(ApiErrorBody::parse(&json!({ "error": "boom" })).is_none());
        assert!(ApiErrorBody::parse(&json!({ "error": {} })).is_none());
        // An empty hint must not print a dangling `hint:` line.
        let error =
            ApiErrorBody::parse(&json!({ "error": { "message": "nope", "hint": "" } })).unwrap();
        assert!(error.hint.is_none());
    }

    #[test]
    fn exit_codes_split_unreachable_from_api_errors() {
        let unreachable = AgentCliError::Unreachable {
            url: "http://127.0.0.1:8080".to_owned(),
            detail: "connection refused".to_owned(),
        };
        assert_eq!(unreachable.exit_code(), 2);
        assert!(unreachable.is_unreachable());
        let api = AgentCliError::Api(ApiErrorBody {
            code: "model_not_found".to_owned(),
            message: "model x is not in the catalog".to_owned(),
            hint: None,
        });
        assert_eq!(api.exit_code(), 1);
        assert!(!api.is_unreachable());
        assert_eq!(AgentCliError::AlreadyReported.exit_code(), 1);
    }

    #[test]
    fn reply_split_reads_status_and_body() {
        let reply = b"HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(split_reply(reply), Some((404, "{}")));
        assert_eq!(split_reply(b"garbage"), None);
        assert_eq!(split_reply(b"HTTP/1.1 nope\r\n\r\n{}"), None);
    }

    #[test]
    fn authority_rejects_non_http_urls() {
        assert_eq!(
            authority("http://127.0.0.1:8080").unwrap(),
            "127.0.0.1:8080"
        );
        assert_eq!(
            authority("http://example.test/api").unwrap(),
            "example.test"
        );
        assert!(authority("https://example.test").is_err());
        assert!(authority("http://").is_err());
    }

    #[test]
    fn sync_targets_only_connected_agents() {
        let list = json!({ "agents": [
            agent("codex", "connected", Some("gpt-5.1"), None),
            agent("claude", "disconnected", None, None),
            agent("pi", "drifted", None, None),
            agent("gemini", "connected", None, None),
        ]});
        assert_eq!(connected_ids(&list), vec!["codex", "gemini"]);
        assert!(connected_ids(&json!({})).is_empty());
    }

    #[test]
    fn key_ids_render_from_the_keys_endpoint() {
        let keys = json!({ "agent": "codex", "keys": [
            { "id": "ak_codex_0001", "created_at": 1 },
            { "id": "ak_codex_0002", "created_at": 2 },
        ]});
        assert_eq!(key_ids(&keys), vec!["ak_codex_0001", "ak_codex_0002"]);
    }
}

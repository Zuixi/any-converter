//! Contract replay: sanitized client→gateway→upstream fixtures from
//! `tests/contracts/**` replayed through the real router against a fake upstream.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use any_converter_core::convert::Format;
use any_converter_server::config::{
    ModelRouteConfig, PassThroughHeadersConfig, ProviderConfig, RouteStrategy, ServerConfig,
    ServerSettings,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::Router;
use axum::body::Body;
use axum::extract::Request as AxumRequest;
use axum::http::{Request, StatusCode, header};
use axum::routing::any;
use pretty_assertions::Comparison;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tower::ServiceExt;

/// Path the gateway must call upstream, per upstream format.
fn upstream_path(format: Format) -> &'static str {
    match format {
        Format::Claude => "/v1/messages",
        Format::OpenAIChat => "/v1/chat/completions",
        Format::OpenAIResponses => "/v1/responses",
        Format::Gemini => "/v1beta/models/{model}:generateContent",
    }
}

#[derive(Debug, Deserialize)]
struct Meta {
    name: String,
    tool: String,
    client_format: String,
    upstream_format: String,
    path: String,
    method: String,
    mode: String,
    stream: bool,
    #[serde(default)]
    normalize: Vec<String>,
}

type Captured = Arc<Mutex<Option<(String, String)>>>;

fn contracts_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/contracts")
}

/// Case directories are the ones holding a `meta.toml`.
fn discover_cases(dir: &Path, out: &mut Vec<PathBuf>) {
    if dir.join("meta.toml").is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    children.sort();
    for child in children {
        discover_cases(&child, out);
    }
}

fn read(dir: &Path, file: &str, fixture: &str) -> String {
    let path = dir.join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{fixture}: cannot read {}: {e}", path.display()))
}

fn read_json(dir: &Path, file: &str, fixture: &str) -> Value {
    let text = read(dir, file, fixture);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{fixture}: {} is not JSON: {e}", dir.join(file).display()))
}

fn parse_format(raw: &str, fixture: &str, field: &str) -> Format {
    Format::parse(raw)
        .unwrap_or_else(|e| panic!("{fixture}: {field} = \"{raw}\" is not a known format: {e}"))
}

/// Replace the values at `normalize` dot-paths with `"<n>"` so run-to-run
/// fields (ids, timestamps, created_at) do not fail the comparison.
fn normalize(mut value: Value, keys: &[String]) -> Value {
    fn walk(value: &mut Value, prefix: &str, keys: &[String]) {
        match value {
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    if keys.contains(&path) {
                        *child = Value::String("<n>".to_string());
                    } else {
                        walk(child, &path, keys);
                    }
                }
            }
            // array indices are not part of the path, so `output.id` hits every item
            Value::Array(items) => {
                for item in items.iter_mut() {
                    walk(item, prefix, keys);
                }
            }
            _ => {}
        }
    }
    walk(&mut value, "", keys);
    value
}

fn truncate(text: &str) -> String {
    text.chars().take(1200).collect()
}

/// Short diff over the top-level keys, plus a per-key pretty comparison.
fn diff_keys(label: &str, actual: &Value, expected: &Value) -> String {
    let keys: Vec<String> = match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            let mut keys: Vec<String> = a.keys().chain(e.keys()).cloned().collect();
            keys.sort();
            keys.dedup();
            keys
        }
        _ => Vec::new(),
    };
    if keys.is_empty() {
        return format!(
            "{label}: not both objects\n{}",
            Comparison::new(&pretty(expected), &pretty(actual))
        );
    }
    let mut out = format!("{label}: mismatched top-level keys\n");
    for key in keys {
        let left = actual.get(&key).unwrap_or(&Value::Null);
        let right = expected.get(&key).unwrap_or(&Value::Null);
        if left == right {
            continue;
        }
        out.push_str(&format!(
            "--- {key}\n{}",
            Comparison::new(&truncate(&pretty(right)), &truncate(&pretty(left)))
        ));
    }
    if out.ends_with("mismatched top-level keys\n") {
        return format!(
            "{label}: values differ\n{}",
            Comparison::new(&pretty(expected), &pretty(actual))
        );
    }
    out
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn assert_json_eq(fixture: &str, what: &str, actual: &Value, expected: &Value) {
    if actual != expected {
        panic!("{fixture}: {what}\n{}", diff_keys(what, actual, expected));
    }
}

/// `(event, data)` pairs of an SSE body, in order.
fn sse_events(body: &str) -> Vec<(Option<String>, Value)> {
    body.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .filter_map(|block| {
            let mut event = None;
            let mut data: Option<String> = None;
            for line in block.lines() {
                if let Some(name) = line.strip_prefix("event: ") {
                    event = Some(name.trim().to_string());
                } else if let Some(payload) = line.strip_prefix("data: ") {
                    data = Some(match data {
                        Some(prev) => format!("{prev}\n{payload}"),
                        None => payload.to_string(),
                    });
                }
            }
            let data = data?;
            // `[DONE]` is not JSON; keep it as a bare string so identity stays comparable
            let value = serde_json::from_str(&data).unwrap_or(Value::String(data));
            Some((event, value))
        })
        .collect()
}

async fn spawn_upstream(
    response: String,
    content_type: &'static str,
    captured: Captured,
) -> String {
    let app = Router::new().fallback(any(move |req: AxumRequest| {
        let captured = captured.clone();
        let response = response.clone();
        async move {
            let path = req.uri().path().to_string();
            let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                .await
                .unwrap();
            *captured.lock().unwrap() = Some((path, String::from_utf8_lossy(&bytes).into_owned()));
            ([(header::CONTENT_TYPE, content_type)], response)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn gateway(format: Format, base_url: String) -> Router {
    let config = ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
            pass_through_headers: PassThroughHeadersConfig::default(),
        },
        providers: vec![ProviderConfig {
            name: "upstream".into(),
            format,
            base_url,
            api_key: "contract-fixture-key".into(),
            account: None,
            accounts: Vec::new(),
            api: None,
            wire: None,
            // empty model_map = passthrough of the client's model name
            model_map: Default::default(),
            endpoints: Default::default(),
            auth: Default::default(),
            plugin: None,
        }],
        model_routes: vec![ModelRouteConfig {
            pattern: "*".into(),
            provider: Some("upstream".into()),
            providers: vec![],
            upstream_model: None,
            strategy: RouteStrategy::Priority,
        }],
        routes: vec![],
        model_metadata: Default::default(),
        logging: Default::default(),
    };
    create_router_with_state(Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        accounts: any_converter_server::credentials::AccountResolver::with_default_store(
            Client::new(),
        ),
        usage_logger: None,
        request_logger: None,
        copilot: Arc::new(
            any_converter_server::credentials::copilot::CopilotSessions::new(Client::new()),
        ),
        hosts: Arc::new(any_converter_server::plugins::ManagedHosts::with_store(
            Arc::new(any_converter_server::plugins::PluginAuthStore::new(
                std::env::temp_dir().join("any-converter-test-plugin-auth.json"),
            )),
        )),
    }))
}

#[tokio::test]
async fn all_contract_fixtures_pass() {
    let root = contracts_root();
    let mut cases = Vec::new();
    discover_cases(&root, &mut cases);
    assert!(
        !cases.is_empty(),
        "expected fixtures under {}",
        root.display()
    );
    for dir in cases {
        run_case(&dir).await;
    }
}

async fn run_case(dir: &Path) {
    let meta_file = dir.join("meta.toml");
    let meta: Meta = toml::from_str(&std::fs::read_to_string(&meta_file).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", meta_file.display()));
    let fixture = format!("{}/{}", meta.tool, meta.name);
    assert_eq!(
        meta.name,
        dir.file_name().unwrap().to_string_lossy(),
        "{fixture}: meta.name must equal the case directory name"
    );
    assert!(
        matches!(meta.mode.as_str(), "identity" | "convert"),
        "{fixture}: unknown mode \"{}\"",
        meta.mode
    );
    assert_eq!(meta.method, "POST", "{fixture}: only POST is supported");

    let client_format = parse_format(&meta.client_format, &fixture, "client_format");
    let upstream_format = parse_format(&meta.upstream_format, &fixture, "upstream_format");
    if meta.mode == "identity" {
        assert_eq!(
            client_format, upstream_format,
            "{fixture}: mode = identity needs matching client/upstream formats"
        );
    }

    let (upstream_file, content_type) = if meta.stream {
        ("upstream.response.sse", "text/event-stream")
    } else {
        ("upstream.response.json", "application/json")
    };
    let upstream_response = read(dir, upstream_file, &fixture);

    let captured: Captured = Arc::new(Mutex::new(None));
    let base_url = spawn_upstream(upstream_response, content_type, captured.clone()).await;

    let response = gateway(upstream_format, base_url)
        .oneshot(
            Request::builder()
                .uri(&meta.path)
                .method(meta.method.as_str())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(read(dir, "client.request.json", &fixture)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let client_body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let client_body = String::from_utf8_lossy(&client_body).into_owned();

    let (seen_path, seen_body) = captured
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| panic!("{fixture}: upstream was never called"));

    // 1) the gateway hit the right upstream endpoint
    assert_eq!(
        seen_path,
        upstream_path(upstream_format),
        "{fixture}: wrong upstream path"
    );

    // 2) the gateway sent the converted request body
    let actual_request: Value = serde_json::from_str(&seen_body)
        .unwrap_or_else(|e| panic!("{fixture}: upstream request is not JSON ({e}): {seen_body}"));
    let expected_request = normalize(
        read_json(dir, "upstream.request.json", &fixture),
        &meta.normalize,
    );
    assert_json_eq(
        &fixture,
        "upstream.request.json",
        &normalize(actual_request, &meta.normalize),
        &expected_request,
    );

    // 3) the client got the expected answer
    assert_eq!(
        status,
        StatusCode::OK,
        "{fixture}: client status, body {client_body}"
    );
    if meta.stream {
        let expected_events: Vec<(Option<String>, Value)> =
            sse_events(&read(dir, "expected.client.response.sse", &fixture))
                .into_iter()
                .map(|(event, data)| (event, normalize(data, &meta.normalize)))
                .collect();
        let actual_events: Vec<(Option<String>, Value)> = sse_events(&client_body)
            .into_iter()
            .map(|(event, data)| (event, normalize(data, &meta.normalize)))
            .collect();
        assert_eq!(
            actual_events.len(),
            expected_events.len(),
            "{fixture}: expected {} SSE events, got {} in:\n{client_body}",
            expected_events.len(),
            actual_events.len()
        );
        for (index, (actual, expected)) in actual_events.iter().zip(&expected_events).enumerate() {
            assert_eq!(actual.0, expected.0, "{fixture}: SSE event #{index} name");
            assert_json_eq(
                &fixture,
                &format!("expected.client.response.sse event #{index}"),
                &actual.1,
                &expected.1,
            );
        }
    } else {
        let actual: Value = serde_json::from_str(&client_body)
            .unwrap_or_else(|e| panic!("{fixture}: client body is not JSON ({e}): {client_body}"));
        let expected = normalize(
            read_json(dir, "expected.client.response.json", &fixture),
            &meta.normalize,
        );
        assert_json_eq(
            &fixture,
            "expected.client.response.json",
            &normalize(actual, &meta.normalize),
            &expected,
        );
    }
}

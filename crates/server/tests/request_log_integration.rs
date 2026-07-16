#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use any_converter_core::convert::Format;
use any_converter_server::config::{
    LoggingConfig, ProviderConfig, RequestLogConfig, RouteConfig, ServerConfig, ServerSettings,
};
use any_converter_server::handlers::AppState;
use any_converter_server::router::create_router_with_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::{Router, routing::post};
use futures_util::stream;
use reqwest::Client;
use serde_json::json;
use std::convert::Infallible;
use tower::ServiceExt;

fn oai_provider(name: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        format: Format::OpenAIChat,
        base_url: base_url.into(),
        api_key: "upstream-key".into(),
        model_map: [("*".into(), "gpt-4".into())].into(),
        endpoints: Default::default(),
        auth: Default::default(),
    }
}

fn test_config(base_url: &str, log_path: PathBuf) -> ServerConfig {
    ServerConfig {
        server: ServerSettings {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: None,
        },
        providers: vec![oai_provider("openai", base_url)],
        model_routes: vec![],
        routes: vec![RouteConfig {
            client_format: Format::Claude,
            provider: "openai".into(),
        }],
        model_metadata: Default::default(),
        logging: LoggingConfig {
            dir: Some(log_path.to_string_lossy().to_string()),
            request_log: RequestLogConfig {
                enabled: true,
                max_capture_bytes: 1024 * 1024,
            },
            ..Default::default()
        },
    }
}

fn read_latest_requests_jsonl(log_path: &PathBuf) -> Vec<serde_json::Value> {
    let entries: Vec<PathBuf> = std::fs::read_dir(log_path)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("requests."))
        })
        .collect();

    let latest = entries
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap())
        .expect("no requests log file found");

    std::fs::read_to_string(latest)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn request_log_non_streaming_captures_body_usage_latency() {
    let log_path = tempfile::tempdir().unwrap();
    let log_path = log_path.path().to_path_buf();

    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            axum::Json(json!({
                "id": "ok",
                "object": "chat.completion",
                "created": 1700000000,
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "Hello!" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11 }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });

    let base = format!("http://{addr}");
    let config = test_config(&base, log_path.clone());
    let state = Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        usage_logger: None,
        request_logger: any_converter_server::request_log::create_request_logger(
            Some(&log_path.to_string_lossy()),
            &RequestLogConfig {
                enabled: true,
                max_capture_bytes: 1024 * 1024,
            },
        ),
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer user-secret")
        .body(Body::from(
            r#"{"model":"claude-3","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_text = String::from_utf8_lossy(&body_bytes);
    assert!(
        status == StatusCode::OK,
        "unexpected status {status}: {body_text}"
    );

    // Give the background writer a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_latest_requests_jsonl(&log_path);
    assert_eq!(records.len(), 1);
    let record = &records[0];

    assert_eq!(record["streaming"], false);
    assert_eq!(record["method"], "POST");
    assert_eq!(record["path"], "/v1/messages");
    assert_eq!(record["response_status"], 200);
    assert!(record["latency_ms"].as_u64().is_some());

    let request_body: serde_json::Value =
        serde_json::from_str(record["request_body"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(request_body["model"], "claude-3");

    let upstream_body: serde_json::Value =
        serde_json::from_str(record["upstream_request_body"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(upstream_body["model"], "gpt-4");

    assert_eq!(record["usage"]["input_tokens"], 10);
    assert_eq!(record["usage"]["output_tokens"], 1);
    assert!(!record["truncated"].as_bool().unwrap());
}

#[tokio::test]
async fn request_log_streaming_captures_ttfb_and_sse_lines() {
    let log_path = tempfile::tempdir().unwrap();
    let log_path = log_path.path().to_path_buf();

    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Sse::new(stream::iter(vec![
            Ok::<Event, Infallible>(Event::default().data(
                json!({
                    "id": "c1",
                    "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {"content": "Hello"}, "finish_reason": null}]
                })
                .to_string(),
            )),
            Ok::<Event, Infallible>(Event::default().data(
                json!({
                    "id": "c1",
                    "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
                })
                .to_string(),
            )),
            Ok::<Event, Infallible>(Event::default().data("[DONE]")),
        ]))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async { axum::serve(listener, upstream).await.unwrap() });

    let base = format!("http://{addr}");
    let config = test_config(&base, log_path.clone());
    let state = Arc::new(AppState {
        config,
        client: Client::builder().no_proxy().build().unwrap(),
        usage_logger: None,
        request_logger: any_converter_server::request_log::create_request_logger(
            Some(&log_path.to_string_lossy()),
            &RequestLogConfig {
                enabled: true,
                max_capture_bytes: 1024 * 1024,
            },
        ),
    });
    let app = create_router_with_state(state);

    let request = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-3","max_tokens":100,"stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_text = String::from_utf8_lossy(&body_bytes);
    assert!(
        status == StatusCode::OK,
        "unexpected status {status}: {body_text}"
    );

    // Consume the stream so the spawned task can finish.
    let _body = body_bytes;

    // Give the background writer a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_latest_requests_jsonl(&log_path);
    assert_eq!(records.len(), 1);
    let record = &records[0];

    assert_eq!(record["streaming"], true);
    assert!(record["latency_ms"].as_u64().is_some());
    let lines = record["response_body"]["lines"].as_array().unwrap();
    assert!(!lines.is_empty());
    assert!(!record["truncated"].as_bool().unwrap());
}

//! Golden matrix: pins today's pairwise conversion behavior as snapshots,
//! so the IR hub flip (docs/design/ir-hub-conversion.md §7 P0) has a
//! byte-level reference. Regenerate with `GOLDEN_UPDATE=1 cargo test --test
//! golden_matrix`; the diff of `tests/golden/` is then the triage list.
//!
//! Generated ids (uuids) and timestamps are normalized away; streams are
//! covered by stream_matrix and pinned there, not here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use any_converter_core::convert::{Format, convert_request, convert_response};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const FORMATS: [(&str, Format); 4] = [
    ("openai_chat", Format::OpenAIChat),
    ("claude", Format::Claude),
    ("openai_resp", Format::OpenAIResponses),
    ("gemini", Format::Gemini),
];

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes()[8] == b'-'
        && s.as_bytes()[13] == b'-'
        && s.as_bytes()[18] == b'-'
        && s.as_bytes()[23] == b'-'
        && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Replace generated values (uuid ids, `created` timestamps) so snapshots
/// are deterministic run to run.
fn normalize(v: &mut Value) {
    match v {
        Value::String(s) => {
            if looks_like_uuid(s) {
                *s = "<uuid>".into();
            } else if s.len() > 36 {
                // prefixed generated ids: chatcmpl-<uuid>, msg_<uuid>, resp_<uuid>
                let (head, tail) = s.split_at(s.len() - 36);
                if looks_like_uuid(tail) {
                    *s = format!("{head}<uuid>");
                }
            }
        }
        Value::Object(map) => {
            for stamp in ["created", "created_at"] {
                if let Some(Value::Number(n)) = map.get(stamp) {
                    if n.is_u64() {
                        map.insert(stamp.into(), Value::from(0u64));
                    }
                }
            }
            for val in map.values_mut() {
                normalize(val);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize(item);
            }
        }
        _ => {}
    }
}

fn fixtures_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn convert_and_pin(
    kind: &str,
    from: (&str, Format),
    to: (&str, Format),
    fixture: &str,
    bytes: &[u8],
) {
    let result = if kind == "request" {
        convert_request(bytes, from.1, to.1)
    } else {
        convert_response(bytes, from.1, to.1)
    };
    let body = match result {
        Ok(out) => {
            let mut v: Value = serde_json::from_slice(&out).unwrap_or_else(|e| {
                panic!("{kind} {}->{} {}: bad JSON out: {e}", from.0, to.0, fixture)
            });
            normalize(&mut v);
            format!("{}\n", serde_json::to_string_pretty(&v).unwrap())
        }
        Err(e) => format!("ERROR: {e}\n"),
    };
    let path = golden_dir().join(format!("{kind}_{}_to_{}_{fixture}", from.0, to.0));
    let update = std::env::var("GOLDEN_UPDATE").is_ok();
    if update {
        fs::create_dir_all(golden_dir()).unwrap();
        fs::write(&path, &body).unwrap();
        return;
    }
    let want = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {} — run GOLDEN_UPDATE=1", path.display()));
    assert_eq!(
        want, body,
        "behavior drift: {kind} {} -> {} on {fixture}",
        from.0, to.0
    );
}

#[test]
fn golden_matrix() {
    for from in FORMATS {
        let dir = fixtures_dir(from.0);
        let mut fixtures: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().into_string().ok()?;
                name.strip_suffix(".json").map(str::to_string)
            })
            .collect();
        fixtures.sort();
        for fixture in fixtures {
            let bytes = fs::read(dir.join(format!("{fixture}.json"))).unwrap();
            let kind = if fixture.starts_with("request_") {
                "request"
            } else if fixture.starts_with("response_") {
                "response"
            } else {
                continue;
            };
            for to in FORMATS {
                if to.0 == from.0 {
                    continue;
                }
                convert_and_pin(kind, from, to, &fixture, &bytes);
            }
        }
    }
}

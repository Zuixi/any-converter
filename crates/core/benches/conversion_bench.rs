#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::useless_format
)]

use std::path::PathBuf;

use any_converter_core::convert::{Format, convert_request, convert_response};
use any_converter_core::sse::parse_sse_block;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_fixture(format_dir: &str, name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(format_dir).join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn load_sse_blocks(format_dir: &str, name: &str) -> Vec<String> {
    let raw = String::from_utf8(load_fixture(format_dir, name)).expect("fixture is valid UTF-8");
    raw.split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn format_dir_name(f: Format) -> &'static str {
    match f {
        Format::OpenAIChat => "openai_chat",
        Format::Claude => "claude",
        Format::OpenAIResponses => "openai_resp",
        Format::Gemini => "gemini",
    }
}

fn all_format_pairs() -> Vec<(Format, Format)> {
    let formats = [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ];
    let mut pairs = Vec::new();
    for &from in &formats {
        for &to in &formats {
            if from != to {
                pairs.push((from, to));
            }
        }
    }
    pairs
}

fn bench_convert_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("convert_request");
    for (from, to) in all_format_pairs() {
        let input = load_fixture(format_dir_name(from), "request_simple.json");
        let bench_name = format!("{}_to_{}", from.as_str(), to.as_str());
        group.bench_function(bench_name, |b| {
            b.iter(|| convert_request(black_box(&input), from, to).unwrap());
        });
    }
    group.finish();
}

fn bench_convert_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("convert_response");
    for (from, to) in all_format_pairs() {
        let input = load_fixture(format_dir_name(from), "response_simple.json");
        let bench_name = format!("{}_to_{}", from.as_str(), to.as_str());
        group.bench_function(bench_name, |b| {
            b.iter(|| convert_response(black_box(&input), from, to).unwrap());
        });
    }
    group.finish();
}

fn bench_sse_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("sse_parse");
    let formats = [
        Format::OpenAIChat,
        Format::Claude,
        Format::OpenAIResponses,
        Format::Gemini,
    ];
    for format in formats {
        let blocks = load_sse_blocks(format_dir_name(format), "stream_text.sse");
        let bench_name = format!("{}", format.as_str());
        group.bench_function(bench_name, |b| {
            b.iter(|| {
                for block in &blocks {
                    black_box(parse_sse_block(block));
                }
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_convert_request,
    bench_convert_response,
    bench_sse_parse
);
criterion_main!(benches);

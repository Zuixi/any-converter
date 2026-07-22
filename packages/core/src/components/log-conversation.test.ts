import type { RequestLogRecord, TraceMessage } from "@any-converter/shared";

import { buildSessionSummaries } from "./log-conversation";

function record(requestId: string, timestamp: string, messages: TraceMessage[]): RequestLogRecord {
  const emptyTrace = { messages: [], tool_definitions: [], tool_calls: [], tool_results: [] };
  return {
    request_id: requestId,
    timestamp,
    client_format: "openai_chat",
    client_id: "codex-cli/1.0",
    provider: "openai",
    client_model: "gpt-5",
    upstream_model: "gpt-5",
    streaming: true,
    method: "POST",
    path: "/v1/chat/completions",
    request_body: null,
    upstream_request_body: null,
    response_status: 200,
    response_body: { text: "{}" },
    latency_ms: 10,
    usage: { input_tokens: 1, output_tokens: 1 },
    trace: {
      client: { ...emptyTrace, messages },
      upstream: emptyTrace,
      response: emptyTrace,
    },
    truncated: false,
  };
}

function assertEqual(actual: unknown, expected: unknown, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

const first = record("request-1", "2026-07-22T00:00:00Z", [
  { role: "system", content_preview: "Be concise" },
  { role: "user", content_preview: "First question" },
]);
const continuation = record("request-2", "2026-07-22T00:01:00Z", [
  { role: "system", content_preview: "Be concise" },
  { role: "user", content_preview: "First question" },
  { role: "assistant", content_preview: "First answer" },
  { role: "user", content_preview: "Follow-up question" },
]);

const sessions = buildSessionSummaries([continuation, first]);
assertEqual(sessions.length, 1, "strictly extended histories should share a session");
assertEqual(sessions[0]?.requestCount, 2, "the inferred session should contain both requests");

const duplicateStart = record("request-3", "2026-07-22T00:02:00Z", first.trace?.client.messages ?? []);
const ambiguousContinuation = record("request-4", "2026-07-22T00:03:00Z", continuation.trace?.client.messages ?? []);
const ambiguousSessions = buildSessionSummaries([first, duplicateStart, ambiguousContinuation]);
assertEqual(ambiguousSessions.length, 3, "ambiguous histories should remain separate sessions");

"use client";

import { useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { RequestLogRecord } from "@any-converter/shared";
import { Badge, Button, Input, Label, cn } from "@any-converter/ui";
import { formatBytes, formatDuration, formatTimestamp } from "@any-converter/shared";

import {
  buildConversationDetail,
  buildRequestSummaries,
  effectiveUsage,
  responseBodyToText,
  sortRecordsAscending,
  totalUsage,
  type ConversationEntry,
  type ConversationRequestSummary,
} from "./log-conversation";

interface LogTableProps {
  records: RequestLogRecord[];
}

export function LogTable({ records }: LogTableProps) {
  const [selectedId, setSelectedId] = useState<string | null>(records[0]?.request_id ?? null);
  const [filter, setFilter] = useState("");
  const [showRaw, setShowRaw] = useState(false);

  const summaries = useMemo(() => buildRequestSummaries(records), [records]);
  const totals = useMemo(() => totalUsage(records), [records]);
  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) {
      return summaries;
    }
    return summaries.filter(({ record, title, subtitle }) =>
      [title, subtitle, record.request_id, record.client_model, record.upstream_model, record.path]
        .join(" ")
        .toLowerCase()
        .includes(q),
    );
  }, [filter, summaries]);

  const selected = useMemo(() => {
    if (filtered.length === 0) {
      return null;
    }
    return filtered.find((item) => item.record.request_id === selectedId) ?? filtered[0];
  }, [filtered, selectedId]);

  const previous = useMemo(() => {
    if (!selected) {
      return undefined;
    }
    const sorted = sortRecordsAscending(records);
    const index = sorted.findIndex((record) => record.request_id === selected.record.request_id);
    return index > 0 ? sorted[index - 1] : undefined;
  }, [records, selected]);

  const detail = useMemo(
    () => (selected ? buildConversationDetail(selected.record, previous) : null),
    [previous, selected],
  );

  return (
    <div className="grid gap-4">
      <div className="grid gap-3 rounded-md border bg-muted/40 p-3 text-sm md:grid-cols-4">
        <Metric label="Requests" value={String(records.length)} />
        <Metric label="Input tokens" value={totals.input_tokens.toLocaleString()} />
        <Metric label="Output tokens" value={totals.output_tokens.toLocaleString()} />
        <Metric label="Total tokens" value={(totals.input_tokens + totals.output_tokens).toLocaleString()} />
      </div>

      <div className="grid gap-2">
        <Label htmlFor="log-search">Search conversations</Label>
        <Input
          id="log-search"
          placeholder="Filter by message, provider, model, status..."
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
        />
      </div>

      <div className="grid min-h-[640px] grid-cols-1 gap-4 lg:grid-cols-3">
        <div className="min-w-0 lg:col-span-1">
          <RequestList
            summaries={filtered}
            selectedId={selected?.record.request_id ?? null}
            onSelect={(summary) => {
              setSelectedId(summary.record.request_id);
              setShowRaw(false);
            }}
          />
        </div>

        {selected && detail ? (
          <div className="grid min-w-0 gap-4 rounded-md border bg-background p-4 lg:col-span-2">
            <RequestHeader summary={selected} />

            <div className="grid gap-3">
              <SectionTitle title="Conversation delta" description="New client-side context compared with the previous request." />
              {detail.contextEntries.length > 0 ? (
                <div className="grid gap-3">
                  {detail.contextEntries.map((entry) => (
                    <ConversationBubble key={entry.id} entry={entry} />
                  ))}
                </div>
              ) : (
                <EmptyState text="No new client context detected for this request." />
              )}
            </div>

            <div className="grid gap-3">
              <SectionTitle title="LLM output" description="Aggregated model response and model tool calls returned to the client." />
              {detail.responseEntries.length > 0 ? (
                <div className="grid gap-3">
                  {detail.responseEntries.map((entry) => (
                    <ConversationBubble key={entry.id} entry={entry} />
                  ))}
                </div>
              ) : (
                <EmptyState text="No response content captured." />
              )}
            </div>

            <div className="grid gap-2 border-t pt-3">
              <Button variant="outline" size="sm" className="w-fit" onClick={() => setShowRaw((value) => !value)}>
                {showRaw ? "Hide raw payloads" : "Show raw payloads"}
              </Button>
              {showRaw && <RawPayloads record={selected.record} />}
            </div>
          </div>
        ) : (
          <div className="flex min-h-[520px] items-center justify-center rounded-md border p-8 text-sm text-muted-foreground lg:col-span-2">
            No request logs found.
          </div>
        )}
      </div>
    </div>
  );
}

function RequestList({
  summaries,
  selectedId,
  onSelect,
}: {
  summaries: ConversationRequestSummary[];
  selectedId: string | null;
  onSelect: (summary: ConversationRequestSummary) => void;
}) {
  return (
    <div className="overflow-hidden rounded-md border">
      <div className="border-b bg-muted px-3 py-2 text-sm font-medium">Requests</div>
      <div className="max-h-[760px] overflow-auto">
        {summaries.map((summary) => {
          const selected = summary.record.request_id === selectedId;
          return (
            <button
              key={summary.record.request_id}
              type="button"
              className={cn(
                "grid w-full gap-2 border-b px-3 py-3 text-left text-sm transition-colors hover:bg-accent",
                selected && "bg-accent",
              )}
              onClick={() => onSelect(summary)}
            >
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0 font-medium leading-5">{summary.title}</div>
                <Badge variant={summary.record.response_status >= 400 ? "destructive" : "secondary"}>
                  {summary.record.response_status}
                </Badge>
              </div>
              <div className="grid gap-1 text-xs text-muted-foreground">
                <span>{formatTimestamp(summary.record.timestamp)}</span>
                <span>{summary.subtitle}</span>
                <span>
                  {summary.record.streaming ? "stream" : "json"} · {summary.newItemCount} visible items
                </span>
              </div>
            </button>
          );
        })}
        {summaries.length === 0 && <EmptyState text="No matching logs." />}
      </div>
    </div>
  );
}

function RequestHeader({ summary }: { summary: ConversationRequestSummary }) {
  const { record } = summary;
  const usage = effectiveUsage(record);
  return (
    <div className="grid gap-3 border-b pb-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="grid gap-1">
          <h2 className="text-xl font-semibold leading-tight">Conversation Request</h2>
          <p className="text-sm text-muted-foreground">{record.request_id}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={record.response_status >= 400 ? "destructive" : "default"}>{record.response_status}</Badge>
          <Badge variant="outline">{record.client_format}</Badge>
          <Badge variant="secondary">{record.provider}</Badge>
        </div>
      </div>
      <div className="grid gap-2 text-sm text-muted-foreground md:grid-cols-4">
        <Metric label="Model" value={`${record.client_model} -> ${record.upstream_model}`} />
        <Metric label="Latency" value={formatDuration(record.latency_ms)} />
        <Metric
          label="Tokens"
          value={`${usage.input_tokens.toLocaleString()} in / ${usage.output_tokens.toLocaleString()} out`}
        />
        <Metric label="Path" value={record.path} />
      </div>
    </div>
  );
}

function ConversationBubble({ entry }: { entry: ConversationEntry }) {
  return (
    <article
      className={cn(
        "grid gap-2 rounded-md border p-3",
        entry.kind === "response" && "border-emerald-200 bg-emerald-50/60",
        entry.kind === "assistant" && "border-blue-200 bg-blue-50/60",
        entry.kind === "user" && "border-slate-200 bg-slate-50",
        entry.kind === "system" && "border-amber-200 bg-amber-50/70",
        entry.kind === "tool_call" && "border-violet-200 bg-violet-50/60",
        entry.kind === "tool_result" && "border-zinc-200 bg-zinc-50",
      )}
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={entry.kind === "response" ? "default" : "outline"}>{entry.title}</Badge>
      </div>
      <div className="min-w-0 overflow-hidden text-sm leading-6 text-foreground">
        {entry.kind === "response" || entry.kind === "assistant" ? (
          <MarkdownContent content={entry.content} />
        ) : (
          <pre className="max-h-[360px] overflow-auto whitespace-pre-wrap break-words font-sans text-sm leading-6">
            {entry.content}
          </pre>
        )}
      </div>
    </article>
  );
}

function MarkdownContent({ content }: { content: string }) {
  return (
    <div className="max-h-[560px] min-w-0 overflow-auto">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => <h1 className="mb-3 mt-2 text-xl font-semibold leading-7">{children}</h1>,
          h2: ({ children }) => <h2 className="mb-2 mt-4 text-lg font-semibold leading-7">{children}</h2>,
          h3: ({ children }) => <h3 className="mb-2 mt-3 text-base font-semibold leading-6">{children}</h3>,
          p: ({ children }) => <p className="my-2 leading-6">{children}</p>,
          ul: ({ children }) => <ul className="my-2 list-disc space-y-1 pl-5">{children}</ul>,
          ol: ({ children }) => <ol className="my-2 list-decimal space-y-1 pl-5">{children}</ol>,
          li: ({ children }) => <li className="pl-1">{children}</li>,
          blockquote: ({ children }) => (
            <blockquote className="my-2 border-l-2 border-border pl-3 text-muted-foreground">{children}</blockquote>
          ),
          code: ({ children, className }) => {
            const codeText = String(children);
            const block = Boolean(className) || codeText.includes("\n");
            if (!block) {
              return <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.9em]">{children}</code>;
            }
            return (
              <code className={cn("block whitespace-pre font-mono text-zinc-50", className)}>
                {children}
              </code>
            );
          },
          pre: ({ children }) => (
            <pre className="my-3 max-w-full overflow-x-auto rounded-md bg-zinc-950 p-3 text-xs leading-5 text-zinc-50">
              {children}
            </pre>
          ),
          table: ({ children }) => (
            <div className="my-3 max-w-full overflow-x-auto rounded-md border bg-background">
              <table className="w-full min-w-max border-collapse text-left text-xs">{children}</table>
            </div>
          ),
          th: ({ children }) => <th className="border-b bg-muted px-3 py-2 font-semibold">{children}</th>,
          td: ({ children }) => <td className="border-b px-3 py-2 align-top">{children}</td>,
          a: ({ children, href }) => (
            <a className="text-blue-700 underline underline-offset-2" href={href} target="_blank" rel="noreferrer">
              {children}
            </a>
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

function RawPayloads({ record }: { record: RequestLogRecord }) {
  const requestText = record.request_body?.text ?? "";
  const upstreamText = record.upstream_request_body?.text ?? "";
  const responseText = responseBodyToText(record.response_body);
  return (
    <div className="grid gap-3">
      {requestText && <RawBlock title="Client request" text={requestText} />}
      {upstreamText && <RawBlock title="Upstream request" text={upstreamText} />}
      <RawBlock title="Response" text={responseText} />
    </div>
  );
}

function RawBlock({ title, text }: { title: string; text: string }) {
  return (
    <div className="grid gap-2">
      <div className="flex items-center justify-between gap-2">
        <h4 className="text-sm font-semibold">{title}</h4>
        <span className="text-xs text-muted-foreground">{formatBytes(new Blob([text]).size)}</span>
      </div>
      <pre className="max-h-64 overflow-auto rounded-md bg-muted p-3 text-xs leading-5">{text}</pre>
    </div>
  );
}

function SectionTitle({ title, description }: { title: string; description: string }) {
  return (
    <div className="grid gap-1">
      <h3 className="text-sm font-semibold text-muted-foreground">{title}</h3>
      <p className="text-sm text-muted-foreground">{description}</p>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid gap-1">
      <span className="text-xs">{label}</span>
      <span className="break-words text-foreground">{value}</span>
    </div>
  );
}

function EmptyState({ text }: { text: string }) {
  return <div className="p-4 text-center text-sm text-muted-foreground">{text}</div>;
}

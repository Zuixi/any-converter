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
import { useI18n } from "../i18n";

interface LogTableProps {
  records: RequestLogRecord[];
}

export function LogTable({ records }: LogTableProps) {
  const { t } = useI18n();
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
        <Metric label={t("logs.requests")} value={String(records.length)} />
        <Metric label={t("logs.inputTokens")} value={totals.input_tokens.toLocaleString()} />
        <Metric label={t("logs.outputTokens")} value={totals.output_tokens.toLocaleString()} />
        <Metric label={t("logs.totalTokens")} value={(totals.input_tokens + totals.output_tokens).toLocaleString()} />
      </div>

      <div className="grid gap-2">
        <Label htmlFor="log-search">{t("logs.search")}</Label>
        <Input
          id="log-search"
          placeholder={t("logs.searchPlaceholder")}
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

            <div className="flex flex-col gap-3">
              {detail.timelineEntries.map((entry) => (
                <ConversationBubble key={entry.id} entry={entry} />
              ))}
            </div>

            <div className="grid gap-2 border-t pt-3">
              <Button variant="outline" size="sm" className="w-fit" onClick={() => setShowRaw((value) => !value)}>
                {showRaw ? t("logs.hideRaw") : t("logs.showRaw")}
              </Button>
              {showRaw && <RawPayloads record={selected.record} />}
            </div>
          </div>
        ) : (
          <div className="flex min-h-[520px] items-center justify-center rounded-md border p-8 text-sm text-muted-foreground lg:col-span-2">
            {t("logs.noLogs")}
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
  const { t } = useI18n();

  return (
    <div className="overflow-hidden rounded-md border">
      <div className="border-b bg-muted px-3 py-2 text-sm font-medium">{t("logs.requests")}</div>
      <div className="max-h-[calc(100vh-280px)] overflow-auto">
        {summaries.map((summary) => {
          const selected = summary.record.request_id === selectedId;
          const clientLabel = summary.record.client_id ?? summary.record.client_format;
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
                <span className="font-medium text-foreground/80">{clientLabel}</span>
                <span>{formatTimestamp(summary.record.timestamp)}</span>
                <span>
                  {summary.record.upstream_model} · {formatDuration(summary.record.latency_ms)} ·{" "}
                  {summary.record.streaming ? "stream" : "json"}
                </span>
              </div>
            </button>
          );
        })}
        {summaries.length === 0 && <EmptyState text={t("logs.noMatches")} />}
      </div>
    </div>
  );
}

function RequestHeader({ summary }: { summary: ConversationRequestSummary }) {
  const { t } = useI18n();
  const { record } = summary;
  const usage = effectiveUsage(record);
  return (
    <div className="grid gap-2 border-b pb-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-lg font-semibold leading-tight">{t("logs.conversationRequest")}</h2>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={record.response_status >= 400 ? "destructive" : "default"}>{record.response_status}</Badge>
          <Badge variant="outline">{record.client_format}</Badge>
          <Badge variant="secondary">{record.provider}</Badge>
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span className="font-mono">{record.request_id}</span>
        <span>·</span>
        <span>{record.client_model} → {record.upstream_model}</span>
        <span>·</span>
        <span>{formatDuration(record.latency_ms)}</span>
        <span>·</span>
        <span>{usage.input_tokens.toLocaleString()} in / {usage.output_tokens.toLocaleString()} out</span>
        <span>·</span>
        <span>{record.path}</span>
      </div>
    </div>
  );
}

function ConversationBubble({ entry }: { entry: ConversationEntry }) {
  const [expanded, setExpanded] = useState(!entry.collapsible);

  const isUser = entry.side === "right";
  const isTool = entry.kind === "tool_call" || entry.kind === "tool_result";

  return (
    <article
      className={cn(
        "flex w-full flex-col gap-2",
        isUser ? "items-end" : "items-start",
        isTool && "pl-3",
      )}
    >
      <div
        className={cn(
          "max-w-[85%] rounded-lg border p-3",
          isUser
            ? "border-slate-700 bg-slate-800 text-white"
            : "border-border bg-muted",
          isTool && "border-l-2 border-l-violet-400",
        )}
      >
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={entry.kind === "response" ? "default" : "outline"}>{entry.title}</Badge>
          {entry.collapsible && (
            <Button
              variant="ghost"
              size="sm"
              className="h-6 px-2 text-xs"
              onClick={() => setExpanded((value) => !value)}
            >
              {expanded ? "▲" : "▼"} {entry.collapsedTitle}
            </Button>
          )}
        </div>
        {expanded && (
          <div className="mt-2 min-w-0 overflow-hidden text-sm leading-6">
            <MarkdownContent content={entry.content} dark={isUser} />
          </div>
        )}
      </div>
    </article>
  );
}

function MarkdownContent({ content, dark }: { content: string; dark?: boolean }) {
  return (
    <div className="min-w-0 overflow-auto">
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
              return (
                <code
                  className={cn(
                    "rounded px-1 py-0.5 font-mono text-[0.9em]",
                    dark ? "bg-slate-700 text-slate-100" : "bg-muted",
                  )}
                >
                  {children}
                </code>
              );
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

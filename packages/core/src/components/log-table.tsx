"use client";

import { useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { RequestLogRecord } from "@any-converter/shared";
import { Badge, Button, Input, Label, cn } from "@any-converter/ui";
import { formatBytes, formatDuration, formatTimestamp } from "@any-converter/shared";

import {
  buildConversationDetail,
  buildSessionSummaries,
  effectiveUsage,
  responseBodyToText,
  sortRecordsAscending,
  totalUsage,
  type ConversationEntry,
  type SessionSummary,
} from "./log-conversation";
import { useI18n } from "../i18n";

interface LogTableProps {
  records: RequestLogRecord[];
}

export function LogTable({ records }: LogTableProps) {
  const { t } = useI18n();
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [showRaw, setShowRaw] = useState(false);

  const sessions = useMemo(() => buildSessionSummaries(records), [records]);
  const totals = useMemo(() => totalUsage(records), [records]);

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) {
      return sessions;
    }
    return sessions.filter((session) =>
      [session.title, session.subtitle, session.clientId, session.sessionId ?? ""]
        .join(" ")
        .toLowerCase()
        .includes(q),
    );
  }, [filter, sessions]);

  const selected = useMemo(() => {
    if (filtered.length === 0) {
      return null;
    }
    return filtered.find((s) => s.key === selectedKey) ?? filtered[0];
  }, [filtered, selectedKey]);

  const sortedRecords = useMemo(
    () => (selected ? sortRecordsAscending(selected.records) : []),
    [selected],
  );

  return (
    <div className="grid min-w-0 gap-4">
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

      <div className="grid min-w-0 grid-cols-1 gap-4 xl:grid-cols-[minmax(240px,320px)_minmax(0,1fr)] xl:items-start">
        <div className="min-w-0">
          <SessionList
            sessions={filtered}
            selectedKey={selected?.key ?? null}
            onSelect={(session) => {
              setSelectedKey(session.key);
              setShowRaw(false);
            }}
          />
        </div>

        {selected && sortedRecords.length > 0 ? (
          <div className="grid min-w-0 max-w-full gap-4 overflow-hidden rounded-md border bg-background p-4">
            <SessionHeader session={selected} />

            <div className="flex flex-col gap-6">
              {sortedRecords.map((record, index) => (
                <RequestBlock
                  key={record.request_id}
                  record={record}
                  previous={index > 0 ? sortedRecords[index - 1] : undefined}
                />
              ))}
            </div>

            <div className="grid gap-2 border-t pt-3">
              <Button variant="outline" size="sm" className="w-fit" onClick={() => setShowRaw((value) => !value)}>
                {showRaw ? t("logs.hideRaw") : t("logs.showRaw")}
              </Button>
              {showRaw && <RawPayloads records={sortedRecords} />}
            </div>
          </div>
        ) : (
          <div className="flex min-h-[320px] items-center justify-center rounded-md border p-8 text-sm text-muted-foreground">
            {t("logs.noLogs")}
          </div>
        )}
      </div>
    </div>
  );
}

function SessionList({
  sessions,
  selectedKey,
  onSelect,
}: {
  sessions: SessionSummary[];
  selectedKey: string | null;
  onSelect: (session: SessionSummary) => void;
}) {
  const { t } = useI18n();

  return (
    <div className="min-w-0 overflow-hidden rounded-md border">
      <div className="border-b bg-muted px-3 py-2 text-sm font-medium">{t("logs.sessions")}</div>
      <div className="max-h-[calc(100vh-280px)] overflow-auto">
        {sessions.map((session) => {
          const selected = session.key === selectedKey;
          return (
            <button
              key={session.key}
              type="button"
              className={cn(
                "grid w-full gap-2 border-b px-3 py-3 text-left text-sm transition-colors hover:bg-accent",
                selected && "bg-accent",
              )}
              onClick={() => onSelect(session)}
            >
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0 font-medium leading-5 [overflow-wrap:anywhere]">{session.title}</div>
                <Badge variant="secondary">{session.requestCount}</Badge>
              </div>
              <div className="grid gap-1 text-xs text-muted-foreground">
                <span className="font-medium text-foreground/80 [overflow-wrap:anywhere]">{session.clientId}</span>
                {session.sessionId && (
                  <span className="font-mono text-[10px] [overflow-wrap:anywhere]">{session.sessionId}</span>
                )}
                <span>{formatTimestamp(session.latestTimestamp)}</span>
              </div>
            </button>
          );
        })}
        {sessions.length === 0 && <EmptyState text={t("logs.noMatches")} />}
      </div>
    </div>
  );
}

function SessionHeader({ session }: { session: SessionSummary }) {
  const { t } = useI18n();
  const latest = session.records[session.records.length - 1];
  const totalIn = session.records.reduce((sum, r) => sum + effectiveUsage(r).input_tokens, 0);
  const totalOut = session.records.reduce((sum, r) => sum + effectiveUsage(r).output_tokens, 0);

  return (
    <div className="grid gap-2 border-b pb-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-lg font-semibold leading-tight">{t("logs.session")}</h2>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline" className="max-w-full [overflow-wrap:anywhere]">
            {session.clientId}
          </Badge>
          <Badge variant="secondary">{session.requestCount} requests</Badge>
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
        {session.sessionId && (
          <>
            <span className="font-mono">{session.sessionId}</span>
            <span>·</span>
          </>
        )}
        <span>{formatTimestamp(session.latestTimestamp)}</span>
        <span>·</span>
        <span>{totalIn.toLocaleString()} in / {totalOut.toLocaleString()} out</span>
        {latest && (
          <>
            <span>·</span>
            <span>{latest.provider}</span>
          </>
        )}
      </div>
    </div>
  );
}

function RequestBlock({ record, previous }: { record: RequestLogRecord; previous?: RequestLogRecord }) {
  const detail = buildConversationDetail(record, previous);
  const usage = effectiveUsage(record);

  return (
    <div className="grid min-w-0 max-w-full gap-3 overflow-hidden rounded-md border bg-muted/20 p-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
        <span className="font-mono">{record.request_id}</span>
        <span>·</span>
        <Badge variant={record.response_status >= 400 ? "destructive" : "default"}>{record.response_status}</Badge>
        <span>·</span>
        <span>{record.client_model} → {record.upstream_model}</span>
        <span>·</span>
        <span>{formatDuration(record.latency_ms)}</span>
        <span>·</span>
        <span>{usage.input_tokens.toLocaleString()} in / {usage.output_tokens.toLocaleString()} out</span>
        <span>·</span>
        <span>{record.path}</span>
      </div>

      <div className="flex min-w-0 max-w-full flex-col gap-3">
        {detail.timelineEntries.map((entry) => (
          <ConversationBubble key={entry.id} entry={entry} />
        ))}
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
        "flex min-w-0 max-w-full flex-col gap-2 overflow-hidden",
        isUser ? "items-end" : "items-start",
        isTool && "pl-3",
      )}
    >
      <div
        className={cn(
          "min-w-0 max-w-full overflow-hidden rounded-lg border p-3 md:max-w-[85%]",
          isUser
            ? "border-[#5a8ae8] bg-[#6f9af9] text-white"
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
          <div className="mt-2 min-w-0 max-w-full overflow-hidden text-sm leading-6 [overflow-wrap:anywhere]">
            <MarkdownContent content={entry.content} dark={isUser} />
          </div>
        )}
      </div>
    </article>
  );
}

function MarkdownContent({ content, dark }: { content: string; dark?: boolean }) {
  return (
    <div className="min-w-0 max-w-full overflow-x-auto [overflow-wrap:anywhere]">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => <h1 className="mb-3 mt-2 text-xl font-semibold leading-7">{children}</h1>,
          h2: ({ children }) => <h2 className="mb-2 mt-4 text-lg font-semibold leading-7">{children}</h2>,
          h3: ({ children }) => <h3 className="mb-2 mt-3 text-base font-semibold leading-6">{children}</h3>,
          p: ({ children }) => <p className="my-2 max-w-full leading-6 [overflow-wrap:anywhere]">{children}</p>,
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
                    dark ? "bg-[#4a7ad8] text-slate-100" : "bg-muted",
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
            <div className="my-3 min-w-0 max-w-full overflow-x-auto rounded-md border bg-background">
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

function RawPayloads({ records }: { records: RequestLogRecord[] }) {
  return (
    <div className="grid min-w-0 max-w-full gap-3">
      {records.map((record) => {
        const requestText = record.request_body?.text ?? "";
        const upstreamText = record.upstream_request_body?.text ?? "";
        const responseText = responseBodyToText(record.response_body);
        return (
          <div key={record.request_id} className="grid min-w-0 max-w-full gap-3 overflow-hidden rounded-md border p-3">
            <div className="text-xs font-mono text-muted-foreground">{record.request_id}</div>
            {requestText && <RawBlock title="Client request" text={requestText} />}
            {upstreamText && <RawBlock title="Upstream request" text={upstreamText} />}
            <RawBlock title="Response" text={responseText} />
          </div>
        );
      })}
    </div>
  );
}

function RawBlock({ title, text }: { title: string; text: string }) {
  return (
    <div className="grid min-w-0 max-w-full gap-2">
      <div className="flex items-center justify-between gap-2">
        <h4 className="text-sm font-semibold">{title}</h4>
        <span className="text-xs text-muted-foreground">{formatBytes(new Blob([text]).size)}</span>
      </div>
      <pre className="max-h-64 min-w-0 max-w-full overflow-auto rounded-md bg-muted p-3 text-xs leading-5">
        {text}
      </pre>
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

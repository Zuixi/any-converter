"use client";

import { useEffect, useState } from "react";

import type { SessionSummary, TranscriptPart, TranscriptTurn } from "@any-converter/shared";
import { formatTimestamp } from "@any-converter/shared";
import { Badge, Button, Input, Label, cn } from "@any-converter/ui";

import { useSessions, useTranscript } from "../hooks/use-sessions";
import { useI18n, type TranslationKey } from "../i18n";

const CLAMP_CHARS = 700;
const CLAMP_LINES = 9;

export function SessionList() {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const { sessions, total, loading, error, loadMore } = useSessions(debounced);

  useEffect(() => {
    const handle = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(handle);
  }, [query]);

  const open = sessions.find((session) => sessionKey(session) === selected) ?? null;

  return (
    <div className="grid min-w-0 gap-4">
      <div className="grid gap-2">
        <Label htmlFor="session-search">{t("logs.search")}</Label>
        <Input
          id="session-search"
          placeholder={t("logs.searchPlaceholder")}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </div>

      {loading && sessions.length === 0 && <p className="text-muted-foreground">{t("logs.loading")}</p>}
      {error && <p className="text-destructive">{error}</p>}

      <div className="min-w-0 overflow-hidden rounded-md border">
        <div className="border-b bg-muted px-3 py-2 text-sm font-medium">
          {t("logs.sessions")} · {total}
        </div>
        {sessions.map((session) => {
          const key = sessionKey(session);
          const isOpen = open !== null && sessionKey(open) === key;
          return (
            <div key={key} className={cn("border-b", isOpen && "bg-accent/40")}>
              <button
                type="button"
                className="grid w-full gap-1 px-3 py-3 text-left text-sm hover:bg-accent"
                aria-expanded={isOpen}
                onClick={() => setSelected(isOpen ? null : key)}
              >
                <div className="flex items-start justify-between gap-3">
                  <span className="min-w-0 font-medium leading-5 [overflow-wrap:anywhere]">
                    {session.title || t("logs.noPrompt")}
                  </span>
                  <span className="shrink-0 text-xs text-muted-foreground">{formatTimestamp(session.last)}</span>
                </div>
                <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
                  <span>{session.client_id}</span>
                  <span>·</span>
                  <span className="[overflow-wrap:anywhere]">{session.model}</span>
                  <span>·</span>
                  <span>
                    {session.requests} {t("logs.requests").toLowerCase()}
                  </span>
                  <span>·</span>
                  <span>
                    {session.input_tokens.toLocaleString()} / {session.output_tokens.toLocaleString()}
                  </span>
                  {session.errors > 0 && <Badge variant="destructive">{session.errors}</Badge>}
                </div>
              </button>
              {isOpen && <Transcript clientId={session.client_id} sessionId={session.session_id} />}
            </div>
          );
        })}
        {sessions.length === 0 && !loading && !error && (
          <div className="p-4 text-center text-sm text-muted-foreground">{t("logs.noLogs")}</div>
        )}
      </div>

      {sessions.length < total && (
        <Button variant="outline" size="sm" className="w-fit" onClick={() => void loadMore()}>
          {t("logs.showMore")}
        </Button>
      )}
    </div>
  );
}

// NUL separates the two ids so no pair of values can collide.
function sessionKey(session: SessionSummary): string {
  return `${session.client_id}\u0000${session.session_id}`;
}

function Transcript({ clientId, sessionId }: { clientId: string; sessionId: string }) {
  const { t } = useI18n();
  const { transcript, loading, error } = useTranscript(clientId, sessionId);

  if (loading) {
    return <p className="px-3 pb-3 text-sm text-muted-foreground">{t("logs.reading")}</p>;
  }
  if (error) {
    return <p className="px-3 pb-3 text-sm text-destructive">{error}</p>;
  }
  if (!transcript || transcript.turns.length === 0) {
    return <p className="px-3 pb-3 text-sm text-muted-foreground">{t("logs.nothingSaid")}</p>;
  }

  return (
    <div className="grid min-w-0 gap-3 px-3 pb-3">
      {transcript.turns.map((turn) => (
        <TurnView key={turn.request_id} turn={turn} />
      ))}
      {transcript.cut && <p className="text-xs text-muted-foreground">{t("logs.cut")}</p>}
    </div>
  );
}

/** One logged request: the facts of the exchange, then the parts it carried. */
function TurnView({ turn }: { turn: TranscriptTurn }) {
  const { t } = useI18n();
  const failed = turn.status >= 400;

  return (
    <div className={cn("min-w-0 overflow-hidden rounded-md border", failed && "border-destructive/50")}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
        <span>{formatTimestamp(turn.timestamp)}</span>
        <span className="font-medium text-foreground [overflow-wrap:anywhere]">{turn.model}</span>
        {turn.provider && <span className="[overflow-wrap:anywhere]">{turn.provider}</span>}
        <span title={`${t("logs.inputTokens")} / ${t("logs.outputTokens")}`}>
          {turn.input_tokens.toLocaleString()} / {turn.output_tokens.toLocaleString()} {t("logs.tokensShort")}
        </span>
        {turn.cache_read_tokens > 0 && (
          <span>
            {t("logs.cacheShort")} {turn.cache_read_tokens.toLocaleString()}
          </span>
        )}
        {turn.ttft_ms > 0 && (
          <span>
            {t("logs.ttft")} {turn.ttft_ms.toLocaleString()} ms
          </span>
        )}
        {turn.flow_ms > 0 && <span>{turn.flow_ms.toLocaleString()} ms</span>}
        {typeof turn.cost_usd === "number" && <span>${turn.cost_usd.toFixed(4)}</span>}
        {failed && (
          <Badge variant="destructive">
            {t("logs.error")} {turn.status}
          </Badge>
        )}
      </div>
      <div className="grid gap-2 p-3">
        {turn.parts.map((part, index) => (
          <PartView key={`${turn.request_id}-${index}`} part={part} />
        ))}
        {turn.error && <p className="text-xs text-destructive [overflow-wrap:anywhere]">{turn.error}</p>}
      </div>
    </div>
  );
}

function PartView({ part }: { part: TranscriptPart }) {
  const { t } = useI18n();
  const folded = part.kind === "thinking" || part.role === "system";
  const long = part.text.length > CLAMP_CHARS || part.text.split("\n").length > CLAMP_LINES;
  const [open, setOpen] = useState(!long);
  const failed = part.kind === "tool_result" && part.is_error;

  const head = (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <Badge variant={part.role === "user" ? "default" : failed ? "destructive" : "outline"}>{label(part, t)}</Badge>
      {part.name && <code className="[overflow-wrap:anywhere]">{part.name}</code>}
      {part.media_type && <span className="text-muted-foreground">{part.media_type}</span>}
      {part.id && <span className="text-muted-foreground/70">#{part.id}</span>}
    </div>
  );
  const body = (
    <pre className="mt-2 max-h-96 overflow-auto whitespace-pre-wrap text-sm leading-6 [overflow-wrap:anywhere]">
      {part.text}
      {part.cut ? `\n… ${part.cut}` : ""}
    </pre>
  );

  if (folded) {
    return (
      <details className="rounded-md border bg-muted/40 p-3">
        <summary className="cursor-pointer">{head}</summary>
        {body}
      </details>
    );
  }

  return (
    <div
      className={cn(
        "rounded-md border p-3",
        part.role === "user" ? "bg-muted/30" : "bg-background",
        failed && "border-destructive/50 bg-destructive/5",
      )}
    >
      {head}
      {open && body}
      {long && (
        <Button variant="ghost" size="sm" className="mt-1 h-6 px-2 text-xs" onClick={() => setOpen((value) => !value)}>
          {open ? t("logs.collapse") : t("logs.expand")}
        </Button>
      )}
    </div>
  );
}

function label(part: TranscriptPart, t: (key: TranslationKey) => string): string {
  if (part.kind === "thinking") {
    return t("logs.thinking");
  }
  if (part.kind === "image") {
    return t("logs.image");
  }
  if (part.kind === "tool_use") {
    return t("logs.toolCall");
  }
  if (part.kind === "tool_result") {
    return part.is_error ? t("logs.toolError") : t("logs.toolResult");
  }
  if (part.role === "system") {
    return t("logs.systemPrompt");
  }
  if (part.role === "assistant") {
    return "Assistant";
  }
  if (part.role === "user") {
    return "You";
  }
  return part.role;
}

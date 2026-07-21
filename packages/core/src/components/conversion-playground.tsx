"use client";

import { useState } from "react";

import { type Format, FORMAT_LABELS, prettyJson } from "@any-converter/shared";
import { Button, FormatSelector, JsonEditor, Label } from "@any-converter/ui";

import { useI18n } from "../i18n";
import { useConvert } from "../hooks/use-convert";

const EXAMPLES = {
  request: {
    openai_chat: { model: "gpt-4.1", messages: [{ role: "user", content: "Hello, world!" }] },
    openai_responses: { model: "gpt-4.1", input: "Hello, world!" },
    claude: { model: "claude-sonnet-4-20250514", max_tokens: 256, messages: [{ role: "user", content: "Hello, world!" }] },
    gemini: { contents: [{ role: "user", parts: [{ text: "Hello, world!" }] }] },
  },
  response: {
    openai_chat: {
      id: "chatcmpl_123",
      object: "chat.completion",
      created: 0,
      model: "gpt-4.1",
      choices: [
        {
          index: 0,
          message: { role: "assistant", content: "Hello!" },
          finish_reason: "stop",
        },
      ],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    },
    openai_responses: {
      id: "resp_123",
      object: "response",
      created_at: 0,
      model: "gpt-4.1",
      status: "completed",
      output: [
        {
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "Hello!" }],
        },
      ],
      usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
    },
    claude: {
      id: "msg_123",
      type: "message",
      role: "assistant",
      model: "claude-sonnet-4-20250514",
      content: [{ type: "text", text: "Hello!" }],
      stop_reason: "end_turn",
      usage: { input_tokens: 1, output_tokens: 1 },
    },
    gemini: {
      candidates: [
        {
          content: { role: "model", parts: [{ text: "Hello!" }] },
          finishReason: "STOP",
        },
      ],
    },
  },
} satisfies Record<"request" | "response", Record<Format, unknown>>;

function examplePayload(mode: "request" | "response", format: Format) {
  return JSON.stringify(EXAMPLES[mode][format], null, 2);
}

export function ConversionPlayground() {
  const { t } = useI18n();
  const [input, setInput] = useState(examplePayload("request", "openai_chat"));
  const [from, setFrom] = useState<Format>("openai_chat");
  const [to, setTo] = useState<Format>("claude");
  const [mode, setMode] = useState<"request" | "response">("request");
  const { output, error, loading, convert } = useConvert();

  const handleConvert = () => {
    const formatted = prettyJson(input);
    if (formatted !== input) {
      setInput(formatted);
    }
    void convert(formatted, from, to, mode);
  };

  const swap = () => {
    const nextFrom = to;
    const nextTo = from;
    setFrom(nextFrom);
    setTo(nextTo);
    setInput(examplePayload(mode, nextFrom));
  };

  const changeMode = (next: "request" | "response") => {
    setMode(next);
    setInput(examplePayload(next, from));
  };

  const changeFrom = (next: Format) => {
    setFrom(next);
    setInput(examplePayload(mode, next));
  };

  const beautifyInput = () => {
    setInput(prettyJson(input));
  };

  return (
    <div className="grid gap-6">
      <div className="flex flex-wrap items-end gap-4">
        <FormatSelector label={t("playground.from")} value={from} onChange={changeFrom} exclude={[to]} />
        <Button variant="outline" onClick={swap}>
          ⇄ {t("playground.swap")}
        </Button>
        <FormatSelector label={t("playground.to")} value={to} onChange={setTo} exclude={[from]} />
        <div className="grid gap-2">
          <Label>{t("playground.mode")}</Label>
          <div className="flex gap-2">
            <Button variant={mode === "request" ? "default" : "outline"} onClick={() => changeMode("request")}>
              {t("playground.request")}
            </Button>
            <Button variant={mode === "response" ? "default" : "outline"} onClick={() => changeMode("response")}>
              {t("playground.response")}
            </Button>
          </div>
        </div>
        <Button variant="secondary" onClick={() => setInput(examplePayload(mode, from))}>
          {t("playground.loadExample")}
        </Button>
        <Button variant="outline" onClick={beautifyInput}>
          {t("playground.beautify")}
        </Button>
        <Button onClick={handleConvert} disabled={loading}>
          {loading ? t("playground.converting") : `${t("playground.convertTo")} ${FORMAT_LABELS[to]}`}
        </Button>
      </div>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <div className="grid gap-2">
          <Label>
            {t("playground.input")} ({FORMAT_LABELS[from]})
          </Label>
          <JsonEditor value={input} onChange={(e) => setInput(e.target.value)} />
        </div>
        <div className="grid gap-2">
          <Label>
            {t("playground.output")} ({FORMAT_LABELS[to]})
          </Label>
          <JsonEditor
            value={output}
            readOnly
            placeholder={t("playground.outputPlaceholder")}
            error={error || undefined}
          />
        </div>
      </div>
    </div>
  );
}

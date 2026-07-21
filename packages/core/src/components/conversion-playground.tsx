"use client";

import { useState } from "react";

import { type Format, FORMAT_LABELS } from "@any-converter/shared";
import { Button, FormatSelector, JsonEditor, Label, Textarea } from "@any-converter/ui";

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
    openai_chat: { id: "chatcmpl_123", model: "gpt-4.1", choices: [{ message: { role: "assistant", content: "Hello!" } }] },
    openai_responses: { id: "resp_123", model: "gpt-4.1", output: [{ type: "message", content: [{ type: "output_text", text: "Hello!" }] }] },
    claude: { id: "msg_123", model: "claude-sonnet-4-20250514", content: [{ type: "text", text: "Hello!" }] },
    gemini: { candidates: [{ content: { parts: [{ text: "Hello!" }] } }] },
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
    void convert(input, from, to, mode);
  };

  const swap = () => {
    setFrom(to);
    setTo(from);
  };

  const changeMode = (next: "request" | "response") => {
    setMode(next);
    setInput(examplePayload(next, from));
  };

  const changeFrom = (next: Format) => {
    setFrom(next);
    setInput(examplePayload(mode, next));
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
          <Textarea
            value={output}
            readOnly
            className="min-h-[320px] font-mono"
            placeholder={t("playground.outputPlaceholder")}
          />
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
      </div>
    </div>
  );
}

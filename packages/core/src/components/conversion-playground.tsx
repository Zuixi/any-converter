"use client";

import { useState } from "react";

import { type Format, FORMAT_LABELS } from "@any-converter/shared";
import { Button, FormatSelector, JsonEditor, Label, Textarea } from "@any-converter/ui";

import { useConvert } from "../hooks/use-convert";

const EXAMPLE_REQUEST = JSON.stringify(
  {
    model: "gpt-4",
    messages: [{ role: "user", content: "Hello, world!" }],
  },
  null,
  2,
);

export function ConversionPlayground() {
  const [input, setInput] = useState(EXAMPLE_REQUEST);
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

  return (
    <div className="grid gap-6">
      <div className="flex flex-wrap items-end gap-4">
        <FormatSelector label="From" value={from} onChange={setFrom} exclude={[to]} />
        <Button variant="outline" onClick={swap}>
          ⇄ Swap
        </Button>
        <FormatSelector label="To" value={to} onChange={setTo} exclude={[from]} />
        <div className="grid gap-2">
          <Label>Mode</Label>
          <div className="flex gap-2">
            <Button variant={mode === "request" ? "default" : "outline"} onClick={() => setMode("request")}>
              Request
            </Button>
            <Button variant={mode === "response" ? "default" : "outline"} onClick={() => setMode("response")}>
              Response
            </Button>
          </div>
        </div>
        <Button onClick={handleConvert} disabled={loading}>
          {loading ? "Converting..." : `Convert to ${FORMAT_LABELS[to]}`}
        </Button>
      </div>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <div className="grid gap-2">
          <Label>Input ({FORMAT_LABELS[from]})</Label>
          <JsonEditor value={input} onChange={(e) => setInput(e.target.value)} />
        </div>
        <div className="grid gap-2">
          <Label>Output ({FORMAT_LABELS[to]})</Label>
          <Textarea
            value={output}
            readOnly
            className="min-h-[320px] font-mono"
            placeholder="Converted output will appear here..."
          />
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
      </div>
    </div>
  );
}

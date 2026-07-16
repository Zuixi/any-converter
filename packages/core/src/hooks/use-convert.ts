"use client";

import { useState, useCallback } from "react";

import type { ConvertApiRequest, ConvertApiResponse, Format } from "@any-converter/shared";

export function useConvert() {
  const [output, setOutput] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  const convert = useCallback(async (input: string, from: Format, to: Format, mode: "request" | "response") => {
    setLoading(true);
    setError("");
    try {
      const body: ConvertApiRequest = { input, from, to, mode };
      const res = await fetch("/api/convert", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const data = (await res.json()) as ConvertApiResponse;
      if (!res.ok || data.error) {
        setError(data.error ?? "Conversion failed");
        setOutput("");
        return;
      }
      setOutput(data.output);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setOutput("");
    } finally {
      setLoading(false);
    }
  }, []);

  return { output, error, loading, convert };
}

"use client";

import { useState, useCallback } from "react";

import type { ConvertApiRequest, Format } from "@any-converter/shared";

import { useApiClient } from "./api-client";

export function useConvert() {
  const api = useApiClient();
  const [output, setOutput] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  const convert = useCallback(async (input: string, from: Format, to: Format, mode: "request" | "response") => {
    setLoading(true);
    setError("");
    try {
      const body: ConvertApiRequest = { input, from, to, mode };
      const data = await api.convert(body);
      setOutput(data.output);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setOutput("");
    } finally {
      setLoading(false);
    }
  }, [api]);

  return { output, error, loading, convert };
}

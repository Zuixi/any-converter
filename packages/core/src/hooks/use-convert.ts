"use client";

import { useState, useCallback } from "react";

import type { ConvertApiRequest, Format } from "@any-converter/shared";
import { prettyJson } from "@any-converter/shared";

import { useApiClient } from "./api-client";

function errorMessage(err: unknown): string {
  if (err instanceof Error) {
    return err.message;
  }
  if (typeof err === "string" && err.trim()) {
    return err;
  }
  if (err && typeof err === "object" && "message" in err && typeof err.message === "string") {
    return err.message;
  }
  return String(err || "Unknown error");
}

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
      setOutput(prettyJson(data.output));
    } catch (err) {
      setError(errorMessage(err));
      setOutput("");
    } finally {
      setLoading(false);
    }
  }, [api]);

  return { output, error, loading, convert };
}

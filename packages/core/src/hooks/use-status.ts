"use client";

import { useEffect, useState } from "react";

import type { StatusData } from "@any-converter/shared";

import { useApiClient } from "./api-client";

export function useStatus(pollMs = 5000) {
  const api = useApiClient();
  const [status, setStatus] = useState<StatusData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const data = await api.getStatus();
        setStatus(data);
        setError("");
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }

    void load();
    const id = setInterval(() => void load(), pollMs);
    return () => clearInterval(id);
  }, [api, pollMs]);

  return { status, loading, error };
}

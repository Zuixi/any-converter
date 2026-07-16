"use client";

import { useEffect, useState } from "react";

import type { StatusData } from "@any-converter/shared";

export function useStatus(pollMs = 5000) {
  const [status, setStatus] = useState<StatusData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const res = await fetch("/api/status");
        if (!res.ok) {
          setError(`Failed to load status: ${res.statusText}`);
          return;
        }
        const data = (await res.json()) as StatusData;
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
  }, [pollMs]);

  return { status, loading, error };
}

"use client";

import { useEffect, useState } from "react";

import type { AggregatedUsage } from "@any-converter/shared";

export function useUsage() {
  const [data, setData] = useState<AggregatedUsage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const res = await fetch("/api/usage");
        if (!res.ok) {
          setError(`Failed to load usage: ${res.statusText}`);
          return;
        }
        const result = (await res.json()) as { records: AggregatedUsage[] };
        setData(result.records);
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, []);

  return { data, loading, error };
}

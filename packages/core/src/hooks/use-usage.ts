"use client";

import { useEffect, useState } from "react";

import type { AggregatedUsage } from "@any-converter/shared";

import { useApiClient } from "./api-client";

export function useUsage() {
  const api = useApiClient();
  const [data, setData] = useState<AggregatedUsage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        setData(await api.getUsage());
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, [api]);

  return { data, loading, error };
}

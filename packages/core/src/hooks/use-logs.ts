"use client";

import { useEffect, useState } from "react";

import type { RequestLogRecord } from "@any-converter/shared";

export function useLogs() {
  const [records, setRecords] = useState<RequestLogRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const res = await fetch("/api/logs");
        if (!res.ok) {
          setError(`Failed to load logs: ${res.statusText}`);
          return;
        }
        const data = (await res.json()) as { records: RequestLogRecord[] };
        setRecords(data.records);
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, []);

  return { records, loading, error };
}

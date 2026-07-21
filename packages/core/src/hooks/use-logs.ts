"use client";

import { useEffect, useState } from "react";

import type { RequestLogRecord } from "@any-converter/shared";

import { useApiClient } from "./api-client";
import { errorMessage } from "./error-message";

export function useLogs() {
  const api = useApiClient();
  const [records, setRecords] = useState<RequestLogRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        setRecords(await api.getLogs());
      } catch (err) {
        setError(errorMessage(err));
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, [api]);

  return { records, loading, error };
}

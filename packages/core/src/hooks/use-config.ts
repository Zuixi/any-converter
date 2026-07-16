"use client";

import { useEffect, useState } from "react";

import type { ServerConfig } from "@any-converter/shared";

import { useApiClient } from "./api-client";

export function useConfig() {
  const api = useApiClient();
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [raw, setRaw] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const data = await api.getConfig();
        setConfig(data.config);
        setRaw(data.raw);
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, [api]);

  const save = async (nextRaw: string) => {
    setLoading(true);
    setSaved(false);
    try {
      await api.saveConfig(nextRaw);
      setSaved(true);
      setRaw(nextRaw);
      setError("");
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setLoading(false);
    }
  };

  return { config, raw, loading, error, saved, save };
}

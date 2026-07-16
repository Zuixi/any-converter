"use client";

import { useEffect, useState } from "react";

import type { ServerConfig } from "@any-converter/shared";

export function useConfig() {
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [raw, setRaw] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    async function load() {
      setLoading(true);
      try {
        const res = await fetch("/api/config");
        if (!res.ok) {
          setError(`Failed to load config: ${res.statusText}`);
          return;
        }
        const data = (await res.json()) as { config: ServerConfig; raw: string };
        setConfig(data.config);
        setRaw(data.raw);
      } catch (err) {
        setError(err instanceof Error ? err.message : "Unknown error");
      } finally {
        setLoading(false);
      }
    }
    void load();
  }, []);

  const save = async (nextRaw: string) => {
    setLoading(true);
    setSaved(false);
    try {
      const res = await fetch("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ raw: nextRaw }),
      });
      if (!res.ok) {
        const data = (await res.json()) as { error?: string };
        setError(data.error ?? "Failed to save config");
        return;
      }
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

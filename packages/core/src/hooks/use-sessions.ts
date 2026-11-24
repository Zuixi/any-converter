"use client";

import { useEffect, useState } from "react";

import type { SessionSummary, SessionTranscript } from "@any-converter/shared";

import { useApiClient } from "./api-client";
import { errorMessage } from "./error-message";

const PAGE = 50;

export function useSessions(query: string) {
  const api = useApiClient();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    async function load() {
      setLoading(true);
      try {
        const page = await api.getSessions(query);
        if (!cancelled) {
          setSessions(page.sessions);
          setTotal(page.total);
          setError("");
        }
      } catch (err) {
        if (!cancelled) {
          setError(errorMessage(err));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [api, query]);

  async function loadMore() {
    if (loading) {
      return;
    }
    setLoading(true);
    try {
      const page = await api.getSessions(query, sessions.length);
      setSessions((current) => [...current, ...page.sessions]);
      setTotal(page.total);
      setError("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  return { sessions, total, loading, error, loadMore, pageSize: PAGE };
}

/** One transcript per session, kept so opening the same one reads nothing again. */
const transcripts = new Map<string, SessionTranscript>();

export function useTranscript(clientId: string, sessionId: string) {
  const api = useApiClient();
  // NUL separates the two ids so no pair of values can collide.
  const key = `${clientId}\u0000${sessionId}`;
  const cached = transcripts.get(key);
  const [transcript, setTranscript] = useState<SessionTranscript | null>(cached ?? null);
  const [loading, setLoading] = useState(!cached);
  const [error, setError] = useState("");

  useEffect(() => {
    const hit = transcripts.get(key);
    if (hit) {
      setTranscript(hit);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    async function load() {
      try {
        const next = await api.getSessionTranscript(clientId, sessionId);
        transcripts.set(key, next);
        if (!cancelled) {
          setTranscript(next);
          setError("");
        }
      } catch (err) {
        if (!cancelled) {
          setError(errorMessage(err));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [api, clientId, key, sessionId]);

  return { transcript, loading, error };
}

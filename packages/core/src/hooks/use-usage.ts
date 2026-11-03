"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import type { Heatmap, LedgerFilter, LedgerPage, UsageQuery, UsageSummary } from "@any-converter/shared";

import { useApiClient } from "./api-client";
import { errorMessage } from "./error-message";

export interface UsageState {
  summary: UsageSummary | null;
  loading: boolean;
  error: string;
}

export interface HeatmapState {
  heatmap: Heatmap | null;
  loading: boolean;
  error: string;
}

export interface LedgerState {
  page: LedgerPage | null;
  loading: boolean;
  error: string;
  loadMore: () => void;
}

export function useUsage(query: UsageQuery): UsageState {
  const api = useApiClient();
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const { from, to, granularity, provider, model, agent, split_by } = query;
  // Callers build a fresh query object on every render, so object identity is
  // not a meaningful dependency: only a changed field should trigger a fetch.
  const queryKey = JSON.stringify(query);

  useEffect(() => {
    // The client contract has no signal parameter, so the controller doubles as
    // the staleness token: aborting it turns a late response into a no-op.
    const controller = new AbortController();
    setLoading(true);
    setError("");
    api
      .getUsage({ from, to, granularity, provider, model, agent, split_by })
      .then((next) => {
        if (controller.signal.aborted) {
          return;
        }
        setSummary(next);
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        setError(errorMessage(err));
      })
      .finally(() => {
        if (controller.signal.aborted) {
          return;
        }
        setLoading(false);
      });
    return () => controller.abort();
  }, [api, queryKey, from, to, granularity, provider, model, agent, split_by]);

  return { summary, loading, error };
}

export function useHeatmap(filter: LedgerFilter): HeatmapState {
  const api = useApiClient();
  const [heatmap, setHeatmap] = useState<Heatmap | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const { from, to, provider, model, agent } = filter;
  // Only the dimensions the rollup keeps narrow the calendar, and the range is
  // the one the API call has to carry; the heatmap's own span is the server's.
  const filterKey = JSON.stringify({ from, to, provider, model, agent });

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError("");
    api
      .getHeatmap({ from, to, provider, model, agent })
      .then((next) => {
        if (controller.signal.aborted) {
          return;
        }
        setHeatmap(next);
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        setError(errorMessage(err));
      })
      .finally(() => {
        if (controller.signal.aborted) {
          return;
        }
        setLoading(false);
      });
    return () => controller.abort();
  }, [api, filterKey, from, to, provider, model, agent]);

  return { heatmap, loading, error };
}

export function useLedger(filter: LedgerFilter): LedgerState {
  const api = useApiClient();
  const [page, setPage] = useState<LedgerPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const inFlight = useRef<AbortController | null>(null);
  const { from, to, day, provider, model, agent, account, failed, query } = filter;

  const load = useCallback(
    (cursor?: number) => {
      // A newer cursor tab or a new filter supersedes whatever is in flight.
      inFlight.current?.abort();
      const controller = new AbortController();
      inFlight.current = controller;
      setLoading(true);
      setError("");
      api
        .getLedger({ from, to, day, provider, model, agent, account, failed, query }, cursor)
        .then((next) => {
          if (controller.signal.aborted) {
            return;
          }
          setPage((previous) => mergePage(previous, next, cursor !== undefined));
        })
        .catch((err: unknown) => {
          if (controller.signal.aborted) {
            return;
          }
          setError(errorMessage(err));
        })
        .finally(() => {
          if (controller.signal.aborted) {
            return;
          }
          setLoading(false);
        });
    },
    [api, from, to, day, provider, model, agent, account, failed, query],
  );

  useEffect(() => {
    setPage(null);
    load();
    return () => inFlight.current?.abort();
  }, [load]);

  const loadMore = useCallback(() => {
    const cursor = page?.next_cursor;
    if (cursor === undefined || loading) {
      return;
    }
    load(cursor);
  }, [load, page, loading]);

  return { page, loading, error, loadMore };
}

/** Append a cursor page, dropping rows the previous pages already showed. */
function mergePage(previous: LedgerPage | null, next: LedgerPage, append: boolean): LedgerPage {
  if (previous === null || !append) {
    return next;
  }
  const seen = new Set(previous.rows.map((row) => row.request_id));
  const rows = [...previous.rows];
  for (const row of next.rows) {
    if (seen.has(row.request_id)) {
      continue;
    }
    seen.add(row.request_id);
    rows.push(row);
  }
  return { ...next, rows };
}

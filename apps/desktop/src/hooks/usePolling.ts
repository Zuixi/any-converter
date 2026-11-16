import { useCallback, useEffect, useRef, useState } from "react";

/**
 * setInterval + fetch is easy to get subtly wrong: hidden tabs keep polling,
 * slow probes stack, and every tick re-renders even when nothing changed.
 * This hook runs `fetchSnapshot` on a cadence with those three fixed —
 * rounds skipped while the document is hidden or the previous one is still
 * in flight, an extra round on focus, and the state identity kept when the
 * next snapshot equals the last. `refresh` runs a round on demand (used
 * after a user action, e.g. starting the gateway) even in a hidden document.
 */
export function usePolling<T>(
  fetchSnapshot: (previous: T | undefined) => Promise<T>,
  isEqual: (a: T, b: T) => boolean,
  intervalMs: number,
): { data: T | undefined; refresh: () => void } {
  const [snapshot, setSnapshot] = useState<T | undefined>(undefined);
  const snapshotRef = useRef<T | undefined>(undefined);
  // The latest callbacks, so callers can pass fresh closures without
  // restarting the poll loop (and without re-fetching) on every render.
  const fetchRef = useRef(fetchSnapshot);
  const equalRef = useRef(isEqual);
  fetchRef.current = fetchSnapshot;
  equalRef.current = isEqual;

  const tickRef = useRef<(force: boolean) => void>(() => {});

  useEffect(() => {
    let cancelled = false;
    let inFlight = false;
    // A refresh that arrived mid-round must not be lost: queue it and run
    // as soon as the current round lands (a range switch would otherwise
    // wait a whole interval for the next tick).
    let pendingRefresh = false;

    const tick = (force: boolean) => {
      if (!force && document.hidden) {
        return;
      }
      if (inFlight) {
        if (force) {
          pendingRefresh = true;
        }
        return;
      }
      inFlight = true;
      void fetchRef
        .current(snapshotRef.current)
        .then((next) => {
          if (cancelled) {
            return;
          }
          const previous = snapshotRef.current;
          if (previous !== undefined && equalRef.current(previous, next)) {
            return;
          }
          snapshotRef.current = next;
          setSnapshot(next);
        })
        .finally(() => {
          inFlight = false;
          if (pendingRefresh) {
            pendingRefresh = false;
            tick(true);
          }
        });
    };

    tickRef.current = tick;
    tick(false);
    const timer = window.setInterval(() => tick(false), intervalMs);

    const onVisibility = () => {
      if (!document.hidden) {
        tick(false);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelled = true;
      tickRef.current = () => {};
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [intervalMs]);

  const refresh = useCallback(() => {
    tickRef.current(true);
  }, []);

  return { data: snapshot, refresh };
}

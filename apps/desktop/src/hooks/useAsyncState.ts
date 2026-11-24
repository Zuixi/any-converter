import React, { useState } from "react";

import { errorMessage } from "@any-converter/core";

export function useAsyncState<T>(
  loader: () => Promise<T>,
  fallback?: T,
): [
  T | undefined,
  React.Dispatch<React.SetStateAction<T | undefined>>,
  string | undefined,
  React.Dispatch<React.SetStateAction<string | undefined>>,
  boolean,
] {
  const [value, setValue] = useState<T | undefined>(fallback);
  const [error, setError] = useState<string | undefined>();
  // Starts true: a page must be able to tell "still loading" apart from
  // "loaded, and the collection is genuinely empty".
  const [loading, setLoading] = useState(true);

  React.useEffect(() => {
    let cancelled = false;
    loader()
      .then((result) => {
        if (!cancelled) {
          setValue(result);
        }
      })
      .catch((cause) => {
        if (!cancelled) {
          // Transports reject with `{kind, message}` objects, not `Error`s.
          setError(errorMessage(cause));
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
    // Intentionally run once on mount for page-level data loads.
  }, []);

  return [value, setValue, error, setError, loading];
}

import React, { useState } from "react";

export function useAsyncState<T>(
  loader: () => Promise<T>,
  fallback?: T,
): [
  T | undefined,
  React.Dispatch<React.SetStateAction<T | undefined>>,
  string | undefined,
  React.Dispatch<React.SetStateAction<string | undefined>>,
] {
  const [value, setValue] = useState<T | undefined>(fallback);
  const [error, setError] = useState<string | undefined>();

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
          setError(String(cause));
        }
      });
    return () => {
      cancelled = true;
    };
    // Intentionally run once on mount for page-level data loads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return [value, setValue, error, setError];
}

import type { LedgerFilter, UsageQuery } from "@any-converter/shared";

/** The hook layer works in explicit ranges, so every usage call sends bounds. */
export function rangeParams(range: { from: string; to: string }): URLSearchParams {
  const params = new URLSearchParams();
  params.set("from", range.from);
  params.set("to", range.to);
  return params;
}

function setOptional(params: URLSearchParams, key: string, value: string | undefined): void {
  if (value !== undefined && value !== "") {
    params.set(key, value);
  }
}

export function usageParams(query: UsageQuery): URLSearchParams {
  const params = rangeParams(query);
  setOptional(params, "granularity", query.granularity);
  setOptional(params, "provider", query.provider);
  setOptional(params, "model", query.model);
  setOptional(params, "agent", query.agent);
  setOptional(params, "split_by", query.split_by);
  return params;
}

export function ledgerParams(filter: LedgerFilter): URLSearchParams {
  const params = rangeParams(filter);
  setOptional(params, "day", filter.day);
  setOptional(params, "provider", filter.provider);
  setOptional(params, "model", filter.model);
  setOptional(params, "agent", filter.agent);
  setOptional(params, "account", filter.account);
  setOptional(params, "query", filter.query);
  if (filter.failed) {
    params.set("failed", "1");
  }
  return params;
}

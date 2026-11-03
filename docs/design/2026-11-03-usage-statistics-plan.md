# Usage Statistics Implementation Plan

> **For agentic workers:** execute one task at a time; each task ends with a verification command whose output must be shown before moving on. Mark `- [x]` only after the verification passes.

**Goal:** Reach parity with magpie's usage subsystem at the level of its **public contract** (`internal/usageapi/usageapi.go`: `Summary`, `Ledger`, `Heatmap`, `Totals`, `Filter`), using SQLite as the single source of truth.

**Architecture decision (validated by measurement):** three layers, one owner each.

| Layer | Owner | Storage |
|---|---|---|
| Write | single blocking writer thread, batched transactions | `request_logs` (narrow) + `request_bodies` (large) |
| Read (aggregate) | `SqliteStorage` | `rollup_hourly` pre-aggregate |
| Read (detail) | `SqliteStorage` | `request_logs` + keyset pagination |

Measured basis (1M rows, 4.2 GB payload, macOS SQLite):

| Approach | 30-day provider×model ranking |
|---|---|
| Wide table, bodies inline (today's shape) | 91.8 s |
| Bodies split out | 1.40 s |
| + `rollup_hourly` | **0.008 s** |

**Tech stack:** Rust 2021 / Axum / rusqlite 0.32 / r2d2; Next.js 14; React 18; `better-sqlite3` 12 (read-only); Tauri.

---

## magpie contract coverage matrix

Every item below is a field or endpoint in magpie's `usageapi` contract. This table is the plan's definition of done — if a row says `⬜` at the end, the plan is not finished.

| # | magpie capability | Where it lands | Status |
|---|---|---|---|
| 1 | `Period`: today / 7d / 30d / all | Task 5.1 | ⬜ |
| 2 | `Period`: picked day range (`2026-10-01..2026-10-07`) | Task 5.1 | ⬜ |
| 3 | `Period`: rolling window (`last:24h`) | Task 5.1 | ⬜ |
| 4 | `Bucket`: hour / day / week | Task 3.4 | ⬜ |
| 5 | `Series`: timeline of `Totals` per point | Task 3.4 | ⬜ |
| 6 | `Series`: each point split by provider/agent/model (`seriesKeep`) | Task 3.5 | ⬜ |
| 7 | `Summary.Models` groups with full `Totals` | Task 3.4 | ⬜ |
| 8 | `Summary.Agents` groups | Task 3.4 + 2.4 | ⬜ |
| 9 | `Summary.Sessions` groups | Task 3.4 + 2.4 | ⬜ |
| 10 | `Summary.Accounts` groups (subscription accounts) | Task 2.6 | ⬜ |
| 11 | `Totals`: calls / errors / input / output / cache_read / cache_write / reasoning | Task 2.2 | ⬜ |
| 12 | `Totals`: latency sum/max, TTFT sum/count | Task 2.2 | ⬜ |
| 13 | `Totals`: `HitRate()` = `CacheRead / (In+CacheRead+CacheWrite−CacheUnknown)` | Task 2.2 | ⬜ |
| 14 | `Totals`: cost at the model's price | Task 2.3 | ⬜ |
| 15 | `Totals.CacheWrite1h` (1-hour TTL writes) | Task 2.7 | ⬜ |
| 16 | `Totals`: decode speed (tokens/s over measured replies) | Task 2.8 | ⬜ |
| 17 | `Record.RouteID` + per-attempt rows + `Totals.Retried` | Task 2.5 | ⬜ |
| 18 | `Record.Served` (the model the vendor said answered) | Task 2.1 | ⬜ |
| 19 | `Ledger`: paged rows + `total` + `offset` | Task 4.1 | ⬜ |
| 20 | `Ledger.Filter`: day / agent / provider / model / failed / retried / query | Task 4.2 | ⬜ |
| 21 | `Ledger.By`: provider/agent/model ranking that stays switchable after one is picked | Task 4.3 | ⬜ |
| 22 | `Ledger` CSV export | Task 7.1 | ⬜ |
| 23 | `Heatmap`: 53 weeks, a day each | Task 6.5 | ⬜ |
| 24 | `Ledger`: three sources (gateway / session file / reported) | **not applicable** | — |
| 25 | `CallerKeys`, `Computers`, `Via` (multi-user / multi-machine) | **not applicable** | — |

Rows 24–25 are excluded because any-converter has no agent-session-file reader, no client-reported ingestion endpoint, no multi-user keys, and no cross-machine sync. If any of those ever ships, `Totals` splits the same way magpie's does and no schema change is needed beyond a `source` column.

**Non-goals:** ClickHouse or any external OLAP engine; multi-process writers; OTel export; full-text search; service tiers (`TierAsked/Sent/Served`); plan credits; loop detection.

---

## Why magpie's own index layer is not reproduced

| magpie module | Lines | Replaced by |
|---|---|---|
| `request_page.go` | 2876 | `select ... where ... limit` + keyset |
| `log_disk.go` | 264 | `journal_mode=wal` + `pragma optimize` |
| `log_index.go` | 411 | `create index` + `analyze` |
| `request_disk.go` | 525 | `request_bodies` table |
| `summary_cache.go` + `summary_fold.go` | 550 | `rollup_hourly` + `ON CONFLICT` |
| `heatmap.go` | 480 | `group by substr(timestamp,1,10)` |
| **≈ 5100** | | **≈ 200 lines of SQL** |

That layer exists because magpie chose JSONL. It is magpie's cost, not its design.

---

## Phase 1 — Storage kernel (no user-visible change)

Ship-alone phase. No API or contract change.

### Task 1.1 — Schema versioning

**Files:** `crates/server/src/storage.rs`, `crates/server/tests/sqlite_storage.rs`

- [ ] Add `const SCHEMA_VERSION: i64 = 4;` and `fn migrate(conn: &Connection)` reading `pragma user_version`, applying steps in order, idempotently.
- [ ] Move today's DDL into step 1 unchanged so existing databases are adopted with no data loss.
- [ ] Test: fresh DB → `user_version == SCHEMA_VERSION`; reopen → no error, no duplicate table.
- [ ] Test: build a DB at `user_version = 1` with the current wide schema, migrate, assert row counts preserved.

**Verify:** `cargo test -p any-converter-server --test sqlite_storage`

### Task 1.2 — Split bodies out of the scan path

**Files:** `crates/server/src/storage.rs`, `crates/server/src/request_log.rs`

- [ ] Step 2 creates:
  ```sql
  create table request_bodies (
    request_id text primary key references request_logs(request_id) on delete cascade,
    request_body_json text,
    upstream_request_body_json text,
    response_body_kind text,
    response_body_json text,
    trace_json text
  ) without rowid;
  ```
- [ ] Metadata and bodies written in one transaction; `recent_request_logs` becomes a `left join` returning a complete `RequestLogRecord`.
- [ ] Backfill in step 2, verify row-for-row, then rebuild `request_logs` without the body columns.
- [ ] Test: a 1 MiB body with `truncated: true` round-trips byte-identically.
- [ ] Test: deleting a parent log row removes its bodies row (cascade).

**Verify:** `cargo test -p any-converter-server --test sqlite_storage`

### Task 1.3 — Delete the `usage_logs` duplicate

`usage_logs` is written on every request and **read by nothing** — every query path uses `request_logs`, and `usage_logs` is a strict field-subset copy.

**Files:** `crates/server/src/storage.rs`, `crates/server/src/usage.rs`, `crates/server/src/router.rs`

- [ ] Step 2 runs `drop table if exists usage_logs;`.
- [ ] Remove `SqliteStorage::insert_usage_record` and the `with_sqlite` plumbing on `UsageLogger`.
- [ ] Test: `rg -n "usage_logs" crates/ apps/ --glob '!target'` returns only the migration line.

### Task 1.4 — Batched writes on a blocking thread

Today `rusqlite` runs synchronously inside `tokio::spawn` (blocking a runtime worker) and every `execute` is its own implicit transaction (one WAL fsync per record).

**Files:** `crates/server/src/storage.rs`, `crates/server/src/request_log.rs`

- [ ] `SqliteStorage::writer()` owns a `std::thread` draining a channel; flush at 256 records or 200 ms; one `conn.transaction()` per batch.
- [ ] Drain and flush on shutdown.
- [ ] Test: 1000 records → all queryable after `flush()`, commits ≤ 5.
- [ ] Test: one malformed row mid-batch does not lose its neighbours.

**Verify:** `cargo test -p any-converter-server --test sqlite_storage`

### Task 1.5 — Retention replaces file-only quota

`disk_quota.rs` **skips** `any-converter.sqlite3`, so the database currently has no bound at all. This must be fixed before SQLite becomes the only store.

**Files:** `crates/server/src/disk_quota.rs`, `crates/server/src/config.rs`, `crates/server/src/storage.rs`

- [ ] `RequestLogConfig.retention_days: u64` (default 30; 0 disables).
- [ ] Step 2 sets `pragma auto_vacuum = incremental`; existing databases get one guarded `VACUUM`.
- [ ] `SqliteStorage::prune_before(cutoff)` → `delete from request_logs where timestamp < ?` then `pragma incremental_vacuum`.
- [ ] Test: rows at 40/20/1 days with `retention_days = 30` → only the oldest goes, with its bodies.
- [ ] Test: `retention_days = 0` deletes nothing.

### Task 1.6 — Keep planner statistics fresh

Measured failure: without statistics SQLite chose `SCAN meta USING INDEX idx_pm` (full 1M-row scan) instead of filtering on `timestamp` — a 65× regression.

**Files:** `crates/server/src/storage.rs`

- [ ] `analyze` once after `migrate`; `pragma optimize` daily in the retention loop.
- [ ] Test: after `analyze`, `explain query plan` for the provider×model aggregate contains `idx_logs_ts` and **not** `SCAN request_logs`.

---

## Phase 2 — Data model

### Task 2.1 — `served_model`, `ttft_ms`, `cache_unknown`

**Files:** `crates/server/src/storage.rs` (step 3), `crates/server/src/request_log.rs`, `crates/core/src/ir/response.rs`

- [ ] Step 3 adds, all `not null default 0` except `served_model`:
  ```sql
  alter table request_logs add column ttft_ms integer not null default 0;
  alter table request_logs add column cache_unknown integer not null default 0;
  alter table request_logs add column served_model text;
  ```
- [ ] `served_model` from the reply's own `model` field, parsed in the same place that already parses usage per format.
- [ ] `ttft_ms` measured request-start → first content token in `handlers.rs`; 0 when not streamed.
- [ ] `cache_unknown` set by a small `fn counts_no_cache(provider: &str) -> bool`, seeded empty — mechanism first, vendor list only when a vendor is observed to behave that way.
- [ ] Test: Claude `cache_read_input_tokens` fills `cache_read_tokens` and leaves `cache_unknown = 0`.
- [ ] Test: a `cache_unknown = 1` row is excluded from the hit-rate denominator.

### Task 2.2 — `Totals` with `hit_rate()`

**Files:** `crates/server/src/storage.rs`

- [ ] `struct Totals { calls, errors, retried, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_write_1h, cache_unknown_tokens, reasoning_tokens, latency_sum_ms, latency_max_ms, ttft_sum_ms, ttft_n, cost_usd }`, `Serialize`.
- [ ] ```rust
  /// Share of the prompt read from the cache, of the calls whose vendor counted it.
  pub fn hit_rate(&self) -> Option<f64> {
      let prompt = self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
          - self.cache_unknown_tokens;
      (prompt > 0).then(|| self.cache_read_tokens as f64 / prompt as f64)
  }
  ```
- [ ] Test: `None` when `cache_unknown_tokens` consumes the prompt.
- [ ] Test: writes are in the denominator, never the numerator.

### Task 2.3 — Cost frozen at write time

**Files:** `crates/server/src/usage.rs`, `crates/server/src/storage.rs`

- [ ] `cost_usd` + `price_version` (hash of the price table) written with the record.
- [ ] Document in `docs/memory/known-gotchas.md`: prices are historical; a price change never rewrites past costs, it needs an explicit recompute pass.
- [ ] Test: two records under different `price_version`s keep their own costs.

### Task 2.4 — Dimension columns the schema is currently dropping

**Files:** `crates/server/src/storage.rs` (step 3), `crates/server/src/request_log.rs`

`client_id` and `session_id` are already captured (`handlers.rs::extract_client_id`, `extract_session_id`) and already in the frontend types — but **`storage.rs` has no columns for them**, so they exist only inside the `record_json` blob and can never be aggregated. Same for `client_format`, which is stored but never indexed.

- [ ] Step 3 adds: `client_id text`, `session_id text`, and an index `idx_logs_client_ts on request_logs(client_id, timestamp)`, `idx_logs_session on request_logs(session_id)`.
- [ ] Backfill from `record_json` with SQLite's JSON1 (`json_extract(record_json,'$.client_id')`).
- [ ] Test: a record with `client_id = "claude-code"` is found by `where client_id = 'claude-code'` after backfill.

**This task is the difference between "provider/model statistics" and "magpie parity".** Without it, `Summary.Agents` and `Summary.Sessions` (matrix rows 8–9) are impossible.

### Task 2.5 — Route identity for failover

**Files:** `crates/server/src/storage.rs` (step 3), `crates/server/src/handlers.rs`

any-converter retries the next provider on a retryable status (`handlers.rs:877`, `943`), which is magpie's "route with several attempts" — but today each attempt is an unrelated row, so a failed try is indistinguishable from a separate request.

- [ ] Step 3 adds `route_id integer` and `attempt integer not null default 0`, index `idx_logs_route on request_logs(route_id)`.
- [ ] Generate `route_id` once per client request in `handlers.rs` and stamp every provider attempt with it plus a zero-based `attempt`.
- [ ] `Totals.retried` = count of routes with more than one attempt; `totals` still sum every attempt's tokens (a failed try that billed tokens is real usage).
- [ ] Test: one request failing on provider A then succeeding on B produces two rows with the same `route_id`, `attempt` 0 and 1, and `retried = 1` in `Totals`.
- [ ] Test: a single-attempt request has `attempt = 0` and contributes nothing to `retried`.

### Task 2.6 — Subscription accounts

**Files:** `crates/server/src/storage.rs` (step 3), `crates/server/src/credentials/mod.rs`

- [ ] Step 3 adds `provider_account text` and index `idx_logs_account on request_logs(provider, provider_account)`.
- [ ] Fill from `AccountResolver` at the point the credential is chosen; empty string for key-based providers.
- [ ] Test: two accounts of the same provider produce two distinct `Summary.Accounts` groups.

### Task 2.7 — `cache_write_1h`

**Files:** `crates/server/src/storage.rs`, `crates/core/src/ir/response.rs`

- [ ] Add `cache_write_1h` to `ir::Usage` and to the Claude reader (`usage.cache_creation` split by `ephemeral_1h_input_tokens`).
- [ ] Test: a Claude reply with `ephemeral_1h_input_tokens: 100` and `ephemeral_5m_input_tokens: 50` yields `cache_write_tokens = 150`, `cache_write_1h = 100`.

### Task 2.8 — Decode speed

**Files:** `crates/server/src/storage.rs`
- [ ] Step 3 adds `decode_ms integer not null default 0` and `decode_out integer not null default 0` (content time and the tokens written in it).
- [ ] `Totals` gains `decode_ms`/`decode_out`; `fn speed(&self) -> Option<f64>` returns `decode_out / (decode_ms/1000)` when both are non-zero.
- [ ] Test: `speed()` is `None` for a reply that was not streamed.

---

## Phase 3 — Pre-aggregation

### Task 3.1 — `rollup_hourly`

**Files:** `crates/server/src/storage.rs` (step 4)

- [ ] ```sql
  create table rollup_hourly (
    bucket text not null, provider text not null, client_model text not null,
    agent text not null default '', route_count integer not null default 0,
    calls integer not null default 0, errors integer not null default 0, retried integer not null default 0,
    input_tokens integer not null default 0, output_tokens integer not null default 0,
    cache_read_tokens integer not null default 0, cache_write_tokens integer not null default 0,
    cache_write_1h integer not null default 0, cache_unknown_tokens integer not null default 0,
    reasoning_tokens integer not null default 0,
    latency_sum_ms integer not null default 0, latency_max_ms integer not null default 0,
    ttft_sum_ms integer not null default 0, ttft_n integer not null default 0,
    decode_ms integer not null default 0, decode_out integer not null default 0,
    cost_usd real not null default 0,
    primary key (bucket, provider, client_model, agent)
  ) without rowid;
  ```
- [ ] `bucket` = UTC hour truncated to `YYYY-MM-DDTHH:00:00Z`, agreeing with `substr(timestamp,1,13)` exactly.
- [ ] **`agent` is in the primary key.** Omitting it would force a second full table for the agent dimension; magpie keeps agents in the same pass.

### Task 3.2 — Maintain it in the write path

**Files:** `crates/server/src/storage.rs`

- [ ] Idempotent upsert **inside the same transaction** as the detail insert, with `latency_max_ms = max(latency_max_ms, excluded.latency_max_ms)`.
- [ ] `route_count`: increment once per distinct `route_id` — implemented as a second tiny table `rollup_routes(hour, route_id)` with `insert or ignore`, counted at read time. Test it; do not fake it with `calls`.
- [ ] Test: 3 calls in one hour/provider/model/agent → one row, `calls = 3`, correct `latency_max_ms`.
- [ ] Test: rollup sums equal `group by` over `request_logs` for 100 random rows.

### Task 3.3 — Rebuild

A pre-aggregate that can only be written forward is a trap: one missed write and the numbers are wrong forever.

**Files:** `crates/server/src/storage.rs`, `crates/cli/src/main.rs`

- [ ] `rebuild_rollup()` → `delete from rollup_hourly; insert ... select ... group by ...` in one transaction, through the single-writer path.
- [ ] Expose as `any-converter stats rebuild` and a Tauri command.
- [ ] Test: corrupt a rollup row, rebuild, assert it matches the `group by` truth.
- [ ] Test: rebuild is safe while the writer is active.

### Task 3.4 — `usage_summary`

**Files:** `crates/server/src/storage.rs`

- [ ] Reads only `rollup_hourly`, bucket expression by granularity:

  | granularity | expression |
  |---|---|
  | `Hour` | `bucket` |
  | `Day` | `substr(bucket,1,10)` |
  | `Week` | `date(bucket,'weekday 1','-7 days')` |
  | `Month` | `substr(bucket,1,7)` |

- [ ] Returns `UsageSummary { totals, series, by_provider, by_agent, by_model, by_account }` from three queries in one deferred read transaction.
- [ ] `by_model` carries its `provider` so the UI can show "model · provider" (magpie's `modelAt`).
- [ ] Test: day granularity over 3 hours of one day → one bucket summing the three.
- [ ] Test: a Sunday/Monday pair lands in different week buckets (Monday-start).
- [ ] Test (gate): 100k rollup rows, 30-day all-dimension summary **< 50 ms**; print the measured number.

### Task 3.5 — Series split by dimension

magpie's `SeriesPoint.By` lets the chart stack by provider/agent/model, keeping the top `seriesKeep = 24` and folding the rest into "other".

**Files:** `crates/server/src/storage.rs`

- [ ] `UsageSummary.series[].by: Option<HashMap<String, Totals>>` populated only when `UsageQuery.split_by` names a dimension — never by default (it multiplies result size).
- [ ] Keep the top 24 by tokens, fold the remainder into a synthetic `"other"` key.
- [ ] Test: 30 distinct providers with `split_by = provider` yields 25 keys, and the folded key's tokens equal the sum of the dropped 6.

---

## Phase 4 — Ledger (the detail layer)

This phase is entirely absent from the first draft of this plan. magpie's `QueryPage`/`LedgerPage` is half of its usage product: without it the UI can only show totals, never answer "which call was that?".

### Task 4.1 — Keyset paging

**Files:** `crates/server/src/storage.rs`

- [ ] `fn ledger_page(&self, f: &Filter, cursor: Option<i64>, limit: u64) -> Ledger` using `where id < ?cursor order by id desc limit ?` — **not** `offset`, which degrades linearly and is what magpie had to work around for ~750k rows.
- [ ] `Ledger { rows, total, bucket, series, totals, by }` with `total` from a separate `count(*)` under the same filter.
- [ ] `limit` clamped to 500, default 100.
- [ ] Test: paging through 250 rows at 100/page yields 100/100/50 with no duplicates and no gaps.
- [ ] Test (gate): cursor page 1000 of a 200k-row table completes in **< 20 ms** (the point of keyset over offset).

### Task 4.2 — Filter

**Files:** `crates/server/src/storage.rs`, `crates/server/src/query.rs`

- [ ] `struct Filter { from, to, day: Option<String>, agent: Option<String>, provider: Option<String>, model: Option<String>, account: Option<String>, failed: bool, retried: bool, query: Option<String> }`.
- [ ] `query` matches `client_model`, `upstream_model`, `served_model`, `provider`, `session_id` case-insensitively (SQLite `like`).
- [ ] `failed` → `response_status >= 400`; `retried` → `route_id in (select route_id from request_logs group by route_id having count(*) > 1)`.
- [ ] Test: `failed = true` returns exactly the rows with status ≥ 400.
- [ ] Test: `retried = true` returns both rows of a two-attempt route, not just the failure.

### Task 4.3 — `By` ranking that stays switchable

magpie's rule: the ranking for the dimension **currently filtered on** is computed **without that filter**, so the other options stay visible to switch to.

**Files:** `crates/server/src/storage.rs`

- [ ] `By { provider, agent, model, model_at }` — each a `Vec<Share { id, name, totals }>` sorted by tokens, reading `rollup_hourly` with the current filter applied but **its own dimension cleared**.
- [ ] Test: filter `provider = "kimi"`; `By.provider` still lists the other providers with non-zero tokens, and `By.model` lists only kimi's models.
- [ ] Test: `By.model_at` labels read `"model · provider"` and never collide across providers.

### Task 4.4 — Day drill-down

- [ ] `Filter.day` restricts to one local calendar day; `series` is then computed **before** the day filter (magpie returns `Series` pre-`Day` so the chart still shows the period).
- [ ] Test: with `day` set, `rows`/`totals` reflect one day while `series` still covers the whole period.

---

## Phase 5 — Query types

**Files:** create `crates/server/src/query.rs`; modify `crates/server/src/lib.rs`

- [ ] `enum Granularity { Hour, Day, Week, Month }`, `FromStr` accepting `hour|day|week|month`.
- [ ] `UsageQuery { from, to, granularity, provider, model, agent, split_by, limit }`.
- [ ] Period resolution in one place, since three surfaces must agree (`cli`, Tauri, Web):
  - presets: `today | 7d | 30d | all`
  - picked range: `<ISO>/<ISO>` (both days inclusive, resolved in the reader's zone)
  - rolling: `last:24h`, `last:90m` (begins that long before now, **not** at a local midnight)
- [ ] Test: `7d` puts `from` exactly 7 days before `to`, both UTC.
- [ ] Test: `last:24h` is a rolling window, so calling it twice an hour apart gives windows 1 hour apart — a preset would give the same calendar day twice.
- [ ] Test: an unknown preset is an error, never a silent fallback to `all`.

---

## Phase 6 — Contract and UI

### Task 6.1 — Shared types

**Files:** `packages/shared/src/types/index.ts`

- [ ] Replace `AggregatedUsage` with `UsageTotals`, `UsageBucket`, `UsageGroup`, `UsageShare`, `UsageSummary`, `LedgerRow`, `Ledger`, `Heatmap`, `Filter`.
- [ ] Keep `UsageRecord` unchanged (it is the IR usage shape, a different thing).

### Task 6.2 — Client and hooks

**Files:** `packages/core/src/hooks/api-client.tsx`, `packages/core/src/hooks/use-usage.ts`

- [ ] `getUsage(q)`, `getLedger(q, cursor)`, `getHeatmap(q)`.
- [ ] Hooks refetch on query change, keep the previous result visible while loading, and cancel a stale in-flight request.

### Task 6.3 — Summary view

**Files:** `packages/views/src/UsageView.tsx`, `packages/core/src/components/usage-chart.tsx`, `packages/core/src/i18n.tsx`

- [ ] Period switch (presets + date range + rolling), granularity switch defaulting by period (today→hour, 7d→day, 30d→day, all→week).
- [ ] Summary cards: total tokens, **cache hit rate** (`—` when `hit_rate()` is `None`, never `0%`), cost, avg/max latency, error rate, retried count.
- [ ] Ranking tables for provider / agent / model / model·provider, each switchable as a filter.
- [ ] Chart stacks by the selected dimension when `split_by` is set.
- [ ] All strings in `i18n.tsx` for both locales; no hardcoded English.

### Task 6.4 — Ledger view

**Files:** create `packages/views/src/LedgerView.tsx`, `apps/web/src/app/usage/...`

- [ ] Table: time, agent, provider, model (requested → served when they differ), tokens, cache, cost, status, latency, route id.
- [ ] Filters wired to `Filter`: agent / provider / model / failed / retried / text query / day drill-down from the chart.
- [ ] Cursor paging ("load more"), with `total` shown.
- [ ] Row expands to the full request/response bodies (already available via `request_bodies`).
- [ ] Test: filtering to `failed` then clearing it restores the full page without a stale cursor.

### Task 6.5 — Heatmap

**Files:** `packages/core/src/components/usage-heatmap.tsx`

- [ ] 53 weeks, Monday-first, one cell per day, shading by tokens, tooltip with calls/tokens/cost.
- [ ] Clicking a day applies `Filter.day` and scrolls to the Ledger.
- [ ] Reads from `rollup_hourly` with `group by substr(bucket,1,10)` — no separate table.

### Task 6.6 — Desktop parity

**Files:** `apps/desktop/src/pages/UsagePage.tsx`, `apps/desktop/src-tauri/src/commands.rs`

- [ ] The Tauri command takes the same `UsageQuery`/`Filter` fields so Web and Desktop can never disagree.
- [ ] Delete `HourlyUsage` and `hourly_usage_from_request_logs` — it uses `min(provider)`/`min(client_model)` as placeholders, which silently attributes a whole hour's tokens to one arbitrary provider. **(User-visible wrong data; goes in CHANGELOG.)**
- [ ] Test: the same query against the same database returns identical totals from Tauri and from SQLite directly.

---

## Phase 7 — Inspectability and export

### Task 7.1 — CSV export

**Files:** `crates/server/src/query.rs`, `packages/core/src/hooks/api-client.tsx`

- [ ] `GET /api/usage/ledger.csv` streaming the same filtered rows as `ledger_page`, header row included.
- [ ] Test: 100 rows export as 101 lines with the expected column order.

### Task 7.2 — Optional JSONL mirror

The only thing magpie's storage choice buys that SQLite loses is inspectability. Recover it without giving up SQL.

- [ ] `RequestLogConfig.jsonl_mirror: bool`, default **false**.
- [ ] Written from the same batch writer **after** the SQLite commit; never read for queries.
- [ ] Documented contract in `docs/memory/known-gotchas.md`: non-authoritative, may lag or lose lines on crash, exists for `jq`/backup/audit.
- [ ] Test: mirror on → N committed records produce N lines; off → no file.

---

## Phase 8 — Verification and docs

### Task 8.1 — Load gate

- [ ] Generate 200k records through the real write path (an `#[ignore]`d release-mode test).
- [ ] Assert: ≥ 5k records/s sustained; 30-day `usage_summary` < 50 ms; cursor page 1000 < 20 ms; DB size < 1.5× stored payload sum.
- [ ] Assert: retention prunes and the file shrinks after `incremental_vacuum`.

### Task 8.2 — Docs

- [ ] `docs/architecture.md`: three-layer storage model + single-writer invariant.
- [ ] `docs/memory/known-gotchas.md`: frozen cost, rollup rebuild requirement, `analyze`/`optimize` necessity, mirror is non-authoritative, keyset-vs-offset.
- [ ] `crates/server/AGENTS.md`: `storage.rs` is the only module that touches SQLite.
- [ ] `CHANGELOG.md`: provider/agent/model/session breakdown, ledger view, and the `min(provider)` correction.
- [ ] `README.md` / `README-zh.md`: only if the config surface changed (`retention_days`, `jsonl_mirror`).

---

## Ordering

```
1.1 → 1.2 →┬→ 1.3
           ├→ 1.4 → 3.2 → 3.3 → 3.4 → 3.5 → 5 → 6
           └→ 1.5
1.6 ───────┘
2.1 → 2.2 → 2.3
2.4 , 2.5 , 2.6 , 2.7 , 2.8 → all feed 3.1/3.4
3.1 → 3.2
4.1 → 4.2 → 4.3 → 4.4      (needs 2.4/2.5; independent of 3.x)
```

Phase 1 ships alone with zero user-visible change. Phase 2 and Task 3.1 can proceed in parallel. Phase 4 (Ledger) is independent of Phase 3 and can be built against `request_logs` directly, which is also its permanent read path. Phases 5–7 are sequential.

## Risks

| Risk | Mitigation |
|---|---|
| Migration loses data on a real user DB | One transaction per step; Task 1.1 tests the adopt path; body backfill verified row-for-row before the old column is dropped |
| Rollup drifts from detail | Same-transaction upsert + Task 3.3 rebuild + Task 3.2 sum-equality test |
| `route_count` corrupted by retries | Counted via `rollup_routes` `insert or ignore`, never inferred from `calls` |
| A second process opens a writer | `open_readonly_in_log_dir` stays the only cross-process path; documented invariant in `crates/server/AGENTS.md` |
| Two aggregators drift (SQL vs JS) | Task 6.6 deletes the JS one; exactly one implementation survives |
| Keyset paging breaks "jump to page N" | Cursor-only UI by design; `total` is still exact, so the count is never wrong |

## Rollback

Phases 1–3 are additive except `drop table usage_logs` (nothing reads it) and the body-table rebuild (verified before drop); Phase 1 reverts by restoring the wide table from `request_bodies`. Phases 4–7 revert by restoring the old `AggregatedUsage` contract; no data changes.

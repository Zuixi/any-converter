//! Detail rows for the request log ("the ledger"): which call was that?
//!
//! The usage views answer "how much in total"; this module answers "which call".
//! It shares one predicate builder between the page, the count, and the totals so
//! the three can never disagree about which rows the filter keeps.

use rusqlite::params_from_iter;
use rusqlite::types::ToSql;
use serde::Serialize;

use super::query::{LedgerFilter, Totals};
use super::{SqliteStorage, StorageError, to_i64};

/// Page size when the caller does not ask for one.
const DEFAULT_LIMIT: u64 = 100;
/// Largest page a caller can ask for; a wider page only costs latency.
const MAX_LIMIT: u64 = 500;
/// Longest ranking a dimension returns; the tail is not worth the round trip.
const MAX_SHARES: usize = 50;

/// The folded aggregate behind [`Totals`], column for column, so the page and
/// the rankings can never report different numbers for the same rows.
pub(super) const TOTALS_AGGREGATES: &str = "\
    sum(case when response_status >= 400 then 1 else 0 end),
    sum(input_tokens), sum(output_tokens),
    sum(coalesce(cache_read_tokens,0)), sum(coalesce(cache_write_tokens,0)),
    sum(cache_write_1h),
    sum(case when cache_unknown = 1
             then input_tokens + coalesce(cache_read_tokens,0) + coalesce(cache_write_tokens,0)
             else 0 end),
    sum(coalesce(reasoning_tokens,0)),
    sum(latency_ms), max(latency_ms),
    sum(ttft_ms), sum(case when ttft_ms > 0 then 1 else 0 end),
    sum(coalesce(cost_usd,0)),
    count(cost_usd),
    sum(case when attempt > 0 and response_status < 400 then 1 else 0 end),
    sum(case when err_type = 'loop' then 1 else 0 end)";

#[derive(Debug, Clone, Serialize)]
pub struct LedgerRow {
    pub request_id: String,
    pub timestamp: String,
    pub provider: String,
    pub client_model: String,
    pub upstream_model: String,
    pub served_model: Option<String>,
    pub agent: Option<String>,
    pub account: Option<String>,
    pub session_id: Option<String>,
    pub status: u16,
    /// Zero-based index of the try that answered: `n` means `n` failovers.
    pub attempt: u32,
    pub streaming: bool,
    pub latency_ms: u64,
    pub ttft_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: Option<f64>,
}

impl LedgerRow {
    /// RFC 4180 CSV of the ledger rows, header included.
    ///
    /// The free [`ledger_csv`] is the implementation; this is the same entry
    /// point through the type the storage module already re-exports.
    pub fn ledger_csv(rows: &[LedgerRow]) -> String {
        ledger_csv(rows)
    }
}

/// RFC 4180 CSV of the ledger rows, header included.
pub fn ledger_csv(rows: &[LedgerRow]) -> String {
    let mut out = String::from(
        "timestamp,provider,client_model,upstream_model,served_model,agent,account,\
         session_id,status,streaming,latency_ms,ttft_ms,input_tokens,output_tokens,\
         cache_read_tokens,cache_write_tokens,reasoning_tokens,cost_usd",
    );
    for row in rows {
        let status = row.status.to_string();
        let streaming = if row.streaming { "true" } else { "false" };
        let latency = row.latency_ms.to_string();
        let ttft = row.ttft_ms.to_string();
        let input = row.input_tokens.to_string();
        let output = row.output_tokens.to_string();
        let cache_read = row.cache_read_tokens.to_string();
        let cache_write = row.cache_write_tokens.to_string();
        let reasoning = row.reasoning_tokens.to_string();
        let cost = row
            .cost_usd
            .map(|value| format!("{value:.6}"))
            .unwrap_or_default();

        out.push('\n');
        push_csv_record(
            &mut out,
            &[
                &row.timestamp,
                &row.provider,
                &row.client_model,
                &row.upstream_model,
                row.served_model.as_deref().unwrap_or(""),
                row.agent.as_deref().unwrap_or(""),
                row.account.as_deref().unwrap_or(""),
                row.session_id.as_deref().unwrap_or(""),
                &status,
                streaming,
                &latency,
                &ttft,
                &input,
                &output,
                &cache_read,
                &cache_write,
                &reasoning,
                &cost,
            ],
        );
    }
    out
}

fn push_csv_record(out: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_csv_field(out, field);
    }
}

fn push_csv_field(out: &mut String, field: &str) {
    if !field.contains([',', '"', '\n', '\r']) {
        out.push_str(field);
        return;
    }
    out.push('"');
    for character in field.chars() {
        if character == '"' {
            out.push('"');
        }
        out.push(character);
    }
    out.push('"');
}

/// One entry of a dimension ranking.
#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(rename = "name")]
    pub label: String,
    pub totals: Totals,
}

/// The ledger's rankings. Each dimension is computed with that dimension's
/// own filter cleared, so the options stay switchable after one is picked.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LedgerBy {
    pub provider: Vec<Share>,
    pub agent: Vec<Share>,
    pub model: Vec<Share>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerPage {
    pub rows: Vec<LedgerRow>,
    /// Rows the filter keeps, on every page, not just this one.
    pub total: u64,
    /// The last row's `id` when this page is full, else `None`.
    pub next_cursor: Option<i64>,
    /// The whole filtered set, not just this page.
    pub totals: Totals,
}

impl SqliteStorage {
    /// One page of detail rows, newest first, with the filtered set's size and totals.
    ///
    /// Paging is keyset, not `offset`: `cursor` is the `id` of the previous page's
    /// last row, so a write landing between two page loads cannot shift a row across
    /// the boundary and make a row appear twice or never.
    pub fn ledger_page(
        &self,
        f: &LedgerFilter,
        cursor: Option<i64>,
        limit: u64,
    ) -> Result<LedgerPage, StorageError> {
        let limit = match limit {
            0 => DEFAULT_LIMIT,
            asked => asked.min(MAX_LIMIT),
        };
        let conn = self.pool.get()?;
        let (predicate, filter_params) = where_clause(f);

        let rows_sql = format!(
            r#"
            select
                id, request_id, timestamp, provider, client_model, upstream_model,
                served_model, client_id, provider_account, session_id,
                response_status, streaming, latency_ms, ttft_ms,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, cost_usd, attempt
            from request_logs
            where {predicate}
              and (? is null or id < ?)
            order by id desc
            limit ?
            "#
        );
        let mut stmt = conn.prepare(&rows_sql)?;

        let mut rows = Vec::new();
        let mut last_id = None;
        {
            let mut page_params: Vec<&dyn ToSql> =
                filter_params.iter().map(|p| p.as_ref()).collect();
            page_params.push(&cursor);
            page_params.push(&cursor);
            let limit_param = to_i64(limit);
            page_params.push(&limit_param);

            let mut query = stmt.query(params_from_iter(page_params))?;
            while let Some(row) = query.next()? {
                last_id = Some(row.get::<_, i64>(0)?);
                rows.push(LedgerRow {
                    request_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    provider: row.get(3)?,
                    client_model: row.get(4)?,
                    upstream_model: row.get(5)?,
                    served_model: row.get(6)?,
                    agent: row.get(7)?,
                    account: row.get(8)?,
                    session_id: row.get(9)?,
                    status: row.get(10)?,
                    streaming: row.get(11)?,
                    latency_ms: u64_column(row, 12)?,
                    ttft_ms: u64_column(row, 13)?,
                    input_tokens: u64_column(row, 14)?,
                    output_tokens: u64_column(row, 15)?,
                    cache_read_tokens: u64_column(row, 16)?,
                    cache_write_tokens: u64_column(row, 17)?,
                    reasoning_tokens: u64_column(row, 18)?,
                    cost_usd: row.get(19)?,
                    attempt: u64_column(row, 20)? as u32,
                });
            }
        }

        // No cursor and no limit: the size of the whole filtered set.
        let total: u64 = conn.query_row(
            &format!("select count(*) from request_logs where {predicate}"),
            params_from_iter(filter_params.iter().map(|p| p.as_ref())),
            |row| row.get(0),
        )?;

        // The same folded aggregate as `usage_summary`, taken from `request_logs`
        // directly. The rollup cannot answer a sub-hour range or an account filter.
        let totals_sql = format!(
            r#"
            select {TOTALS_AGGREGATES}
            from request_logs
            where {predicate}
            "#
        );
        let totals = conn.query_row(
            &totals_sql,
            params_from_iter(filter_params.iter().map(|p| p.as_ref())),
            |row| totals_at(row, 0, total),
        )?;

        // A short page is the last one, so there is nothing left to point at.
        let next_cursor = if rows.len() as u64 == limit {
            last_id
        } else {
            None
        };

        Ok(LedgerPage {
            rows,
            total,
            next_cursor,
            totals,
        })
    }

    /// The ranking of every dimension the filter can name, each one computed with
    /// its own dimension cleared. Picking "kimi" must not empty the provider list:
    /// the only way back to the other providers is that list still holding them.
    pub fn ledger_by(&self, f: &LedgerFilter) -> Result<LedgerBy, StorageError> {
        let conn = self.pool.get()?;

        // Each ranking clones the filter and clears one field, so the other
        // equalities still narrow it: the model list under a provider filter is
        // that provider's models.
        let mut provider_filter = f.clone();
        provider_filter.provider = None;
        let (provider_predicate, provider_params) = where_clause(&provider_filter);
        let provider_sql = format!(
            r#"
            select provider, count(*) as calls, {TOTALS_AGGREGATES}
            from request_logs
            where {provider_predicate}
            group by provider
            order by calls desc, provider asc
            limit {MAX_SHARES}
            "#
        );
        let provider = {
            let mut stmt = conn.prepare(&provider_sql)?;
            let mut query =
                stmt.query(params_from_iter(provider_params.iter().map(|p| p.as_ref())))?;
            let mut shares = Vec::new();
            while let Some(row) = query.next()? {
                let id: String = row.get(0)?;
                shares.push(Share {
                    label: id.clone(),
                    id,
                    provider: None,
                    totals: totals_at(row, 2, u64_column(row, 1)?)?,
                });
            }
            shares
        };

        let mut agent_filter = f.clone();
        agent_filter.agent = None;
        let (agent_predicate, agent_params) = where_clause(&agent_filter);
        let agent_sql = format!(
            r#"
            select coalesce(client_id,'') as agent, count(*) as calls, {TOTALS_AGGREGATES}
            from request_logs
            where {agent_predicate}
            group by agent
            order by calls desc, agent asc
            limit {MAX_SHARES}
            "#
        );
        let agent = {
            let mut stmt = conn.prepare(&agent_sql)?;
            let mut query =
                stmt.query(params_from_iter(agent_params.iter().map(|p| p.as_ref())))?;
            let mut shares = Vec::new();
            while let Some(row) = query.next()? {
                let id: String = row.get(0)?;
                // Rows with no agent are not an option the user can switch to.
                if id.is_empty() {
                    continue;
                }
                shares.push(Share {
                    label: id.clone(),
                    id,
                    provider: None,
                    totals: totals_at(row, 2, u64_column(row, 1)?)?,
                });
            }
            shares
        };

        let mut model_filter = f.clone();
        model_filter.model = None;
        let (model_predicate, model_params) = where_clause(&model_filter);
        let model_sql = format!(
            r#"
            select client_model, provider, count(*) as calls, {TOTALS_AGGREGATES}
            from request_logs
            where {model_predicate}
            group by client_model, provider
            order by calls desc, client_model asc, provider asc
            limit {MAX_SHARES}
            "#
        );
        let model = {
            let mut stmt = conn.prepare(&model_sql)?;
            let mut query =
                stmt.query(params_from_iter(model_params.iter().map(|p| p.as_ref())))?;
            let mut shares = Vec::new();
            while let Some(row) = query.next()? {
                let id: String = row.get(0)?;
                let provider: String = row.get(1)?;
                shares.push(Share {
                    // The same model name on two providers is two different
                    // routes, so the label names the provider it came from.
                    label: format!("{id} · {provider}"),
                    id,
                    provider: Some(provider),
                    totals: totals_at(row, 3, u64_column(row, 2)?)?,
                });
            }
            shares
        };

        Ok(LedgerBy {
            provider,
            agent,
            model,
        })
    }
}

/// The predicate every ledger query shares, with its positional parameters in order.
fn where_clause(f: &LedgerFilter) -> (String, Vec<Box<dyn ToSql>>) {
    let mut clauses = vec!["timestamp >= ?".to_string(), "timestamp < ?".to_string()];
    let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(f.from.clone()), Box::new(f.to.clone())];

    if let Some(day) = &f.day {
        clauses.push("substr(timestamp,1,10) = ?".to_string());
        params.push(Box::new(day.clone()));
    }
    for (column, value) in [
        ("provider", &f.provider),
        ("client_model", &f.model),
        ("client_id", &f.agent),
        ("provider_account", &f.account),
    ] {
        if let Some(value) = value {
            clauses.push(format!("{column} = ?"));
            params.push(Box::new(value.clone()));
        }
    }
    if f.failed {
        clauses.push("response_status >= 400".to_string());
    }
    if f.retried {
        clauses.push("attempt > 0".to_string());
    }
    if let Some(query) = &f.query {
        clauses.push(
            "(lower(client_model) like ? or lower(provider) like ? or \
             lower(coalesce(served_model,'')) like ? or lower(coalesce(session_id,'')) like ?)"
                .to_string(),
        );
        let needle = format!("%{}%", query.to_lowercase());
        for _ in 0..4 {
            params.push(Box::new(needle.clone()));
        }
    }

    (clauses.join(" and "), params)
}

/// Reads [`TOTALS_AGGREGATES`] from `offset`, with the group's row count as `calls`.
pub(super) fn totals_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
    calls: u64,
) -> rusqlite::Result<Totals> {
    Ok(Totals {
        calls,
        errors: u64_column(row, offset)?,
        input_tokens: u64_column(row, offset + 1)?,
        output_tokens: u64_column(row, offset + 2)?,
        cache_read_tokens: u64_column(row, offset + 3)?,
        cache_write_tokens: u64_column(row, offset + 4)?,
        cache_write_1h: u64_column(row, offset + 5)?,
        cache_unknown_tokens: u64_column(row, offset + 6)?,
        reasoning_tokens: u64_column(row, offset + 7)?,
        latency_sum_ms: u64_column(row, offset + 8)?,
        latency_max_ms: u64_column(row, offset + 9)?,
        ttft_sum_ms: u64_column(row, offset + 10)?,
        ttft_calls: u64_column(row, offset + 11)?,
        cost_usd: row.get::<_, Option<f64>>(offset + 12)?.unwrap_or(0.0),
        // The ledger reads `request_logs` directly, so it counts the rows that
        // carry no price against the group's own call count.
        unpriced: calls.saturating_sub(u64_column(row, offset + 13)?),
        retried: u64_column(row, offset + 14)?,
        loops: u64_column(row, offset + 15)?,
        // The aggregates carry no per-reply speed; the ledger row keeps
        // `decode_ms`/`decode_out` for the calls whose speed was measured.
        ..Totals::default()
    })
}

/// A nullable integer column as a non-negative `u64`; `sum()` over no rows is NULL.
fn u64_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, Option<i64>>(index)?.unwrap_or(0).max(0) as u64)
}

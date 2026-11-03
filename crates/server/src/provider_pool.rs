//! Account ordering within one provider (P9).
//!
//! Mirrors magpie's two-layer split: the **user's** account order is
//! authoritative (`internal/provider/account_order.go` — `SetAccountOrder`
//! moves accounts around, never enables a disabled one by a drag), and
//! quota (`internal/provider/allowance*.go`) only moves the accounts past
//! their line to the back — magpie skips the used-up ones without ever
//! reordering the rest.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::credentials::Allowance;

/// The account ids to try for a request, in the user's order, with the
/// ones past their line moved last rather than reordered (magpie keeps the
/// user's order authoritative). Accounts with no allowance information
/// count as not exhausted.
/// Where a provider's account order is kept: `<config>/account-order.json`,
/// beside the secrets file. The provider's own config is the user's to edit,
/// so the order lives on its own and is read when the pool is built.
pub fn order_path(secrets_path: &std::path::Path) -> std::path::PathBuf {
    secrets_path
        .parent()
        .map(|dir| dir.join("account-order.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("account-order.json"))
}

/// Save `ids` as `provider`'s account order. Every id must belong to that
/// provider, and no id may appear twice — an order that disagrees with the
/// accounts that exist would silently drop seats.
pub fn save_order(
    secrets_path: &std::path::Path,
    provider: &str,
    ids: &[String],
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        match id.split_once(':') {
            Some((prefix, _)) if prefix == provider => {}
            _ => return Err(format!("{id} is not an account of {provider}")),
        }
        if !seen.insert(id) {
            return Err(format!("{id} is listed twice"));
        }
    }
    let path = order_path(secrets_path);
    let mut all: std::collections::BTreeMap<String, Vec<String>> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    all.insert(provider.to_string(), ids.to_vec());
    let bytes = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    crate::credentials::store::write_private(&path, &bytes).map_err(|e| e.to_string())
}

/// `provider`'s saved account order, when one was given.
pub fn saved_order(secrets_path: &std::path::Path, provider: &str) -> Option<Vec<String>> {
    let path = order_path(secrets_path);
    let all: std::collections::BTreeMap<String, Vec<String>> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    all.get(provider).cloned()
}

/// `ids` in the user's saved order: the ones it names first, in that order,
/// then any it doesn't know in the order they came.
pub fn apply_order(ids: &[&str], saved: Option<&Vec<String>>) -> Vec<String> {
    let Some(saved) = saved else {
        return ids.iter().map(|id| id.to_string()).collect();
    };
    let mut out: Vec<String> = saved
        .iter()
        .filter(|id| ids.iter().any(|mine| mine == &id.as_str()))
        .cloned()
        .collect();
    for id in ids {
        if !out.iter().any(|taken| taken == id) {
            out.push((*id).to_string());
        }
    }
    out
}

pub fn order_accounts<'a>(
    ids: &'a [&'a str],
    allowances: &HashMap<&'a str, Allowance>,
    model: &str,
    share: f64,
    now: SystemTime,
) -> Vec<&'a str> {
    let mut room = Vec::new();
    let mut spent = Vec::new();
    for id in ids {
        let exhausted = allowances
            .get(id)
            .is_some_and(|allowance| allowance.exhausted(model, share, now));
        if exhausted {
            spent.push(*id);
        } else {
            room.push(*id);
        }
    }
    room.extend(spent);
    room
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::credentials::Limit;

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    fn window(used: f64) -> Allowance {
        Allowance::new(vec![Limit {
            used,
            resets: Some(NOW + std::time::Duration::from_secs(3600)),
            span: std::time::Duration::ZERO,
            model: None,
        }])
    }

    #[test]
    fn test_exhausted_moves_last_unknown_keeps_place() {
        // User order: a (exhausted), b (room left), c (unknown) → the
        // not-exhausted stay in the user's order first, the exhausted
        // move last: [b, c, a].
        let ids = ["a", "b", "c"];
        let mut allowances = HashMap::new();
        // a: exhausted (99 ≥ 95); b: room left; c: unknown (no entry).
        allowances.insert("a", window(99.0));
        allowances.insert("b", window(10.0));

        let ordered = order_accounts(&ids, &allowances, "gemini-3.5-flash", 95.0, NOW);
        assert_eq!(ordered, vec!["b", "c", "a"]);
    }

    #[test]
    fn test_all_exhausted_keeps_the_user_order() {
        let ids = ["a", "b", "c"];
        let mut allowances = HashMap::new();
        allowances.insert("a", window(96.0));
        allowances.insert("b", window(97.0));
        allowances.insert("c", window(98.0));

        let ordered = order_accounts(&ids, &allowances, "gemini-3.5-flash", 95.0, NOW);
        assert_eq!(ordered, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_stable_partition_keeps_order_within_each_half() {
        let ids = ["x", "y", "z", "w"];
        let mut allowances = HashMap::new();
        allowances.insert("x", window(99.0));
        allowances.insert("z", window(99.5));

        let ordered = order_accounts(&ids, &allowances, "gemini-3.5-flash", 95.0, NOW);
        assert_eq!(ordered, vec!["y", "w", "x", "z"]);
    }
}

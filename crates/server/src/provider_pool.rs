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

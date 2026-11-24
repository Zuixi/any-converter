use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{ResolvedMember, RouteStrategy};

static ROUND_ROBIN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Order a route's resolved members for one request.
///
/// `priority` keeps the configured order (primaries, then fallbacks).
/// `round_robin` rotates that order by one seat per request.
/// `weighted` picks a weighted-honouring starting seat per request, then
/// keeps the remaining failover order behind it.
pub(crate) fn order_members(
    members: &[ResolvedMember],
    strategy: &RouteStrategy,
) -> Vec<ResolvedMember> {
    if members.len() <= 1 || *strategy == RouteStrategy::Priority {
        return members.to_vec();
    }

    let (primary, fallback): (Vec<_>, Vec<_>) = members.iter().partition(|member| !member.fallback);
    let candidates = if primary.is_empty() {
        &fallback
    } else {
        &primary
    };
    let tick = ROUND_ROBIN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let start = match strategy {
        RouteStrategy::RoundRobin => tick as usize % candidates.len(),
        RouteStrategy::Weighted => weighted_start(candidates, tick),
        RouteStrategy::Priority => 0,
    };
    let mut ordered: Vec<_> = candidates[start..]
        .iter()
        .chain(candidates[..start].iter())
        .map(|member| (*member).clone())
        .collect();
    if !primary.is_empty() {
        ordered.extend(fallback.into_iter().cloned());
    }
    ordered
}

/// Deterministic weighted pick: fold the tick across the cumulative weights.
fn weighted_start(members: &[&ResolvedMember], tick: u64) -> usize {
    let total: u64 = members
        .iter()
        .map(|member| u64::from(member.weight.max(1)))
        .sum();
    if total == 0 {
        return 0;
    }
    let mut point = tick % total;
    for (index, member) in members.iter().enumerate() {
        let weight = u64::from(member.weight.max(1));
        if point < weight {
            return index;
        }
        point -= weight;
    }
    members.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(provider: &str, weight: u32) -> ResolvedMember {
        ResolvedMember {
            provider: provider.to_string(),
            upstream_model: "m".to_string(),
            weight,
            fallback: false,
        }
    }

    #[test]
    fn test_round_robin_rotates_provider_order() {
        let members = vec![
            member("primary", 1),
            member("backup", 1),
            member("third", 1),
        ];
        let first = order_members(&members, &RouteStrategy::RoundRobin);
        let second = order_members(&members, &RouteStrategy::RoundRobin);

        let names = |list: Vec<ResolvedMember>| -> Vec<String> {
            list.into_iter().map(|member| member.provider).collect()
        };
        assert_eq!(names(first), vec!["primary", "backup", "third"]);
        assert_eq!(names(second), vec!["backup", "third", "primary"]);
    }

    #[test]
    fn test_priority_keeps_order() {
        let members = vec![member("primary", 1), member("backup", 5)];
        let ordered = order_members(&members, &RouteStrategy::Priority);
        assert_eq!(ordered[0].provider, "primary");
        assert_eq!(ordered[1].provider, "backup");
    }

    #[test]
    fn test_weighted_start_follows_weights() {
        let members = [member("heavy", 3), member("light", 1)];
        let refs: Vec<_> = members.iter().collect();
        let starts: Vec<&str> = (0..4)
            .map(|tick| refs[weighted_start(&refs, tick)].provider.as_str())
            .collect();
        assert_eq!(starts, vec!["heavy", "heavy", "heavy", "light"]);
    }
}

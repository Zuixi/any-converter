use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::RouteStrategy;

static ROUND_ROBIN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn order_provider_names(
    provider_names: &[String],
    strategy: &RouteStrategy,
) -> Vec<String> {
    if provider_names.len() <= 1 || *strategy == RouteStrategy::Priority {
        return provider_names.to_vec();
    }

    let start =
        (ROUND_ROBIN_COUNTER.fetch_add(1, Ordering::Relaxed) as usize) % provider_names.len();
    provider_names[start..]
        .iter()
        .chain(provider_names[..start].iter())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin_rotates_provider_order() {
        let providers = vec![
            "primary".to_string(),
            "backup".to_string(),
            "third".to_string(),
        ];
        let first = order_provider_names(&providers, &RouteStrategy::RoundRobin);
        let second = order_provider_names(&providers, &RouteStrategy::RoundRobin);

        assert_eq!(first, vec!["primary", "backup", "third"]);
        assert_eq!(second, vec!["backup", "third", "primary"]);
    }
}

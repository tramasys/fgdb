use std::{borrow::Borrow, collections::HashMap, hash::Hash, time::Duration};

/// Soft responsiveness targets. Crossing one records a diagnostic and causes
/// the caller to yield, paginate, defer, or shed optional work. These are not
/// correctness timeouts. Those remain owned by the MI transport.
pub(crate) const UI_RENDER_BUDGET: Duration = Duration::from_millis(16);
pub(crate) const MI_INSPECTION_BUDGET: Duration = Duration::from_millis(750);
pub(crate) const MI_CONTROL_BUDGET: Duration = Duration::from_secs(2);
pub(crate) const MI_BACKGROUND_BUDGET: Duration = Duration::from_secs(5);
pub(crate) const MI_SCOPED_QUEUE_BUDGET: Duration = Duration::from_millis(500);
pub(crate) const THREAD_WIDGET_BUDGET: usize = 256;
pub(crate) const THREAD_SELECTOR_BUDGET: usize = 512;
pub(crate) const STACK_FRAME_WIDGET_BUDGET: usize = 512;
pub(crate) const MODULE_WIDGET_BUDGET: usize = 256;
pub(crate) const STOP_POINT_WIDGET_BUDGET: usize = 512;
pub(crate) const LOCALS_ROOT_PAGE_SIZE: usize = 512;
pub(crate) const MODULE_METADATA_FILE_BUDGET: usize = 512;
pub(crate) const MODULE_METADATA_TIME_BUDGET: Duration = Duration::from_secs(3);
pub(crate) const RESOLVED_SOURCE_PATH_CACHE_BUDGET: usize = 4_096;
pub(crate) const DISASSEMBLY_SOURCE_CACHE_BUDGET: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetOutcome {
    Slow,
    Partial,
    Deferred,
    Rejected,
    Evicted,
}

impl BudgetOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Partial => "partial",
            Self::Deferred => "deferred",
            Self::Rejected => "rejected",
            Self::Evicted => "evicted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PerformanceNotice {
    pub(crate) outcome: BudgetOutcome,
    pub(crate) operation: String,
    pub(crate) detail: String,
}

impl PerformanceNotice {
    pub(crate) fn slow(operation: impl Into<String>, elapsed: Duration, budget: Duration) -> Self {
        Self {
            outcome: BudgetOutcome::Slow,
            operation: operation.into(),
            detail: format!(
                "completed in {} (soft budget {})",
                format_duration(elapsed),
                format_duration(budget)
            ),
        }
    }

    pub(crate) fn count(
        outcome: BudgetOutcome,
        operation: impl Into<String>,
        shown: usize,
        total: usize,
    ) -> Self {
        Self {
            outcome,
            operation: operation.into(),
            detail: format!("showing {shown} of {total} entries"),
        }
    }

    pub(crate) fn message(&self) -> String {
        format!(
            "Performance budget: {} was {} — {}",
            self.operation,
            self.outcome.label(),
            self.detail
        )
    }
}

pub(crate) fn duration_notice(
    operation: impl Into<String>,
    elapsed: Duration,
    budget: Duration,
) -> Option<PerformanceNotice> {
    (elapsed > budget).then(|| PerformanceNotice::slow(operation, elapsed, budget))
}

#[derive(Clone, Debug)]
struct AdaptiveLimit {
    current: usize,
    minimum: usize,
    maximum: usize,
    fast_samples: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenderLimitAdjustment {
    pub(crate) previous: usize,
    pub(crate) current: usize,
}

/// Learns conservative widget page sizes from actual GTK construction time.
/// Limits fall quickly after a missed frame and recover slowly after several
/// consistently cheap renders, avoiding oscillation on heterogeneous hosts.
#[derive(Default)]
pub(crate) struct AdaptiveRenderBudgets {
    limits: HashMap<String, AdaptiveLimit>,
}

impl AdaptiveRenderBudgets {
    pub(crate) fn limit(&mut self, operation: &str, default: usize, minimum: usize) -> usize {
        if let Some(limit) = self.limits.get(operation) {
            return limit.current;
        }

        self.limits
            .entry(operation.to_owned())
            .or_insert(AdaptiveLimit {
                current: default,
                minimum: minimum.min(default).max(1),
                maximum: default.max(1),
                fast_samples: 0,
            })
            .current
    }

    pub(crate) fn observe(
        &mut self,
        operation: &str,
        elapsed: Duration,
    ) -> Option<RenderLimitAdjustment> {
        let limit = self.limits.get_mut(operation)?;
        let previous = limit.current;

        if elapsed > UI_RENDER_BUDGET {
            limit.current = limit.current.div_ceil(2).max(limit.minimum);
            limit.fast_samples = 0;
        } else if elapsed <= UI_RENDER_BUDGET / 2 {
            limit.fast_samples = limit.fast_samples.saturating_add(1);

            if limit.fast_samples >= 8 {
                limit.current = limit
                    .current
                    .saturating_add(limit.minimum)
                    .min(limit.maximum);

                limit.fast_samples = 0;
            }
        } else {
            limit.fast_samples = 0;
        }

        (previous != limit.current).then_some(RenderLimitAdjustment {
            previous,
            current: limit.current,
        })
    }
}

fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

#[derive(Clone, Debug)]
struct CacheEntry<K, V> {
    key: K,
    value: V,
    previous: Option<usize>,
    next: Option<usize>,
}

/// Bounded derived data with O(1) expected lookup, promotion, and eviction.
/// Entries form an index-linked list from least to most recently used. Slots
/// are reused on eviction, so access history cannot grow memory or wrap a clock.
/// Eviction never touches authoritative debugger lifecycle state.
#[derive(Clone, Debug)]
pub(crate) struct BoundedLruCache<K, V> {
    indices: HashMap<K, usize>,
    entries: Vec<CacheEntry<K, V>>,
    capacity: usize,
    oldest: Option<usize>,
    newest: Option<usize>,
}

impl<K, V> BoundedLruCache<K, V>
where
    K: Clone + Eq + Hash,
{
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            indices: HashMap::new(),
            entries: Vec::new(),
            capacity,
            oldest: None,
            newest: None,
        }
    }

    pub(crate) fn get_cloned<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
        V: Clone,
    {
        let index = *self.indices.get(key)?;
        self.promote(index);

        Some(self.entries[index].value.clone())
    }

    /// Returns true when an entry was evicted or capacity is zero.
    pub(crate) fn insert(&mut self, key: K, value: V) -> bool {
        if let Some(&index) = self.indices.get(&key) {
            self.entries[index].value = value;
            self.promote(index);
            return false;
        }

        if self.capacity == 0 {
            return true;
        }

        let evicted = self.entries.len() == self.capacity;

        let index = if evicted {
            let index = self
                .oldest
                .expect("a full nonempty cache has an oldest entry");
            self.indices.remove(&self.entries[index].key);
            self.detach(index);
            index
        } else {
            self.entries.len()
        };

        self.indices.insert(key.clone(), index);
        let entry = CacheEntry {
            key,
            value,
            previous: self.newest,
            next: None,
        };

        if evicted {
            self.entries[index] = entry;
        } else {
            self.entries.push(entry);
        }

        self.append(index);

        evicted
    }

    fn detach(&mut self, index: usize) {
        let CacheEntry { previous, next, .. } = self.entries[index];

        if let Some(previous) = previous {
            self.entries[previous].next = next;
        } else {
            self.oldest = next;
        }

        if let Some(next) = next {
            self.entries[next].previous = previous;
        } else {
            self.newest = previous;
        }
    }

    fn append(&mut self, index: usize) {
        if let Some(newest) = self.newest {
            self.entries[newest].next = Some(index);
        } else {
            self.oldest = Some(index);
        }

        self.newest = Some(index);
    }

    fn promote(&mut self, index: usize) {
        if self.newest == Some(index) {
            return;
        }

        self.detach(index);
        self.entries[index].previous = self.newest;
        self.entries[index].next = None;
        self.append(index);
    }

    pub(crate) fn clear(&mut self) {
        self.indices.clear();
        self.entries.clear();
        self.oldest = None;
        self.newest = None;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_budget_only_reports_actual_breaches() {
        assert!(duration_notice("render", Duration::from_millis(16), UI_RENDER_BUDGET).is_none());

        let notice =
            duration_notice("render", Duration::from_millis(17), UI_RENDER_BUDGET).unwrap();

        assert_eq!(notice.outcome, BudgetOutcome::Slow);
        assert!(notice.message().contains("17 ms"));
    }

    #[test]
    fn count_notices_make_partial_results_explicit() {
        let notice = PerformanceNotice::count(BudgetOutcome::Partial, "threads", 256, 1_024);

        assert_eq!(
            notice.message(),
            "Performance budget: threads was partial — showing 256 of 1024 entries"
        );
    }

    #[test]
    fn bounded_cache_evicts_the_least_recently_used_entry() {
        let mut cache = BoundedLruCache::new(2);
        assert!(!cache.insert(String::from("a"), 1));
        assert!(!cache.insert(String::from("b"), 2));
        assert_eq!(cache.get_cloned("a"), Some(1));
        assert!(cache.insert(String::from("c"), 3));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get_cloned("a"), Some(1));
        assert_eq!(cache.get_cloned("b"), None);
        assert_eq!(cache.get_cloned("c"), Some(3));
    }

    #[test]
    fn bounded_cache_matches_recency_model_through_updates_and_resets() {
        for capacity in 0..=8 {
            let mut cache = BoundedLruCache::new(capacity);
            let mut model = std::collections::VecDeque::new();
            let mut random = 123_u32;

            for value in 0..4096 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let key = (random >> 16) % 17;
                let previous = model
                    .iter()
                    .position(|(existing, _)| *existing == key)
                    .and_then(|index| model.remove(index));

                match random % 11 {
                    0 => {
                        cache.clear();
                        model.clear();
                    }
                    1..=4 => {
                        assert_eq!(cache.get_cloned(&key), previous.map(|(_, value)| value));

                        if let Some(previous) = previous {
                            model.push_back(previous);
                        }
                    }
                    _ => {
                        let evicted = previous.is_none() && model.len() == capacity;
                        assert_eq!(cache.insert(key, value), evicted);

                        if evicted {
                            model.pop_front();
                        }

                        if capacity != 0 {
                            model.push_back((key, value));
                        }
                    }
                }

                assert_eq!(cache.len(), model.len());
                assert_eq!(cache.indices.len(), model.len());
                let mut index = cache.oldest;
                let mut previous = None;

                for &(key, value) in &model {
                    let current = index.unwrap();
                    let entry = &cache.entries[current];
                    assert_eq!((entry.key, entry.value), (key, value));
                    assert_eq!(entry.previous, previous);
                    assert_eq!(cache.indices.get(&key), Some(&current));
                    previous = index;
                    index = entry.next;
                }

                assert_eq!(index, None);
                assert_eq!(cache.newest, previous);
            }
        }
    }

    #[test]
    fn zero_capacity_cache_never_retains_derived_data() {
        let mut cache = BoundedLruCache::new(0);
        assert!(cache.insert("unused", 1));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get_cloned(&"unused"), None);
    }

    #[test]
    fn adaptive_render_limits_shed_fast_and_recover_slowly() {
        let mut budgets = AdaptiveRenderBudgets::default();
        assert_eq!(budgets.limit("threads", 256, 32), 256);

        assert_eq!(
            budgets
                .observe("threads", Duration::from_millis(20))
                .unwrap()
                .current,
            128
        );

        for _ in 0..7 {
            assert!(
                budgets
                    .observe("threads", Duration::from_millis(4))
                    .is_none()
            );
        }

        assert_eq!(
            budgets
                .observe("threads", Duration::from_millis(4))
                .unwrap()
                .current,
            160
        );
    }
}

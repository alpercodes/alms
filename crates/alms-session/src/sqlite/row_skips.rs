//! Accounting for durable rows a loader could not parse and therefore dropped
//! (#1241).
//!
//! Every list-shaped loader in this crate ends in a `filter_map` that discards
//! rows it cannot turn into a domain type. That is the right *policy* — see
//! "Reconciliation policy: absence must be a safe belief" in
//! `docs/architecture.md` — but until #1241 it was applied silently: a corrupt
//! `agents` row vanished from the registry, a corrupt `sessions` row vanished
//! from the sidebar, and the only trace was a `warn!` line in a log nobody was
//! tailing.
//!
//! Three of the counted sites are **not** loaders — `delete_agent` (twice) and
//! `migrate_telegram_context_ids` collect ids in order to delete or rewrite
//! them, so a dropped row there leaves durable orphans rather than merely
//! going unserved. They are the policy's "third branch"; each names its own
//! leak in a comment and prefixes its log `detail` with the site, because they
//! land in the same per-table slot as the loader drops.
//!
//! The quarantine rule requires a site to *count* what it drops, not just log
//! it. This module is that counter: one process-lifetime total per table,
//! surfaced as `persistence_rows_skipped_total` on `GET /operations/metrics`.
//!
//! The table set is a closed enum rather than a string-keyed map on purpose —
//! the metrics payload then has a fixed, discoverable shape (every table is
//! reported, including the zeroes) and a typo at a call site is a compile
//! error rather than a phantom series.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A durable table whose loaders may quarantine a row they cannot parse.
///
/// The string form matches the SQL table name so an operator reading the
/// metrics payload can go straight to `sqlite3`. The one exception is
/// [`PersistenceTable::Timeline`], which is a `UNION` query across `messages`
/// and `runs` rather than a table of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PersistenceTable {
    Agents,
    AuditEvents,
    Jobs,
    Messages,
    RunToolCalls,
    Runs,
    SessionSummaries,
    Sessions,
    /// Not a table — the `messages`/`runs` `UNION` behind `GET /timeline`.
    Timeline,
}

impl PersistenceTable {
    /// Every table, in the order they are reported. Adding a variant here is
    /// all that is needed for it to appear on `/operations/metrics`.
    pub const ALL: [Self; 9] = [
        Self::Agents,
        Self::AuditEvents,
        Self::Jobs,
        Self::Messages,
        Self::RunToolCalls,
        Self::Runs,
        Self::SessionSummaries,
        Self::Sessions,
        Self::Timeline,
    ];

    /// Stable metric label for this table.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::AuditEvents => "audit_events",
            Self::Jobs => "jobs",
            Self::Messages => "messages",
            Self::RunToolCalls => "run_tool_calls",
            Self::Runs => "runs",
            Self::SessionSummaries => "session_summaries",
            Self::Sessions => "sessions",
            Self::Timeline => "timeline",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Agents => 0,
            Self::AuditEvents => 1,
            Self::Jobs => 2,
            Self::Messages => 3,
            Self::RunToolCalls => 4,
            Self::Runs => 5,
            Self::SessionSummaries => 6,
            Self::Sessions => 7,
            Self::Timeline => 8,
        }
    }
}

/// Process-lifetime skip counts, one slot per [`PersistenceTable`].
///
/// Shared by every clone of a `SqliteStore` (they all wrap the same
/// connection), so the totals describe the database, not a handle to it.
#[derive(Debug)]
pub(super) struct RowSkipCounters {
    per_table: [AtomicU64; PersistenceTable::ALL.len()],
}

impl Default for RowSkipCounters {
    fn default() -> Self {
        Self {
            per_table: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl RowSkipCounters {
    pub(super) fn record(&self, table: PersistenceTable) {
        self.per_table[table.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn get(&self, table: PersistenceTable) -> u64 {
        self.per_table[table.index()].load(Ordering::Relaxed)
    }

    pub(super) fn total(&self) -> u64 {
        self.per_table
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .sum()
    }

    pub(super) fn by_table(&self) -> BTreeMap<&'static str, u64> {
        PersistenceTable::ALL
            .iter()
            .map(|table| (table.as_str(), self.get(*table)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_has_a_distinct_slot() {
        let mut indexes: Vec<usize> = PersistenceTable::ALL.iter().map(|t| t.index()).collect();
        indexes.sort_unstable();
        indexes.dedup();
        assert_eq!(
            indexes.len(),
            PersistenceTable::ALL.len(),
            "two PersistenceTable variants share a counter slot"
        );
        assert!(
            indexes.iter().all(|i| *i < PersistenceTable::ALL.len()),
            "a PersistenceTable index is out of bounds"
        );
    }

    #[test]
    fn every_table_has_a_distinct_label() {
        let mut labels: Vec<&str> = PersistenceTable::ALL.iter().map(|t| t.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), PersistenceTable::ALL.len());
    }

    #[test]
    fn counters_are_per_table_and_sum_to_the_total() {
        let counters = RowSkipCounters::default();
        assert_eq!(counters.total(), 0);
        assert!(counters.by_table().values().all(|v| *v == 0));

        counters.record(PersistenceTable::Agents);
        counters.record(PersistenceTable::Agents);
        counters.record(PersistenceTable::Sessions);

        assert_eq!(counters.get(PersistenceTable::Agents), 2);
        assert_eq!(counters.get(PersistenceTable::Sessions), 1);
        assert_eq!(counters.get(PersistenceTable::Runs), 0);
        assert_eq!(counters.total(), 3);

        let by_table = counters.by_table();
        assert_eq!(by_table.len(), PersistenceTable::ALL.len());
        assert_eq!(by_table["agents"], 2);
        assert_eq!(by_table["sessions"], 1);
        assert_eq!(by_table["runs"], 0);
    }
}

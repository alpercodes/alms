// SPDX-License-Identifier: Apache-2.0

//! Accounting for durable *columns* a parser could not read and replaced with a
//! fallback, keeping the row (#1246).
//!
//! This is the sibling of [`super::row_skips`], and the distinction between
//! them is the whole point. A row skip means the daemon **declined to believe**
//! a row: it is dropped, kept out of live state, and the reconciliation
//! policy's obligation 3 ("the quarantined fact must not reach live state") is
//! discharged. A field degradation is the opposite shape — the row *is*
//! projected into live state, carrying one column the parser could not read.
//! Obligation 3 cannot be discharged here, because there is no way to withhold
//! a single column without dropping the row, and for these sites dropping is
//! strictly worse (see below).
//!
//! That makes this counter the more alarming of the two despite the row
//! surviving, and it is why these sites must not increment
//! `persistence_rows_skipped_total`: that number means "rows the daemon cannot
//! see", and these rows are perfectly visible, just wrong.
//!
//! Note what "worse" does and does not mean here. It is about *containment*,
//! not about blast radius per site: a quarantined row is bounded by
//! construction and a degraded one is not, so the class deserves the louder
//! alert. Three of the four sites below are attribution defects on boot-path
//! rows. The fourth, `session_summaries.last_run_id`, is the one whose
//! individual damage matches the class's reputation.
//!
//! ## Why these sites degrade instead of dropping
//!
//! Apply the policy's test — *if I drop this row entirely, does the daemon do
//! anything it would not have done had the row been correct?*
//!
//! At the two `runs.*` fields the answer is decided by where `parse_run_row`
//! is actually reached from. In production it has exactly two readers, both
//! boot-only: the stale-run sweep in `Gateway::new`
//! (`alms-gateway/src/gateway.rs:374`) and `load_all_runs` during
//! `hydrate_from_store`. `load_run` and `load_runs_by_session` have no
//! production callers at all. So dropping buys nothing and costs the boot:
//!
//! - `mark_stale_runs_failed` collects its sweep with
//!   `collect::<Result<_, _>>()`, and `Gateway::new` propagates that with `?`.
//!   A row-level parse error there is therefore an **unbootable daemon**,
//!   which is the exact outcome the reconciliation policy exists to prevent
//!   (#1236). (`hydrate_from_store`'s own call to the same sweep swallows the
//!   error and returns, so it is specifically the `gateway.rs` call that makes
//!   this fatal — checking only `hydrate_from_store` leads to the opposite
//!   conclusion.)
//! - The run vanishes from session history and from startup hydration, so the
//!   stale-run sweep can never reconcile it and the durable row claims
//!   `queued`/`running` forever.
//!
//! What a degraded `runs.job_id` actually costs is **attribution**: `GET /runs`
//! reports `job_id: null` and `derive_trigger` labels the run `"user"` instead
//! of `"scheduled"`. It is *not* a cancellation hazard — `cancel_runs_for_job`
//! only considers `Queued`/`Running` runs, and `hydrate_from_store` refuses to
//! project `Queued`/`Running` rows into the live map at all (#1236/#1239), so
//! every run that reaches the live map through this parser is terminal by
//! construction. Live runs enter through `insert_persisted_run` carrying a
//! `Run` built in memory, whose `job_id` never round-trips through here.
//! `runs.parent_run_id` is the same shape again: `"subagent"` reads as
//! `"user"`, plus a null `parent_session_id` breadcrumb.
//!
//! `session_summaries.last_run_id` is the one that is not attribution at all,
//! and it is the only one on a **live read path**. That column is the
//! compare-and-swap sentinel for episodic summary upserts (#1123). Degraded to
//! `None`, `upsert_session_summary_optimistic` takes its
//! `WHERE last_run_id IS NULL` branch, matches zero rows against the non-NULL
//! garbage cell, falls through to the `INSERT`, trips the unique constraint,
//! and reports a *conflict*. The caller in `alms-runtime/src/episodic.rs`
//! reloads (getting the same degraded `None`) and retries three times, burning
//! a fresh LLM summarization call each attempt, then logs a concurrency error
//! that names a cause which is not the cause. Episodic memory for that session
//! can never be updated again. Dropping the row instead would silently lose
//! the summary and re-summarize from scratch; the counter plus the `warn!` are
//! what turn a permanent, self-misdiagnosing deadlock into a named fault.
//!
//! `agents.name` is a different shape again: it does not mis-attribute a row,
//! it makes `delete_agent` skip its DM-cleanup branch entirely, stranding
//! shared DM sessions whose participants are all gone. That stranding is
//! *additive* — nothing is deleted that should have survived — which is what
//! makes it quarantinable under the policy's third branch.
//!
//! The field set is a closed enum for the same reason [`super::row_skips`] uses
//! one: the metrics payload gets a fixed, discoverable shape (every field is
//! reported, including the zeroes) and a typo at a call site is a compile error
//! rather than a phantom series.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A durable column whose parser may fall back rather than fail the row.
///
/// The string form is `<table>.<column>` so an operator reading the metrics
/// payload can go straight to `sqlite3` with a `SELECT` they can write without
/// consulting the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DegradedField {
    /// `delete_agent` could not read `agents.name` for the agent being
    /// deleted, so it skipped the DM-cleanup branch.
    AgentsName,
    /// `runs.job_id` failed to parse as a UUID. The run is hydrated with
    /// `job_id: None`, so it is no longer attributable to its job: `GET /runs`
    /// reports a null job and the run is labelled `"user"` rather than
    /// `"scheduled"`.
    RunsJobId,
    /// `runs.parent_run_id` failed to parse as a UUID. The run is hydrated
    /// with `parent_run_id: None`, so it reads as top-level rather than as a
    /// subagent run of its parent.
    RunsParentRunId,
    /// `session_summaries.last_run_id` failed to parse as a UUID. The summary
    /// is loaded with `last_run_id: None`, which is not just an attribution
    /// defect: that value is the compare-and-swap sentinel for episodic
    /// summary upserts (#1123), so the degraded read deadlocks the write.
    SessionSummariesLastRunId,
}

impl DegradedField {
    /// Every field, in the order they are reported. Adding a variant here is
    /// all that is needed for it to appear on `/operations/metrics`.
    pub const ALL: [Self; 4] = [
        Self::AgentsName,
        Self::RunsJobId,
        Self::RunsParentRunId,
        Self::SessionSummariesLastRunId,
    ];

    /// Stable metric label for this field, in `<table>.<column>` form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentsName => "agents.name",
            Self::RunsJobId => "runs.job_id",
            Self::RunsParentRunId => "runs.parent_run_id",
            Self::SessionSummariesLastRunId => "session_summaries.last_run_id",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::AgentsName => 0,
            Self::RunsJobId => 1,
            Self::RunsParentRunId => 2,
            Self::SessionSummariesLastRunId => 3,
        }
    }
}

/// Process-lifetime degradation counts, one slot per [`DegradedField`].
///
/// Shared by every clone of a `SqliteStore` (they all wrap the same
/// connection), so the totals describe the database, not a handle to it.
#[derive(Debug)]
pub(super) struct FieldDegradationCounters {
    per_field: [AtomicU64; DegradedField::ALL.len()],
}

impl Default for FieldDegradationCounters {
    fn default() -> Self {
        Self {
            per_field: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl FieldDegradationCounters {
    pub(super) fn record(&self, field: DegradedField) {
        self.per_field[field.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn get(&self, field: DegradedField) -> u64 {
        self.per_field[field.index()].load(Ordering::Relaxed)
    }

    pub(super) fn total(&self) -> u64 {
        self.per_field
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .sum()
    }

    pub(super) fn by_field(&self) -> BTreeMap<&'static str, u64> {
        DegradedField::ALL
            .iter()
            .map(|field| (field.as_str(), self.get(*field)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_a_distinct_slot() {
        let mut indexes: Vec<usize> = DegradedField::ALL.iter().map(|f| f.index()).collect();
        indexes.sort_unstable();
        indexes.dedup();
        assert_eq!(
            indexes.len(),
            DegradedField::ALL.len(),
            "two DegradedField variants share a counter slot"
        );
        assert!(
            indexes.iter().all(|i| *i < DegradedField::ALL.len()),
            "a DegradedField index is out of bounds"
        );
    }

    #[test]
    fn every_field_has_a_distinct_label() {
        let mut labels: Vec<&str> = DegradedField::ALL.iter().map(|f| f.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), DegradedField::ALL.len());
    }

    #[test]
    fn every_label_is_table_dot_column() {
        for field in DegradedField::ALL {
            let label = field.as_str();
            assert_eq!(
                label.split('.').count(),
                2,
                "{label} is not in <table>.<column> form"
            );
            assert!(
                label.split('.').all(|part| !part.is_empty()),
                "{label} has an empty table or column part"
            );
        }
    }

    /// The doctrine in `docs/architecture.md` and `docs/api.md` presents this
    /// set as the *complete* inventory of foreign-key field degradation in
    /// `alms-session`. #1246 originally closed it at three and missed
    /// `session_summaries.last_run_id`, so the claim is pinned here: adding a
    /// site without updating the docs has to fail this test first.
    #[test]
    fn the_inventory_is_exactly_these_fields() {
        let labels: Vec<&str> = DegradedField::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(
            labels,
            [
                "agents.name",
                "runs.job_id",
                "runs.parent_run_id",
                "session_summaries.last_run_id",
            ],
            "the degradation inventory changed -- update docs/architecture.md's scope-note table, docs/api.md 8.1, and the site-table denominator"
        );
    }

    #[test]
    fn counters_are_per_field_and_sum_to_the_total() {
        let counters = FieldDegradationCounters::default();
        assert_eq!(counters.total(), 0);
        assert!(counters.by_field().values().all(|v| *v == 0));

        counters.record(DegradedField::RunsJobId);
        counters.record(DegradedField::RunsJobId);
        counters.record(DegradedField::AgentsName);

        assert_eq!(counters.get(DegradedField::RunsJobId), 2);
        assert_eq!(counters.get(DegradedField::AgentsName), 1);
        assert_eq!(counters.get(DegradedField::RunsParentRunId), 0);
        assert_eq!(counters.total(), 3);

        let by_field = counters.by_field();
        assert_eq!(by_field.len(), DegradedField::ALL.len());
        assert_eq!(by_field["runs.job_id"], 2);
        assert_eq!(by_field["agents.name"], 1);
        assert_eq!(by_field["runs.parent_run_id"], 0);
        assert_eq!(by_field["session_summaries.last_run_id"], 0);
    }
}

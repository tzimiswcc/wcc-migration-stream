//! Bounded, value-free WP-006 reconciliation and result-approval reads.
//!
//! Every query opens the controlled `SQLite` database read-only, verifies prior
//! decision and WP-006 evidence, and executes inside one deferred snapshot.
//! Broad responses expose identities, counts, reason codes, and hashes only;
//! they never expose accepted source, overlay, or effective canonical values.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, decode_cursor, encode_cursor, parse_unique_json},
    read_model::{PageInfo, PageRequest},
    reconciliation_approval::{MigrationResultCandidate, ResultAction},
};

const SNAPSHOT_LIST_SCHEMA: &str = "migration-stream/read/result-reconciliation-snapshot-list/v1";
const SNAPSHOT_SUMMARY_SCHEMA: &str =
    "migration-stream/read/result-reconciliation-snapshot-summary/v1";
const ROW_LIST_SCHEMA: &str = "migration-stream/read/result-reconciliation-row-list/v1";
const ROW_DETAIL_SCHEMA: &str = "migration-stream/read/result-reconciliation-row-detail/v1";
const CANDIDATE_LIST_SCHEMA: &str = "migration-stream/read/result-candidate-list/v1";
const ELIGIBILITY_SCHEMA: &str = "migration-stream/read/result-approval-eligibility/v1";
const ACTION_HISTORY_SCHEMA: &str = "migration-stream/read/result-action-history/v1";

const SNAPSHOT_CURSOR: &str = "result-reconciliation-snapshots/v1";
const ROW_CURSOR: &str = "result-reconciliation-rows/v1";
const CANDIDATE_CURSOR: &str = "result-candidates/v1";
const ACTION_CURSOR: &str = "result-actions/v1";
const REQUIRED_SCHEMA_VERSION: i64 = crate::state::SCHEMA_VERSION;
const MAXIMUM_PAGE_SIZE: u32 = 200;
const MAXIMUM_CURSOR_BYTES: usize = 8 * 1024;
const MAXIMUM_IDENTIFIER_BYTES: usize = 256;
const MAXIMUM_SOURCE_FILE_BYTES: usize = 1_024;
const MAXIMUM_CODES: usize = 4_096;
const MAXIMUM_LINEAGE_REFERENCES: usize = 4_096;

const CATEGORIES: [&str; 8] = [
    "INCLUDED_VALID",
    "INCLUDED_CORRECTED",
    "INCLUDED_WAIVED",
    "INCLUDED_ACCEPTED_AS_IS",
    "EXCLUDED",
    "BLOCKED_UNRESOLVED",
    "BLOCKED_INCOMPLETE",
    "BLOCKED_CONFLICT",
];

/// One count in the exact closed eight-category vocabulary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationCategoryCount {
    pub category: String,
    pub row_count: u64,
}

/// Value-free immutable snapshot summary with derived currentness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationSnapshotSummary {
    pub snapshot_id: String,
    pub state: String,
    pub job_id: String,
    pub accepted_row_count: u64,
    pub governing_input_hash: String,
    pub snapshot_hash: Option<String>,
    pub current: bool,
    pub counts: Vec<ReconciliationCategoryCount>,
    pub warning_codes: Vec<String>,
    pub blocker_codes: Vec<String>,
    pub completed_at: Option<String>,
}

/// Stable page of immutable completed reconciliation snapshots.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationSnapshotList {
    pub schema_version: String,
    pub scope: JobScope,
    pub items: Vec<ResultReconciliationSnapshotSummary>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

/// Exact completed reconciliation snapshot summary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationSnapshot {
    pub schema_version: String,
    pub data: ResultReconciliationSnapshotSummary,
    pub canonical_hash: String,
}

/// Job-scoped response identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobScope {
    pub job_id: String,
}

/// Allow-listed filters for compact reconciliation rows.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRowFilters {
    pub category: Option<String>,
    pub source_file: Option<String>,
    pub source_ordinal: Option<u64>,
    pub entity_id: Option<String>,
    pub issue_id: Option<String>,
    pub decision_id: Option<String>,
}

/// Exact immutable snapshot scope for one row page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRowScope {
    pub job_id: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
}

/// Value-free compact reconciliation row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRowSummary {
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub entity_id: String,
    pub category: String,
    pub explanation_codes: Vec<String>,
    pub lineage_hashes: Vec<String>,
}

/// Bounded stable page of compact row classifications.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationRowList {
    pub schema_version: String,
    pub scope: ReconciliationRowScope,
    pub filters: ReconciliationRowFilters,
    pub items: Vec<ReconciliationRowSummary>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

/// One compact immutable reference in the exact row reason chain.
///
/// The reference identifies governing validation, decision, correction, or
/// re-evaluation evidence without exposing an original or effective value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationLineageReferenceView {
    pub reference_kind: String,
    pub reference_id: String,
    pub reference_hash: String,
}

/// Value-free exact classification evidence for one accepted source row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationRowDetailData {
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub row_identity_hash: String,
    pub entity_id: String,
    pub category: String,
    pub explanation_codes: Vec<String>,
    pub lineage_references: Vec<ReconciliationLineageReferenceView>,
    pub classification_hash: String,
}

/// Exact, job- and snapshot-scoped reconciliation row detail.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReconciliationRowDetail {
    pub schema_version: String,
    pub scope: ReconciliationRowScope,
    pub data: ResultReconciliationRowDetailData,
    pub canonical_hash: String,
}

/// Immutable candidate summary with derived currentness and eligibility.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultCandidateSummary {
    pub candidate_id: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub candidate_hash: String,
    pub current: bool,
    pub eligible: bool,
    pub included_count: u64,
    pub excluded_count: u64,
    pub blocked_count: u64,
    pub warning_codes: Vec<String>,
    pub required_acknowledgments: Vec<String>,
    pub creator: String,
    pub preparation_reason: String,
    pub created_at: String,
}

/// Stable page of immutable local-readiness candidates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultCandidateList {
    pub schema_version: String,
    pub scope: JobScope,
    pub items: Vec<ResultCandidateSummary>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

/// Derived, fail-closed candidate eligibility data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultApprovalEligibilityData {
    pub candidate_id: String,
    pub candidate_hash: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub eligible: bool,
    pub current: bool,
    pub reason_codes: Vec<String>,
    pub required_acknowledgments: Vec<String>,
    pub expected_active_action_id: Option<String>,
    pub expected_generation: u64,
}

/// Exact eligibility read for one immutable candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultApprovalEligibility {
    pub schema_version: String,
    pub data: ResultApprovalEligibilityData,
    pub canonical_hash: String,
}

/// Immutable, value-free result-action history item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultActionSummary {
    pub action_id: String,
    pub action_kind: String,
    pub action_hash: String,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub actor: String,
    pub reason: String,
    pub generation: u64,
    pub predecessor_action_id: Option<String>,
    pub occurred_at: String,
}

/// Job/candidate filter scope for immutable result actions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultActionScope {
    pub job_id: String,
    pub candidate_id: Option<String>,
}

/// Stable bounded page of the append-only result-action chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultActionHistory {
    pub schema_version: String,
    pub scope: ResultActionScope,
    pub items: Vec<ResultActionSummary>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

/// Read-only WP-006 service over one controlled workspace.
#[derive(Clone, Debug)]
pub struct ReconciliationReadService {
    workspace: PathBuf,
    query_timeout: Duration,
}

type ExactReconciliationRow = (
    String,
    i64,
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
);

#[allow(clippy::missing_errors_doc)]
impl ReconciliationReadService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            query_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_query_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_mins(1) {
            return invalid("query timeout must be in 1ns..=60s");
        }
        self.query_timeout = timeout;
        Ok(self)
    }

    pub fn snapshot_list(
        &self,
        job_id: &str,
        page: &PageRequest,
    ) -> Result<ResultReconciliationSnapshotList> {
        validate_identifier(job_id, "job ID")?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        verify_job(&connection, job_id)?;
        let scope = JobScope {
            job_id: job_id.to_owned(),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = empty_filter_hash()?;
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| snapshot_cursor(cursor, &scope_hash, &filter_hash))
            .transpose()?;
        let mut sql = String::from(
            "SELECT snapshot_id, state, job_id, accepted_row_count,
                    governing_input_hash, snapshot_hash, warning_codes_json,
                    blocker_codes_json, completed_at, validation_run_id
             FROM reconciliation_snapshots WHERE job_id = ?",
        );
        let mut values = vec![SqlValue::Text(job_id.to_owned())];
        if let Some((completed_at, snapshot_id)) = &after {
            sql.push_str(" AND (completed_at < ? OR (completed_at = ? AND snapshot_id < ?))");
            values.extend([
                SqlValue::Text(completed_at.clone()),
                SqlValue::Text(completed_at.clone()),
                SqlValue::Text(snapshot_id.clone()),
            ]);
        }
        sql.push_str(" ORDER BY completed_at DESC, snapshot_id DESC LIMIT ?");
        values.push(SqlValue::Integer(i64::from(page_size + 1)));
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), snapshot_header_row)?;
        let mut headers = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = headers.len() > usize::try_from(page_size).unwrap_or(usize::MAX);
        if has_more {
            headers.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
        }
        let next_cursor = if has_more {
            let last = headers.last().ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("snapshot page lost its final row".to_owned())
            })?;
            Some(encode_cursor(
                SNAPSHOT_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([last.completed_at, last.snapshot_id]),
            )?)
        } else {
            None
        };
        let mut current_hashes = BTreeMap::new();
        let mut items = Vec::with_capacity(headers.len());
        for header in headers {
            let current_hash = current_hash_for_run(
                &connection,
                job_id,
                &header.validation_run_id,
                &mut current_hashes,
            )?;
            items.push(snapshot_summary_from_header(
                &connection,
                header,
                current_hash,
            )?);
        }
        let mut response = ResultReconciliationSnapshotList {
            schema_version: SNAPSHOT_LIST_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn snapshot_summary(
        &self,
        job_id: &str,
        snapshot_id: &str,
    ) -> Result<ResultReconciliationSnapshot> {
        validate_identifier(job_id, "job ID")?;
        validate_identifier(snapshot_id, "snapshot ID")?;
        let connection = self.open_snapshot()?;
        let header = connection
            .query_row(
                "SELECT snapshot_id, state, job_id, accepted_row_count,
                        governing_input_hash, snapshot_hash, warning_codes_json,
                        blocker_codes_json, completed_at, validation_run_id
                 FROM reconciliation_snapshots
                 WHERE snapshot_id = ?1 AND job_id = ?2",
                params![snapshot_id, job_id],
                snapshot_header_row,
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::NotFound(format!("reconciliation snapshot {snapshot_id}"))
            })?;
        let current_hash = current_governing_hash(&connection, job_id, &header.validation_run_id)?;
        let mut response = ResultReconciliationSnapshot {
            schema_version: SNAPSHOT_SUMMARY_SCHEMA.to_owned(),
            data: snapshot_summary_from_header(&connection, header, &current_hash)?,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn reconciliation_rows(
        &self,
        job_id: &str,
        snapshot_id: &str,
        filters: &ReconciliationRowFilters,
        page: &PageRequest,
    ) -> Result<ResultReconciliationRowList> {
        validate_identifier(job_id, "job ID")?;
        validate_identifier(snapshot_id, "snapshot ID")?;
        validate_row_filters(filters)?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let snapshot_hash: String = connection
            .query_row(
                "SELECT snapshot_hash FROM reconciliation_snapshots
                 WHERE snapshot_id = ?1 AND job_id = ?2",
                params![snapshot_id, job_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::NotFound(format!("reconciliation snapshot {snapshot_id}"))
            })?;
        let scope = ReconciliationRowScope {
            job_id: job_id.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            snapshot_hash,
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = canonical_hash(&serde_json::to_value(filters)?)?;
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| row_cursor(cursor, &scope_hash, &filter_hash))
            .transpose()?;
        let (sql, values) = row_sql(snapshot_id, filters, after.as_ref(), page_size + 1)?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), compact_row)?;
        let mut items = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = items.len() > usize::try_from(page_size).unwrap_or(usize::MAX);
        if has_more {
            items.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
        }
        for item in &mut items {
            item.lineage_hashes = load_lineage_hashes(
                &connection,
                snapshot_id,
                &item.source_file,
                item.source_ordinal,
            )?;
        }
        let next_cursor = if has_more {
            let last = items.last().ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("row page lost its final row".to_owned())
            })?;
            Some(encode_cursor(
                ROW_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([last.source_file, last.source_ordinal]),
            )?)
        } else {
            None
        };
        let mut response = ResultReconciliationRowList {
            schema_version: ROW_LIST_SCHEMA.to_owned(),
            scope,
            filters: filters.clone(),
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    #[allow(clippy::too_many_lines)]
    pub fn reconciliation_row_detail(
        &self,
        job_id: &str,
        snapshot_id: &str,
        source_file: &str,
        source_ordinal: u64,
    ) -> Result<ResultReconciliationRowDetail> {
        validate_identifier(job_id, "job ID")?;
        validate_identifier(snapshot_id, "snapshot ID")?;
        validate_safe_source_file(source_file)?;
        if source_ordinal == 0 {
            return invalid("source ordinal must be positive");
        }
        let connection = self.open_snapshot()?;
        let row: ExactReconciliationRow = connection
            .query_row(
                "SELECT r.source_file, r.source_ordinal, r.line_start, r.line_end,
                        r.row_identity_hash, r.entity_id, r.category,
                        r.explanation_codes_json, r.classification_hash
                 FROM reconciliation_row_classifications r
                 JOIN reconciliation_snapshots s
                   ON s.snapshot_id = r.snapshot_id AND s.job_id = r.job_id
                 WHERE r.snapshot_id = ?1 AND r.job_id = ?2
                   AND r.source_file = ?3 AND r.source_ordinal = ?4",
                params![
                    snapshot_id,
                    job_id,
                    source_file,
                    to_i64(source_ordinal, "source ordinal")?
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::NotFound(format!(
                    "reconciliation row {source_file}:{source_ordinal} in snapshot {snapshot_id}"
                ))
            })?;
        let snapshot_hash: String = connection.query_row(
            "SELECT snapshot_hash FROM reconciliation_snapshots
             WHERE snapshot_id = ?1 AND job_id = ?2",
            params![snapshot_id, job_id],
            |record| record.get(0),
        )?;
        validate_hash(&snapshot_hash, "snapshot hash")?;
        validate_hash(&row.4, "row identity hash")?;
        validate_hash(&row.8, "row classification hash")?;
        if row.0 != source_file || !CATEGORIES.contains(&row.6.as_str()) {
            return integrity("exact reconciliation row identity or category is invalid");
        }
        let persisted_ordinal = positive_sqlite_u64(row.1, "row source ordinal")?;
        let line_start = positive_sqlite_u64(row.2, "row line start")?;
        let line_end = positive_sqlite_u64(row.3, "row line end")?;
        if persisted_ordinal != source_ordinal || line_end < line_start {
            return integrity("exact reconciliation row positions are invalid");
        }
        let explanation_codes = parse_codes(&row.7, "row explanation codes")?;
        let lineage_references =
            load_lineage_references(&connection, snapshot_id, source_file, source_ordinal)?;
        let mut response = ResultReconciliationRowDetail {
            schema_version: ROW_DETAIL_SCHEMA.to_owned(),
            scope: ReconciliationRowScope {
                job_id: job_id.to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                snapshot_hash,
            },
            data: ResultReconciliationRowDetailData {
                source_file: row.0,
                source_ordinal: persisted_ordinal,
                line_start,
                line_end,
                row_identity_hash: row.4,
                entity_id: row.5,
                category: row.6,
                explanation_codes,
                lineage_references,
                classification_hash: row.8,
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn candidate_list(&self, job_id: &str, page: &PageRequest) -> Result<ResultCandidateList> {
        validate_identifier(job_id, "job ID")?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        verify_job(&connection, job_id)?;
        let scope = JobScope {
            job_id: job_id.to_owned(),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = empty_filter_hash()?;
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| scalar_u64_cursor(cursor, CANDIDATE_CURSOR, &scope_hash, &filter_hash))
            .transpose()?;
        let mut sql = String::from(
            "SELECT candidate_id, snapshot_id, snapshot_hash, candidate_hash,
                    included_count, excluded_count, blocked_count,
                    warning_codes_json, required_acknowledgments_json,
                    creator, preparation_reason, created_at, creation_generation,
                    governing_input_hash, canonical_candidate_json
             FROM migration_result_candidates WHERE job_id = ?",
        );
        let mut values = vec![SqlValue::Text(job_id.to_owned())];
        if let Some(generation) = after {
            sql.push_str(" AND creation_generation < ?");
            values.push(SqlValue::Integer(to_i64(
                generation,
                "candidate cursor generation",
            )?));
        }
        sql.push_str(" ORDER BY creation_generation DESC LIMIT ?");
        values.push(SqlValue::Integer(i64::from(page_size + 1)));
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), candidate_row)?;
        let mut candidates = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = candidates.len() > usize::try_from(page_size).unwrap_or(usize::MAX);
        if has_more {
            candidates.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
        }
        let mut current_hashes = BTreeMap::new();
        let mut items = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            let current_hash = current_hash_for_run(
                &connection,
                job_id,
                &candidate.domain.governing.source_validation_run.identity,
                &mut current_hashes,
            )?;
            items.push(candidate.summary(current_hash == candidate.governing_input_hash));
        }
        let next_cursor = if has_more {
            let last = candidates.last().ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("candidate page lost its final row".to_owned())
            })?;
            Some(encode_cursor(
                CANDIDATE_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([last.creation_generation]),
            )?)
        } else {
            None
        };
        let mut response = ResultCandidateList {
            schema_version: CANDIDATE_LIST_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn approval_eligibility(
        &self,
        job_id: &str,
        candidate_id: &str,
    ) -> Result<ResultApprovalEligibility> {
        validate_identifier(job_id, "job ID")?;
        validate_identifier(candidate_id, "candidate ID")?;
        let connection = self.open_snapshot()?;
        let candidate = connection
            .query_row(
                "SELECT candidate_id, snapshot_id, snapshot_hash, candidate_hash,
                        included_count, excluded_count, blocked_count,
                        warning_codes_json, required_acknowledgments_json,
                        creator, preparation_reason, created_at, creation_generation,
                        governing_input_hash, canonical_candidate_json
                 FROM migration_result_candidates
                 WHERE candidate_id = ?1 AND job_id = ?2",
                params![candidate_id, job_id],
                candidate_row,
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::NotFound(format!("migration-result candidate {candidate_id}"))
            })?;
        let current_hash = current_governing_hash(
            &connection,
            job_id,
            &candidate.domain.governing.source_validation_run.identity,
        )?;
        let current = current_hash == candidate.governing_input_hash;
        let reason_codes = eligibility_reason_codes(&connection, &candidate.snapshot_id, current)?;
        let (expected_active_action_id, expected_generation) = action_head(&connection, job_id)?;
        let mut response = ResultApprovalEligibility {
            schema_version: ELIGIBILITY_SCHEMA.to_owned(),
            data: ResultApprovalEligibilityData {
                candidate_id: candidate.candidate_id,
                candidate_hash: candidate.candidate_hash,
                snapshot_id: candidate.snapshot_id,
                snapshot_hash: candidate.snapshot_hash,
                eligible: reason_codes.is_empty(),
                current,
                reason_codes,
                required_acknowledgments: candidate.required_acknowledgments,
                expected_active_action_id,
                expected_generation,
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn action_history(
        &self,
        job_id: &str,
        candidate_id: Option<&str>,
        page: &PageRequest,
    ) -> Result<ResultActionHistory> {
        validate_identifier(job_id, "job ID")?;
        if let Some(candidate_id) = candidate_id {
            validate_identifier(candidate_id, "candidate ID")?;
        }
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        verify_job(&connection, job_id)?;
        let scope = ResultActionScope {
            job_id: job_id.to_owned(),
            candidate_id: candidate_id.map(str::to_owned),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = canonical_hash(&json!({ "candidateId": candidate_id }))?;
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| scalar_u64_cursor(cursor, ACTION_CURSOR, &scope_hash, &filter_hash))
            .transpose()?;
        let mut sql = String::from(
            "SELECT action_id, action_kind, action_hash, candidate_id,
                    candidate_hash, actor, reason, generation,
                    predecessor_action_id, occurred_at, canonical_action_json
             FROM migration_result_actions WHERE job_id = ?",
        );
        let mut values = vec![SqlValue::Text(job_id.to_owned())];
        if let Some(candidate_id) = candidate_id {
            sql.push_str(" AND candidate_id = ?");
            values.push(SqlValue::Text(candidate_id.to_owned()));
        }
        if let Some(generation) = after {
            sql.push_str(" AND generation < ?");
            values.push(SqlValue::Integer(to_i64(
                generation,
                "action cursor generation",
            )?));
        }
        sql.push_str(" ORDER BY generation DESC LIMIT ?");
        values.push(SqlValue::Integer(i64::from(page_size + 1)));
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), action_row)?;
        let mut keyed = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = keyed.len() > usize::try_from(page_size).unwrap_or(usize::MAX);
        if has_more {
            keyed.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
        }
        let next_cursor = if has_more {
            let last = keyed.last().ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("action page lost its final row".to_owned())
            })?;
            Some(encode_cursor(
                ACTION_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([last.0]),
            )?)
        } else {
            None
        };
        let items = keyed.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let mut response = ResultActionHistory {
            schema_version: ACTION_HISTORY_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    fn open_snapshot(&self) -> Result<Connection> {
        let mut connection = crate::state::open_read_only(&self.workspace.join("state.sqlite3"))?;
        let started = Instant::now();
        let timeout = self.query_timeout;
        connection.progress_handler(100, Some(move || started.elapsed() >= timeout))?;
        connection.execute_batch("BEGIN DEFERRED;")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != REQUIRED_SCHEMA_VERSION {
            return integrity(format!(
                "WP-006 reads require schema version {REQUIRED_SCHEMA_VERSION}; found {version}"
            ));
        }
        crate::decision_integrity::verify_decision_integrity_connection(&connection)
            .map_err(|error| KernelError::ResultApprovalIntegrity(error.to_string()))?;
        crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &connection,
        )?;
        Ok(connection)
    }
}

#[derive(Debug)]
struct SnapshotHeader {
    snapshot_id: String,
    state: String,
    job_id: String,
    accepted_row_count: u64,
    governing_input_hash: String,
    snapshot_hash: String,
    warning_codes: Vec<String>,
    blocker_codes: Vec<String>,
    completed_at: String,
    validation_run_id: String,
}

fn snapshot_header_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotHeader> {
    let accepted_row_count = sqlite_u64(row.get(3)?, "snapshot accepted row count")?;
    let warning_json: String = row.get(6)?;
    let blocker_json: String = row.get(7)?;
    Ok(SnapshotHeader {
        snapshot_id: row.get(0)?,
        state: row.get(1)?,
        job_id: row.get(2)?,
        accepted_row_count,
        governing_input_hash: row.get(4)?,
        snapshot_hash: row.get(5)?,
        warning_codes: parse_codes_sql(&warning_json, "snapshot warning codes")?,
        blocker_codes: parse_codes_sql(&blocker_json, "snapshot blocker codes")?,
        completed_at: row.get(8)?,
        validation_run_id: row.get(9)?,
    })
}

fn snapshot_summary_from_header(
    connection: &Connection,
    header: SnapshotHeader,
    current_hash: &str,
) -> Result<ResultReconciliationSnapshotSummary> {
    validate_hash(
        &header.governing_input_hash,
        "snapshot governing-input hash",
    )?;
    validate_hash(&header.snapshot_hash, "snapshot hash")?;
    if header.state != "COMPLETE" {
        return integrity("persisted reconciliation snapshot is not COMPLETE");
    }
    let counts = load_category_counts(connection, &header.snapshot_id)?;
    let total = counts.iter().try_fold(0_u64, |sum, item| {
        sum.checked_add(item.row_count).ok_or_else(|| {
            KernelError::ResultApprovalIntegrity("category count overflow".to_owned())
        })
    })?;
    if total != header.accepted_row_count {
        return integrity("snapshot category counts do not close to accepted rows");
    }
    Ok(ResultReconciliationSnapshotSummary {
        snapshot_id: header.snapshot_id,
        state: header.state,
        job_id: header.job_id,
        accepted_row_count: header.accepted_row_count,
        current: header.governing_input_hash == current_hash,
        governing_input_hash: header.governing_input_hash,
        snapshot_hash: Some(header.snapshot_hash),
        counts,
        warning_codes: header.warning_codes,
        blocker_codes: header.blocker_codes,
        completed_at: Some(header.completed_at),
    })
}

fn load_category_counts(
    connection: &Connection,
    snapshot_id: &str,
) -> Result<Vec<ReconciliationCategoryCount>> {
    let mut statement = connection.prepare(
        "SELECT category, row_count FROM reconciliation_category_counts
         WHERE snapshot_id = ?1 ORDER BY category",
    )?;
    let rows = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            sqlite_u64(row.get(1)?, "reconciliation category count")?,
        ))
    })?;
    let persisted = rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    if persisted.len() != CATEGORIES.len()
        || persisted
            .keys()
            .any(|category| !CATEGORIES.contains(&category.as_str()))
    {
        return integrity("snapshot does not persist every exact reconciliation category once");
    }
    CATEGORIES
        .iter()
        .map(|category| {
            persisted
                .get(*category)
                .copied()
                .map(|row_count| ReconciliationCategoryCount {
                    category: (*category).to_owned(),
                    row_count,
                })
                .ok_or_else(|| {
                    KernelError::ResultApprovalIntegrity(format!(
                        "snapshot lacks category {category}"
                    ))
                })
        })
        .collect()
}

fn compact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReconciliationRowSummary> {
    let explanation_json: String = row.get(6)?;
    Ok(ReconciliationRowSummary {
        source_file: row.get(0)?,
        source_ordinal: sqlite_u64(row.get(1)?, "row source ordinal")?,
        line_start: sqlite_u64(row.get(2)?, "row line start")?,
        line_end: sqlite_u64(row.get(3)?, "row line end")?,
        entity_id: row.get(4)?,
        category: row.get(5)?,
        explanation_codes: parse_codes_sql(&explanation_json, "row explanation codes")?,
        lineage_hashes: Vec::new(),
    })
}

fn load_lineage_hashes(
    connection: &Connection,
    snapshot_id: &str,
    source_file: &str,
    source_ordinal: u64,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT reference_hash FROM reconciliation_row_lineage_references
         WHERE snapshot_id = ?1 AND source_file = ?2 AND source_ordinal = ?3
         ORDER BY reference_hash",
    )?;
    let rows = statement.query_map(
        params![
            snapshot_id,
            source_file,
            to_i64(source_ordinal, "source ordinal")?
        ],
        |row| row.get::<_, String>(0),
    )?;
    let hashes = rows.collect::<std::result::Result<BTreeSet<_>, _>>()?;
    for hash in &hashes {
        validate_hash(hash, "row lineage hash")?;
    }
    Ok(hashes.into_iter().collect())
}

fn load_lineage_references(
    connection: &Connection,
    snapshot_id: &str,
    source_file: &str,
    source_ordinal: u64,
) -> Result<Vec<ReconciliationLineageReferenceView>> {
    let mut statement = connection.prepare(
        "SELECT reference_kind, reference_id, reference_hash
         FROM reconciliation_row_lineage_references
         WHERE snapshot_id = ?1 AND source_file = ?2 AND source_ordinal = ?3
         ORDER BY reference_kind, reference_id, reference_hash
         LIMIT ?4",
    )?;
    let limit = i64::try_from(MAXIMUM_LINEAGE_REFERENCES + 1).map_err(|_| {
        KernelError::ResultApprovalBound(
            "lineage-reference bound exceeds SQLite INTEGER".to_owned(),
        )
    })?;
    let rows = statement.query_map(
        params![
            snapshot_id,
            source_file,
            to_i64(source_ordinal, "source ordinal")?,
            limit
        ],
        |row| {
            Ok(ReconciliationLineageReferenceView {
                reference_kind: row.get(0)?,
                reference_id: row.get(1)?,
                reference_hash: row.get(2)?,
            })
        },
    )?;
    let references = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if references.len() > MAXIMUM_LINEAGE_REFERENCES {
        return Err(KernelError::ResultApprovalBound(format!(
            "exact row lineage exceeds {MAXIMUM_LINEAGE_REFERENCES} references"
        )));
    }
    for reference in &references {
        if !matches!(
            reference.reference_kind.as_str(),
            "VALIDATION_RUN"
                | "VALIDATION_ISSUE"
                | "ISSUE_DECISION"
                | "CORRECTION_OVERLAY"
                | "CORRECTION_EFFECTIVATION"
                | "REEVALUATION_RUN"
        ) {
            return integrity("exact row lineage contains an unsupported reference kind");
        }
        validate_identifier(&reference.reference_id, "lineage reference ID")?;
        validate_hash(&reference.reference_hash, "lineage reference hash")?;
    }
    if references.windows(2).any(|pair| {
        (
            &pair[0].reference_kind,
            &pair[0].reference_id,
            &pair[0].reference_hash,
        ) >= (
            &pair[1].reference_kind,
            &pair[1].reference_id,
            &pair[1].reference_hash,
        )
    }) {
        return integrity("exact row lineage is not strictly sorted and unique");
    }
    Ok(references)
}

fn row_sql(
    snapshot_id: &str,
    filters: &ReconciliationRowFilters,
    after: Option<&(String, u64)>,
    limit: u32,
) -> Result<(String, Vec<SqlValue>)> {
    let mut sql = String::from(
        "SELECT r.source_file, r.source_ordinal, r.line_start, r.line_end,
                r.entity_id, r.category, r.explanation_codes_json
         FROM reconciliation_row_classifications r WHERE r.snapshot_id = ?",
    );
    let mut values = vec![SqlValue::Text(snapshot_id.to_owned())];
    if let Some(category) = &filters.category {
        sql.push_str(" AND r.category = ?");
        values.push(SqlValue::Text(category.clone()));
    }
    if let Some(source_file) = &filters.source_file {
        sql.push_str(" AND r.source_file = ?");
        values.push(SqlValue::Text(source_file.clone()));
    }
    if let Some(source_ordinal) = filters.source_ordinal {
        sql.push_str(" AND r.source_ordinal = ?");
        values.push(SqlValue::Integer(to_i64(
            source_ordinal,
            "source-ordinal filter",
        )?));
    }
    if let Some(entity_id) = &filters.entity_id {
        sql.push_str(" AND r.entity_id = ?");
        values.push(SqlValue::Text(entity_id.clone()));
    }
    for (kind, identity) in [
        ("VALIDATION_ISSUE", filters.issue_id.as_ref()),
        ("ISSUE_DECISION", filters.decision_id.as_ref()),
    ] {
        if let Some(identity) = identity {
            sql.push_str(
                " AND EXISTS (
                     SELECT 1 FROM reconciliation_row_lineage_references l
                     WHERE l.snapshot_id = r.snapshot_id
                       AND l.source_file = r.source_file
                       AND l.source_ordinal = r.source_ordinal
                       AND l.reference_kind = ? AND l.reference_id = ?)",
            );
            values.extend([
                SqlValue::Text(kind.to_owned()),
                SqlValue::Text(identity.clone()),
            ]);
        }
    }
    if let Some((source_file, source_ordinal)) = after {
        sql.push_str(
            " AND (r.source_file > ?
                    OR (r.source_file = ? AND r.source_ordinal > ?))",
        );
        values.extend([
            SqlValue::Text(source_file.clone()),
            SqlValue::Text(source_file.clone()),
            SqlValue::Integer(to_i64(*source_ordinal, "row cursor source ordinal")?),
        ]);
    }
    sql.push_str(" ORDER BY r.source_file, r.source_ordinal LIMIT ?");
    values.push(SqlValue::Integer(i64::from(limit)));
    Ok((sql, values))
}

#[derive(Debug)]
struct CandidateRow {
    candidate_id: String,
    snapshot_id: String,
    snapshot_hash: String,
    candidate_hash: String,
    included_count: u64,
    excluded_count: u64,
    blocked_count: u64,
    warning_codes: Vec<String>,
    required_acknowledgments: Vec<String>,
    creator: String,
    preparation_reason: String,
    created_at: String,
    creation_generation: u64,
    governing_input_hash: String,
    domain: MigrationResultCandidate,
}

impl CandidateRow {
    fn summary(&self, current: bool) -> ResultCandidateSummary {
        ResultCandidateSummary {
            candidate_id: self.candidate_id.clone(),
            snapshot_id: self.snapshot_id.clone(),
            snapshot_hash: self.snapshot_hash.clone(),
            candidate_hash: self.candidate_hash.clone(),
            current,
            eligible: current && self.blocked_count == 0,
            included_count: self.included_count,
            excluded_count: self.excluded_count,
            blocked_count: self.blocked_count,
            warning_codes: self.warning_codes.clone(),
            required_acknowledgments: self.required_acknowledgments.clone(),
            creator: self.creator.clone(),
            preparation_reason: self.preparation_reason.clone(),
            created_at: self.created_at.clone(),
        }
    }
}

fn candidate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CandidateRow> {
    let warning_json: String = row.get(7)?;
    let acknowledgments_json: String = row.get(8)?;
    let canonical_json: String = row.get(14)?;
    let domain = parse_candidate_sql(&canonical_json)?;
    let result = CandidateRow {
        candidate_id: row.get(0)?,
        snapshot_id: row.get(1)?,
        snapshot_hash: row.get(2)?,
        candidate_hash: row.get(3)?,
        included_count: sqlite_u64(row.get(4)?, "candidate included count")?,
        excluded_count: sqlite_u64(row.get(5)?, "candidate excluded count")?,
        blocked_count: sqlite_u64(row.get(6)?, "candidate blocked count")?,
        warning_codes: parse_codes_sql(&warning_json, "candidate warning codes")?,
        required_acknowledgments: parse_codes_sql(
            &acknowledgments_json,
            "candidate required acknowledgments",
        )?,
        creator: row.get(9)?,
        preparation_reason: row.get(10)?,
        created_at: row.get(11)?,
        creation_generation: sqlite_u64(row.get(12)?, "candidate creation generation")?,
        governing_input_hash: row.get(13)?,
        domain,
    };
    candidate_columns_match(&result)?;
    Ok(result)
}

fn parse_candidate_sql(value: &str) -> rusqlite::Result<MigrationResultCandidate> {
    let parsed = parse_unique_json(value.as_bytes()).map_err(sql_integrity_error)?;
    let candidate: MigrationResultCandidate =
        serde_json::from_value(parsed).map_err(sql_json_error)?;
    candidate.validate().map_err(sql_integrity_error)?;
    Ok(candidate)
}

fn candidate_columns_match(candidate: &CandidateRow) -> rusqlite::Result<()> {
    let domain = &candidate.domain;
    if candidate.candidate_id != domain.candidate_id
        || candidate.snapshot_id != domain.snapshot_id
        || candidate.snapshot_hash != domain.snapshot_hash
        || candidate.candidate_hash != domain.candidate_hash
        || candidate.included_count != domain.counts.included
        || candidate.excluded_count != domain.counts.excluded
        || candidate.blocked_count != domain.counts.blocked
        || candidate.warning_codes != domain.warning_codes
        || candidate.required_acknowledgments != domain.required_acknowledgement_codes
        || candidate.creator != domain.practitioner_id
        || candidate.preparation_reason != domain.preparation_reason
        || candidate.created_at != domain.created_at
        || candidate.creation_generation != domain.creation_generation
        || candidate.governing_input_hash != domain.governing_input_hash
    {
        return Err(sql_integrity_message(
            "candidate columns differ from canonical candidate evidence",
        ));
    }
    Ok(())
}

fn action_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(u64, ResultActionSummary)> {
    let canonical_json: String = row.get(10)?;
    let value = parse_unique_json(canonical_json.as_bytes()).map_err(sql_integrity_error)?;
    let domain: ResultAction = serde_json::from_value(value).map_err(sql_json_error)?;
    domain.validate().map_err(sql_integrity_error)?;
    let generation = sqlite_u64(row.get(7)?, "result-action generation")?;
    let item = ResultActionSummary {
        action_id: row.get(0)?,
        action_kind: row.get(1)?,
        action_hash: row.get(2)?,
        candidate_id: row.get(3)?,
        candidate_hash: row.get(4)?,
        actor: row.get(5)?,
        reason: row.get(6)?,
        generation,
        predecessor_action_id: row.get(8)?,
        occurred_at: row.get(9)?,
    };
    if item.action_id != domain.action_id
        || item.action_kind != domain.payload.kind.as_str()
        || item.action_hash != domain.action_hash
        || item.candidate_id != domain.payload.candidate_id
        || item.candidate_hash != domain.payload.candidate_hash
        || item.actor != domain.payload.actor
        || item.reason != domain.payload.reason
        || item.generation != domain.generation
        || item.predecessor_action_id != domain.payload.predecessor_action_id
        || item.occurred_at != domain.action_timestamp
    {
        return Err(sql_integrity_message(
            "result-action columns differ from canonical action evidence",
        ));
    }
    Ok((generation, item))
}

fn eligibility_reason_codes(
    connection: &Connection,
    snapshot_id: &str,
    current: bool,
) -> Result<Vec<String>> {
    let counts = load_category_counts(connection, snapshot_id)?;
    let mut reasons = BTreeSet::new();
    if !current {
        reasons.insert("STALE_GOVERNING_INPUTS".to_owned());
    }
    for count in counts {
        if count.row_count > 0 && count.category.starts_with("BLOCKED_") {
            reasons.insert(count.category);
        }
    }
    Ok(reasons.into_iter().collect())
}

fn action_head(connection: &Connection, job_id: &str) -> Result<(Option<String>, u64)> {
    let head: Option<(String, i64)> = connection
        .query_row(
            "SELECT action_id, generation
             FROM migration_result_actions WHERE job_id = ?1
             ORDER BY generation DESC LIMIT 1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((action_id, generation)) = head else {
        return Ok((None, 0));
    };
    let generation = positive_or_zero(generation, "active result-action generation")?;
    Ok((Some(action_id), generation))
}

fn current_hash_for_run<'a>(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    cache: &'a mut BTreeMap<String, String>,
) -> Result<&'a str> {
    if let std::collections::btree_map::Entry::Vacant(entry) =
        cache.entry(validation_run_id.to_owned())
    {
        entry.insert(current_governing_hash(
            connection,
            job_id,
            validation_run_id,
        )?);
    }
    cache
        .get(validation_run_id)
        .map(String::as_str)
        .ok_or_else(|| KernelError::ResultApprovalIntegrity("currentness cache failed".to_owned()))
}

fn current_governing_hash(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
) -> Result<String> {
    crate::reconciliation_service::current_governing_input_hash_for_read(
        connection,
        job_id,
        validation_run_id,
    )
}

fn validate_row_filters(filters: &ReconciliationRowFilters) -> Result<()> {
    if filters
        .category
        .as_deref()
        .is_some_and(|category| !CATEGORIES.contains(&category))
    {
        return invalid("row category filter is not in the closed WP-006 vocabulary");
    }
    if let Some(source_file) = &filters.source_file {
        validate_text_filter(source_file, "source-file filter", MAXIMUM_SOURCE_FILE_BYTES)?;
    }
    if filters.source_ordinal == Some(0) {
        return invalid("source-ordinal filter must be positive");
    }
    for (value, label) in [
        (&filters.entity_id, "entity filter"),
        (&filters.issue_id, "issue filter"),
        (&filters.decision_id, "decision filter"),
    ] {
        if let Some(value) = value {
            validate_text_filter(value, label, MAXIMUM_IDENTIFIER_BYTES)?;
        }
    }
    Ok(())
}

fn validate_safe_source_file(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_SOURCE_FILE_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return invalid("source file must be a bounded safe relative path");
    }
    for segment in value.split('/') {
        let reserved_extension = Path::new(segment)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("part")
                    || extension.eq_ignore_ascii_case("tombstone")
            });
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.trim() != segment
            || reserved_extension
        {
            return invalid(
                "source file cannot use traversal, live-part, or tombstone namespace names",
            );
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTIFIER_BYTES
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return invalid(format!("{label} must be a bounded safe identifier"));
    }
    Ok(())
}

fn validate_text_filter(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return invalid(format!("{label} exceeds its bound"));
    }
    Ok(())
}

fn validate_page(page: &PageRequest) -> Result<u32> {
    if page.page_size == 0 || page.page_size > MAXIMUM_PAGE_SIZE {
        return invalid("page size must be in 1..=200");
    }
    if page
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > MAXIMUM_CURSOR_BYTES)
    {
        return invalid("cursor exceeds 8192 bytes");
    }
    Ok(page.page_size)
}

fn verify_job(connection: &Connection, job_id: &str) -> Result<()> {
    let found: bool = connection
        .query_row("SELECT 1 FROM jobs WHERE id = ?1", [job_id], |_| Ok(true))
        .optional()?
        .unwrap_or(false);
    if !found {
        return Err(KernelError::NotFound(format!("job {job_id}")));
    }
    Ok(())
}

fn parse_codes_sql(value: &str, label: &str) -> rusqlite::Result<Vec<String>> {
    parse_codes(value, label).map_err(sql_integrity_error)
}

fn parse_codes(value: &str, label: &str) -> Result<Vec<String>> {
    let parsed = parse_unique_json(value.as_bytes())?;
    let codes: Vec<String> = serde_json::from_value(parsed)?;
    if codes.len() > MAXIMUM_CODES
        || codes.windows(2).any(|pair| pair[0] >= pair[1])
        || codes.iter().any(|code| {
            code.is_empty()
                || code.len() > MAXIMUM_IDENTIFIER_BYTES
                || code.chars().any(char::is_control)
        })
    {
        return integrity(format!("{label} are not bounded, sorted, and unique"));
    }
    Ok(codes)
}

fn validate_hash(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return integrity(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

fn snapshot_cursor(cursor: &str, scope_hash: &str, filter_hash: &str) -> Result<(String, String)> {
    let payload = decode_cursor(cursor, SNAPSHOT_CURSOR, scope_hash, filter_hash)?;
    let values = payload
        .last_key
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid snapshot cursor".to_owned()))?;
    let completed_at = values[0]
        .as_str()
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid snapshot cursor".to_owned()))?;
    let snapshot_id = values[1]
        .as_str()
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid snapshot cursor".to_owned()))?;
    validate_text_filter(completed_at, "snapshot cursor timestamp", 64)?;
    validate_identifier(snapshot_id, "snapshot cursor ID")?;
    Ok((completed_at.to_owned(), snapshot_id.to_owned()))
}

fn row_cursor(cursor: &str, scope_hash: &str, filter_hash: &str) -> Result<(String, u64)> {
    let payload = decode_cursor(cursor, ROW_CURSOR, scope_hash, filter_hash)?;
    let values = payload
        .last_key
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid row cursor".to_owned()))?;
    let source_file = values[0]
        .as_str()
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid row cursor".to_owned()))?;
    let ordinal = values[1]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| KernelError::ResultApprovalInvalid("invalid row cursor".to_owned()))?;
    validate_text_filter(
        source_file,
        "row cursor source file",
        MAXIMUM_SOURCE_FILE_BYTES,
    )?;
    Ok((source_file.to_owned(), ordinal))
}

fn scalar_u64_cursor(
    cursor: &str,
    contract: &str,
    scope_hash: &str,
    filter_hash: &str,
) -> Result<u64> {
    let payload = decode_cursor(cursor, contract, scope_hash, filter_hash)?;
    payload
        .last_key
        .as_array()
        .filter(|values| values.len() == 1)
        .and_then(|values| values[0].as_u64())
        .filter(|value| *value > 0)
        .ok_or_else(|| KernelError::ResultApprovalInvalid(format!("invalid {contract} cursor")))
}

fn empty_filter_hash() -> Result<String> {
    canonical_hash(&json!({}))
}

fn page_info(page_size: u32, returned: usize, next_cursor: Option<String>) -> Result<PageInfo> {
    Ok(PageInfo {
        page_size,
        returned: u32::try_from(returned)
            .map_err(|_| KernelError::ResultApprovalBound("page count overflow".to_owned()))?,
        next_cursor,
    })
}

fn response_hash<T: Serialize>(response: &T) -> Result<String> {
    let mut value = serde_json::to_value(response)?;
    let object = value.as_object_mut().ok_or_else(|| {
        KernelError::ResultApprovalIntegrity("read response must serialize as an object".to_owned())
    })?;
    object.insert("canonical_hash".to_owned(), Value::String(String::new()));
    canonical_hash(&value)
}

fn positive_or_zero(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| KernelError::ResultApprovalIntegrity(format!("{label} is negative")))
}

fn positive_sqlite_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| KernelError::ResultApprovalIntegrity(format!("{label} is not positive")))
}

fn to_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::ResultApprovalBound(format!("{label} exceeds SQLite INTEGER")))
}

fn sqlite_u64(value: i64, label: &str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| sql_integrity_message(&format!("{label} is negative")))
}

fn sql_integrity_error(error: impl std::fmt::Display) -> rusqlite::Error {
    sql_integrity_message(&error.to_string())
}

fn sql_json_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn sql_integrity_message(message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.to_owned(),
        )),
    )
}

fn invalid<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ResultApprovalInvalid(detail.into()))
}

fn integrity<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ResultApprovalIntegrity(detail.into()))
}

#[cfg(test)]
mod tests {
    use super::{
        ReconciliationLineageReferenceView, ReconciliationRowFilters, validate_safe_source_file,
    };

    #[test]
    fn exact_row_source_file_filter_excludes_live_and_inert_namespaces() {
        validate_safe_source_file("synthetic/rows.csv").expect("safe accepted path");
        for unsafe_path in [
            "/absolute.csv",
            "../escape.csv",
            "synthetic\\rows.csv",
            "synthetic/live.part",
            "synthetic/inert.TOMBSTONE",
            "synthetic//rows.csv",
            "synthetic/ rows.csv",
        ] {
            assert!(
                validate_safe_source_file(unsafe_path).is_err(),
                "unsafe path must fail: {unsafe_path}"
            );
        }
    }

    #[test]
    fn row_filter_and_lineage_contracts_are_explicit_and_value_free() {
        let filters = serde_json::to_value(ReconciliationRowFilters::default())
            .expect("serialize row filters");
        assert_eq!(
            filters.get("source_ordinal"),
            Some(&serde_json::Value::Null)
        );

        let reference = serde_json::to_value(ReconciliationLineageReferenceView {
            reference_kind: "CORRECTION_EFFECTIVATION".to_owned(),
            reference_id: "effectivation-1".to_owned(),
            reference_hash: "a".repeat(64),
        })
        .expect("serialize lineage reference");
        assert_eq!(reference.as_object().map(serde_json::Map::len), Some(3));
        assert!(reference.get("reference_kind").is_some());
        assert!(reference.get("reference_id").is_some());
        assert!(reference.get("reference_hash").is_some());
        assert!(reference.get("value").is_none());
    }
}

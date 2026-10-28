//! Bounded WP-005 practitioner reads over immutable decision evidence.
//!
//! Broad queue/count contracts never expose source, original, overlay, or
//! effective values. Those values appear only in exact issue/correction detail
//! reads after authority and integrity replay.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, decode_cursor, encode_cursor},
    decision_service::{DecisionCommandResult, load_command_result},
    effective_values::{effective_value_hash, load_effective_field},
    mapping::{MappingDocument, load_stored_mapping},
    profile_registry::{inspect_profile_connection, load_verified_profile_document},
    read_model::{
        CanonicalLineageView, PageInfo, PageRequest, practitioner_lineage,
        validate_accepted_archive,
    },
    validation::{verify_validation_issue_page_for_read, verify_validation_run_for_read},
};

const DECISION_QUEUE_SCHEMA: &str = "migration-stream/read/decision-queue/v1";
const DECISION_COUNTS_SCHEMA: &str = "migration-stream/read/decision-counts/v1";
const DECISION_ISSUE_SCHEMA: &str = "migration-stream/read/decision-aware-issue-detail/v1";
const DECISION_HISTORY_SCHEMA: &str = "migration-stream/read/decision-history/v1";
const CORRECTION_DETAIL_SCHEMA: &str = "migration-stream/read/correction-overlay-detail/v1";
const DECISION_RESULT_SCHEMA: &str = "migration-stream/read/decision-result/v1";
const DECISION_QUEUE_CURSOR: &str = "decision-queue/v1";
const DECISION_HISTORY_CURSOR: &str = "decision-history/v1";
const MAXIMUM_PAGE_SIZE: u32 = 200;
const MAXIMUM_GROUP_ITEMS: usize = 200;
const MAXIMUM_REASON_SUMMARY_CHARS: usize = 160;
const REQUIRED_SCHEMA_VERSION: i64 = crate::state::SCHEMA_VERSION;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionQueueFilters {
    pub disposition: Option<String>,
    pub severity: Option<String>,
    pub entity_id: Option<String>,
    pub field_id: Option<String>,
    pub rule_id: Option<String>,
    pub source_file: Option<String>,
    pub reevaluation_state: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionScopeView {
    pub job_id: String,
    pub validation_run_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSummary {
    pub decision_id: String,
    pub disposition: String,
    pub practitioner_identifier: String,
    pub applied_at: String,
    pub reason_summary: String,
    pub correction_overlay_id: Option<String>,
    pub reevaluation_id: Option<String>,
    pub reevaluation_state: String,
    pub generation: u64,
    pub supersession_count: u64,
    pub lifecycle_outcome: String,
    pub integrity_status: String,
    pub active: bool,
}

/// Value-free lifecycle evidence for the latest governed command attempt.
///
/// This summary is safe for broad queues: it exposes identities and lifecycle
/// state, but never source, original, overlay, or effective values and never a
/// practitioner reason.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAttemptSummary {
    pub decision_attempt_id: String,
    pub disposition: String,
    pub correction_overlay_id: Option<String>,
    pub reevaluation_id: Option<String>,
    pub attempt_state: String,
    pub reevaluation_state: String,
    pub lifecycle_outcome: String,
    pub integrity_status: String,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionQueueItem {
    pub issue_id: String,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub active_decision: Option<DecisionSummary>,
    pub latest_attempt: Option<DecisionAttemptSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionQueuePage {
    pub schema_version: String,
    pub scope: DecisionScopeView,
    pub filters: DecisionQueueFilters,
    pub items: Vec<DecisionQueueItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CountItem {
    pub key: String,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CountGroup {
    pub items: Vec<CountItem>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCountGroups {
    pub by_disposition: CountGroup,
    pub by_severity: CountGroup,
    pub by_entity: CountGroup,
    pub by_field: CountGroup,
    pub by_rule: CountGroup,
    pub by_source_file: CountGroup,
    pub by_reevaluation_state: CountGroup,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCounts {
    pub schema_version: String,
    pub scope: DecisionScopeView,
    pub groups: DecisionCountGroups,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedRowView {
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionConstraints {
    pub maximum_characters: Option<u64>,
    pub allow_empty: bool,
    pub allow_null: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionFieldContext {
    pub received_source_value: Option<String>,
    pub original_canonical_value: Option<String>,
    pub original_lineage: CanonicalLineageView,
    pub active_overlay_value: Option<String>,
    pub effective_canonical_value: Option<String>,
    pub effective_value_hash: String,
    pub active_overlay_id: Option<String>,
    pub correction_eligible: bool,
    pub allowed_operations: Vec<String>,
    pub constraints: CorrectionConstraints,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionConcurrencyView {
    pub expected_active_decision_id: Option<String>,
    pub expected_active_generation: u64,
    pub expected_effective_value_hash: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAwareIssueData {
    pub job_id: String,
    pub validation_run_id: String,
    pub issue_id: String,
    pub issue_hash: String,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    pub accepted_row: AcceptedRowView,
    pub field_context: DecisionFieldContext,
    pub active_decision: Option<DecisionSummary>,
    pub latest_attempt: Option<DecisionAttemptSummary>,
    pub decision_history_count: u64,
    pub concurrency: DecisionConcurrencyView,
    pub authority_notice: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAwareIssueDetail {
    pub schema_version: String,
    pub data: DecisionAwareIssueData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionHistoryItem {
    pub decision_id: String,
    pub disposition: String,
    pub practitioner_identifier: String,
    pub applied_at: String,
    pub reason: String,
    pub evidence_reference: Option<String>,
    pub prior_decision_id: Option<String>,
    pub generation: u64,
    pub correction_overlay_id: Option<String>,
    pub reevaluation_id: Option<String>,
    pub canonical_hash: String,
    pub lifecycle_outcome: String,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionHistoryScope {
    pub job_id: String,
    pub validation_run_id: String,
    pub issue_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionHistoryPage {
    pub schema_version: String,
    pub scope: DecisionHistoryScope,
    pub items: Vec<DecisionHistoryItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionOverlayData {
    pub overlay_id: String,
    pub decision_attempt_id: String,
    pub decision_id: Option<String>,
    pub job_id: String,
    pub validation_run_id: String,
    pub issue_id: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub entity_id: String,
    pub field_id: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub operation: String,
    pub original_canonical_value: Option<String>,
    pub original_lineage: CanonicalLineageView,
    pub prior_effective_value: Option<String>,
    pub overlay_value: Option<String>,
    pub effective_canonical_value: Option<String>,
    pub effective_value_hash: Option<String>,
    pub practitioner_identifier: String,
    pub reason: String,
    pub attempted_at: String,
    pub applied_at: Option<String>,
    pub active: bool,
    pub superseded_by_overlay_id: Option<String>,
    pub reevaluation_id: Option<String>,
    pub attempt_state: String,
    pub reevaluation_state: String,
    pub lifecycle_outcome: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub integrity_status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionOverlayDetail {
    pub schema_version: String,
    pub data: CorrectionOverlayData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug)]
pub struct DecisionReadService {
    workspace: PathBuf,
    query_timeout: Duration,
}

#[allow(clippy::missing_errors_doc)]
impl DecisionReadService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            query_timeout: Duration::from_secs(5),
        }
    }

    pub fn decision_queue(
        &self,
        job_id: &str,
        validation_run_id: &str,
        filters: &DecisionQueueFilters,
        page: &PageRequest,
    ) -> Result<DecisionQueuePage> {
        validate_filters(filters)?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        verify_scope(&connection, job_id, validation_run_id)?;
        let scope = DecisionScopeView {
            job_id: job_id.to_owned(),
            validation_run_id: validation_run_id.to_owned(),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = canonical_hash(&serde_json::to_value(filters)?)?;
        let after_issue = page
            .cursor
            .as_deref()
            .map(|cursor| {
                let payload =
                    decode_cursor(cursor, DECISION_QUEUE_CURSOR, &scope_hash, &filter_hash)?;
                cursor_string(&payload.last_key, DECISION_QUEUE_CURSOR)
            })
            .transpose()?;

        let (sql, values) = queue_sql(
            validation_run_id,
            filters,
            after_issue.as_deref(),
            page_size + 1,
        );
        let mut statement = connection.prepare(&sql)?;
        let mapped = statement.query_map(params_from_iter(values), decision_queue_row)?;
        let mut keyed = mapped.collect::<std::result::Result<Vec<_>, _>>()?;
        let next_cursor = if keyed.len() > usize::try_from(page_size).unwrap_or(usize::MAX) {
            keyed.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
            let last = keyed.last().ok_or_else(|| {
                KernelError::DecisionIntegrity("decision queue lost its last item".to_owned())
            })?;
            Some(encode_cursor(
                DECISION_QUEUE_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([last.0]),
            )?)
        } else {
            None
        };
        let items = keyed.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let mut response = DecisionQueuePage {
            schema_version: DECISION_QUEUE_SCHEMA.to_owned(),
            scope,
            filters: filters.clone(),
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn decision_counts(&self, job_id: &str, validation_run_id: &str) -> Result<DecisionCounts> {
        let connection = self.open_snapshot()?;
        verify_scope(&connection, job_id, validation_run_id)?;
        let mut maps = (
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let count_sql = format!(
            "SELECT COALESCE(d.disposition, 'UNRESOLVED'), vi.severity,
                    vi.entity_id, vi.field_id, vi.rule_id, vi.source_file,
                    COALESCE({}, 'NOT_REQUIRED')
             FROM validation_issues vi
             LEFT JOIN issue_decisions d
               ON d.validation_run_id = vi.run_id AND d.issue_id = vi.issue_id
              AND d.generation = (
                  SELECT MAX(head.generation) FROM issue_decisions head
                  WHERE head.validation_run_id = vi.run_id
                    AND head.issue_id = vi.issue_id
              )
             LEFT JOIN decision_attempts a
               ON a.attempt_id = (
                  SELECT latest.attempt_id FROM decision_attempts latest
                  WHERE latest.validation_run_id = vi.run_id
                    AND latest.issue_id = vi.issue_id
                  ORDER BY latest.created_at DESC, latest.attempt_id DESC LIMIT 1
               )
             LEFT JOIN issue_decisions ad ON ad.decision_attempt_id = a.attempt_id
             LEFT JOIN reevaluation_runs r
               ON r.reevaluation_id = (
                  SELECT latest.reevaluation_id FROM reevaluation_runs latest
                  WHERE latest.decision_attempt_id = a.attempt_id
                  ORDER BY latest.created_at DESC, latest.reevaluation_id DESC LIMIT 1
               )
             LEFT JOIN decision_attempt_events ae
               ON ae.sequence = (
                  SELECT MAX(latest.sequence) FROM decision_attempt_events latest
                  WHERE latest.attempt_id = a.attempt_id
               )
             WHERE vi.run_id = ?1 ORDER BY vi.issue_id",
            reevaluation_state_sql()
        );
        let mut statement = connection.prepare(&count_sql)?;
        for row in statement.query_map([validation_run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })? {
            let row = row?;
            increment(&mut maps.0, row.0)?;
            increment(&mut maps.1, row.1)?;
            increment(&mut maps.2, row.2)?;
            increment(&mut maps.3, row.3)?;
            increment(&mut maps.4, row.4)?;
            increment(&mut maps.5, row.5)?;
            increment(&mut maps.6, row.6)?;
        }
        let mut response = DecisionCounts {
            schema_version: DECISION_COUNTS_SCHEMA.to_owned(),
            scope: DecisionScopeView {
                job_id: job_id.to_owned(),
                validation_run_id: validation_run_id.to_owned(),
            },
            groups: DecisionCountGroups {
                by_disposition: count_group(maps.0),
                by_severity: count_group(maps.1),
                by_entity: count_group(maps.2),
                by_field: count_group(maps.3),
                by_rule: count_group(maps.4),
                by_source_file: count_group(maps.5),
                by_reevaluation_state: count_group(maps.6),
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    #[allow(clippy::items_after_statements, clippy::too_many_lines)]
    pub fn issue_detail(
        &self,
        job_id: &str,
        validation_run_id: &str,
        issue_id: &str,
    ) -> Result<DecisionAwareIssueDetail> {
        let connection = self.open_snapshot()?;
        verify_scope(&connection, job_id, validation_run_id)?;
        verify_validation_issue_page_for_read(
            &connection,
            validation_run_id,
            &[issue_id.to_owned()],
        )?;
        type IssueRow = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let issue: IssueRow = connection
            .query_row(
                "SELECT vi.issue_hash, vi.severity, vi.rule_id, vi.issue_code,
                    vi.display_message, vi.entity_id, vi.field_id, vi.source_file,
                    vi.source_ordinal, vi.line_start, vi.line_end,
                    vr.profile_id, vr.profile_version, vr.profile_hash,
                    vr.mapping_id, vr.mapping_version, vr.mapping_hash
             FROM validation_issues vi JOIN validation_runs vr ON vr.id = vi.run_id
             WHERE vi.run_id = ?1 AND vi.issue_id = ?2 AND vr.job_id = ?3",
                params![validation_run_id, issue_id, job_id],
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
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        row.get(14)?,
                        row.get(15)?,
                        row.get(16)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| KernelError::NotFound(format!("issue {issue_id}")))?;
        let scope = crate::decisions::DecisionScope {
            job_id: job_id.to_owned(),
            validation_run_id: validation_run_id.to_owned(),
            issue_id: issue_id.to_owned(),
            issue_hash: issue.0.clone(),
            accepted_source_file: issue.7.clone(),
            source_ordinal: nonnegative_u64(issue.8, "issue ordinal")?,
            line_start: nonnegative_u64(issue.9, "line start")?,
            line_end: nonnegative_u64(issue.10, "line end")?,
            canonical_entity_id: issue.5.clone(),
            canonical_field_id: issue.6.clone(),
            profile_id: issue.11.clone(),
            profile_version: issue.12.clone(),
            profile_hash: issue.13.clone(),
            mapping_id: issue.14.clone(),
            mapping_version: issue.15.clone(),
            mapping_hash: issue.16.clone(),
        };
        let mapping = load_stored_mapping(&connection, &scope.mapping_id, &scope.mapping_version)?;
        if mapping.mapping_hash != scope.mapping_hash {
            return Err(KernelError::DecisionIntegrity(
                "mapping hash differs in decision detail".to_owned(),
            ));
        }
        let effective = load_effective_field(&connection, &scope, &mapping.document)?;
        let profile = load_verified_profile_document(
            &connection,
            &scope.profile_id,
            &scope.profile_version,
            &scope.profile_hash,
        )?;
        let profile_view = inspect_profile_connection(
            &connection,
            &scope.profile_id,
            &scope.profile_version,
            &scope.profile_hash,
        )?;
        let eligibility = read_eligibility(
            &profile,
            &mapping.document,
            &scope,
            profile_view.can_start_new_runs,
        );
        let active_decision = active_summary(&connection, validation_run_id, issue_id)?;
        let history_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM issue_decisions WHERE validation_run_id = ?1 AND issue_id = ?2",
            params![validation_run_id, issue_id],
            |row| row.get(0),
        )?;
        let generation = active_decision
            .as_ref()
            .map_or(0, |decision| decision.generation);
        let active_id = active_decision
            .as_ref()
            .map(|decision| decision.decision_id.clone());
        let latest_attempt = latest_attempt_summary(
            &connection,
            validation_run_id,
            issue_id,
            active_id.as_deref(),
        )?;
        let active_overlay_value = effective
            .active_overlay_id
            .as_ref()
            .and_then(|_| effective.value.clone());
        let original_lineage = practitioner_lineage(&effective.original_lineage)?;
        let mut response = DecisionAwareIssueDetail {
            schema_version: DECISION_ISSUE_SCHEMA.to_owned(),
            data: DecisionAwareIssueData {
                job_id: job_id.to_owned(),
                validation_run_id: validation_run_id.to_owned(),
                issue_id: issue_id.to_owned(),
                issue_hash: issue.0,
                severity: issue.1,
                rule_id: issue.2,
                issue_code: issue.3,
                display_message: issue.4,
                entity_id: issue.5,
                field_id: issue.6,
                accepted_row: AcceptedRowView {
                    source_file: issue.7,
                    source_ordinal: scope.source_ordinal,
                    line_start: scope.line_start,
                    line_end: scope.line_end,
                },
                field_context: DecisionFieldContext {
                    received_source_value: effective.received_source_value,
                    original_canonical_value: effective.original_value,
                    original_lineage,
                    active_overlay_value,
                    effective_canonical_value: effective.value,
                    effective_value_hash: effective.effective_value_hash.clone(),
                    active_overlay_id: effective.active_overlay_id,
                    correction_eligible: eligibility.0,
                    allowed_operations: eligibility.1,
                    constraints: eligibility.2,
                },
                active_decision,
                latest_attempt,
                decision_history_count: nonnegative_u64(history_count, "decision history count")?,
                concurrency: DecisionConcurrencyView {
                    expected_active_decision_id: active_id,
                    expected_active_generation: generation,
                    expected_effective_value_hash: effective.effective_value_hash,
                },
                authority_notice:
                    "This decision is practitioner evidence, not migration-result approval."
                        .to_owned(),
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn decision_history(
        &self,
        job_id: &str,
        validation_run_id: &str,
        issue_id: &str,
        page: &PageRequest,
    ) -> Result<DecisionHistoryPage> {
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        verify_scope(&connection, job_id, validation_run_id)?;
        let scope = DecisionHistoryScope {
            job_id: job_id.to_owned(),
            validation_run_id: validation_run_id.to_owned(),
            issue_id: issue_id.to_owned(),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = canonical_hash(&json!({}))?;
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| {
                let payload =
                    decode_cursor(cursor, DECISION_HISTORY_CURSOR, &scope_hash, &filter_hash)?;
                cursor_u64(&payload.last_key, DECISION_HISTORY_CURSOR)
            })
            .transpose()?
            .unwrap_or(0);
        let maximum: i64 = connection.query_row("SELECT COALESCE(MAX(generation),0) FROM issue_decisions WHERE validation_run_id=?1 AND issue_id=?2", params![validation_run_id,issue_id], |row| row.get(0))?;
        let mut statement = connection.prepare(
            "SELECT decision_id, disposition, practitioner_id, committed_at, reason,
                    evidence_reference, predecessor_decision_id, generation,
                    correction_overlay_id, reevaluation_id, decision_hash, lifecycle_outcome
             FROM issue_decisions WHERE validation_run_id=?1 AND issue_id=?2 AND generation>?3
             ORDER BY generation, decision_id LIMIT ?4",
        )?;
        let mapped = statement.query_map(
            params![
                validation_run_id,
                issue_id,
                sqlite_i64(after, "history cursor")?,
                sqlite_i64(u64::from(page_size + 1), "history limit")?
            ],
            |row| {
                let generation: i64 = row.get(7)?;
                Ok(DecisionHistoryItem {
                    decision_id: row.get(0)?,
                    disposition: row.get(1)?,
                    practitioner_identifier: row.get(2)?,
                    applied_at: row.get(3)?,
                    reason: row.get(4)?,
                    evidence_reference: row.get(5)?,
                    prior_decision_id: row.get(6)?,
                    generation: u64::try_from(generation).unwrap_or(u64::MAX),
                    correction_overlay_id: row.get(8)?,
                    reevaluation_id: row.get(9)?,
                    canonical_hash: row.get(10)?,
                    lifecycle_outcome: row.get(11)?,
                    active: generation == maximum,
                })
            },
        )?;
        let mut items = mapped.collect::<std::result::Result<Vec<_>, _>>()?;
        let next_cursor = if items.len() > usize::try_from(page_size).unwrap_or(usize::MAX) {
            items.truncate(usize::try_from(page_size).unwrap_or(usize::MAX));
            Some(encode_cursor(
                DECISION_HISTORY_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([items
                    .last()
                    .ok_or_else(|| KernelError::DecisionIntegrity(
                        "history lost last item".to_owned()
                    ))?
                    .generation]),
            )?)
        } else {
            None
        };
        let mut response = DecisionHistoryPage {
            schema_version: DECISION_HISTORY_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    #[allow(clippy::items_after_statements, clippy::too_many_lines)]
    pub fn correction_detail(&self, overlay_id: &str) -> Result<CorrectionOverlayDetail> {
        let connection = self.open_snapshot()?;
        struct OverlayRow {
            attempt_id: String,
            decision_id: Option<String>,
            job_id: String,
            validation_run_id: String,
            issue_id: String,
            source_file: String,
            source_ordinal: i64,
            line_start: i64,
            line_end: i64,
            entity_id: String,
            field_id: String,
            profile_id: String,
            profile_version: String,
            profile_hash: String,
            mapping_id: String,
            mapping_version: String,
            mapping_hash: String,
            operation: String,
            replacement_text: Option<String>,
            effective_value_text: Option<String>,
            effective_value_hash: Option<String>,
            practitioner_id: String,
            reason: String,
            attempted_at: String,
            reevaluation_id: Option<String>,
            reevaluation_state: Option<String>,
            error_code: Option<String>,
            error_message: Option<String>,
            applied_at: Option<String>,
            decision_outcome: Option<String>,
            attempt_state: String,
            prior_effective_value_hash: String,
        }
        let row: OverlayRow = connection
            .query_row(
                "SELECT o.decision_attempt_id,d.decision_id,
                    a.job_id,a.validation_run_id,a.issue_id,
                    a.accepted_source_file,a.source_ordinal,a.line_start,a.line_end,
                    a.canonical_entity_id,a.canonical_field_id,
                    a.profile_id,a.profile_version,a.profile_hash,
                    a.mapping_id,a.mapping_version,a.mapping_hash,
                    o.operation,o.replacement_text,ce.effective_value_text,
                    ce.effective_value_hash,o.practitioner_id,o.reason,o.created_at,
                    r.reevaluation_id,r.state,r.error_code,r.error_message,
                    ce.effectivated_at,d.lifecycle_outcome,ae.state,
                    o.prior_effective_value_hash
             FROM correction_overlays o
             JOIN decision_attempts a ON a.attempt_id=o.decision_attempt_id
             JOIN decision_attempt_events ae
               ON ae.sequence=(
                  SELECT MAX(latest.sequence) FROM decision_attempt_events latest
                  WHERE latest.attempt_id=o.decision_attempt_id
               )
             LEFT JOIN issue_decisions d ON d.correction_overlay_id=o.overlay_id
             LEFT JOIN correction_effectivations ce
               ON ce.correction_overlay_id=o.overlay_id
             LEFT JOIN reevaluation_runs r
               ON r.reevaluation_id=COALESCE(
                  d.reevaluation_id,
                  (SELECT latest.reevaluation_id FROM reevaluation_runs latest
                   WHERE latest.decision_attempt_id=o.decision_attempt_id
                   ORDER BY latest.created_at DESC,latest.reevaluation_id DESC LIMIT 1)
               )
             WHERE o.overlay_id=?1",
                [overlay_id],
                |r| {
                    Ok(OverlayRow {
                        attempt_id: r.get(0)?,
                        decision_id: r.get(1)?,
                        job_id: r.get(2)?,
                        validation_run_id: r.get(3)?,
                        issue_id: r.get(4)?,
                        source_file: r.get(5)?,
                        source_ordinal: r.get(6)?,
                        line_start: r.get(7)?,
                        line_end: r.get(8)?,
                        entity_id: r.get(9)?,
                        field_id: r.get(10)?,
                        profile_id: r.get(11)?,
                        profile_version: r.get(12)?,
                        profile_hash: r.get(13)?,
                        mapping_id: r.get(14)?,
                        mapping_version: r.get(15)?,
                        mapping_hash: r.get(16)?,
                        operation: r.get(17)?,
                        replacement_text: r.get(18)?,
                        effective_value_text: r.get(19)?,
                        effective_value_hash: r.get(20)?,
                        practitioner_id: r.get(21)?,
                        reason: r.get(22)?,
                        attempted_at: r.get(23)?,
                        reevaluation_id: r.get(24)?,
                        reevaluation_state: r.get(25)?,
                        error_code: r.get(26)?,
                        error_message: r.get(27)?,
                        applied_at: r.get(28)?,
                        decision_outcome: r.get(29)?,
                        attempt_state: r.get(30)?,
                        prior_effective_value_hash: r.get(31)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| KernelError::NotFound(format!("correction overlay {overlay_id}")))?;
        verify_scope(&connection, &row.job_id, &row.validation_run_id)?;
        let scope = crate::decisions::DecisionScope {
            job_id: row.job_id.clone(),
            validation_run_id: row.validation_run_id.clone(),
            issue_id: row.issue_id.clone(),
            issue_hash: connection.query_row(
                "SELECT issue_hash FROM validation_issues WHERE run_id=?1 AND issue_id=?2",
                params![row.validation_run_id, row.issue_id],
                |r| r.get(0),
            )?,
            accepted_source_file: row.source_file.clone(),
            source_ordinal: nonnegative_u64(row.source_ordinal, "overlay ordinal")?,
            line_start: nonnegative_u64(row.line_start, "overlay line start")?,
            line_end: nonnegative_u64(row.line_end, "overlay line end")?,
            canonical_entity_id: row.entity_id.clone(),
            canonical_field_id: row.field_id.clone(),
            profile_id: row.profile_id.clone(),
            profile_version: row.profile_version.clone(),
            profile_hash: row.profile_hash.clone(),
            mapping_id: row.mapping_id.clone(),
            mapping_version: row.mapping_version.clone(),
            mapping_hash: row.mapping_hash.clone(),
        };
        let mapping = load_stored_mapping(&connection, &scope.mapping_id, &scope.mapping_version)?;
        let current = load_effective_field(&connection, &scope, &mapping.document)?;
        let specific_value = operation_value(&row.operation, row.replacement_text.clone())?;
        let has_authority = row.decision_id.is_some();
        if has_authority
            != (row.effective_value_hash.is_some()
                && row.applied_at.is_some()
                && row.decision_outcome.is_some())
        {
            return Err(KernelError::DecisionIntegrity(
                "correction overlay authority and effectivation evidence disagree".to_owned(),
            ));
        }
        let initial_hash = effective_value_hash(
            &scope,
            &current.original_value_hash,
            None,
            None,
            0,
            &current.original_value,
        )?;
        let prior = if row.prior_effective_value_hash == initial_hash {
            current.original_value.clone()
        } else {
            connection
                .query_row(
                    "SELECT prior.effective_value_text FROM correction_effectivations prior
             JOIN issue_decisions pd ON pd.decision_id=prior.decision_id
             JOIN decision_attempts pa ON pa.attempt_id=pd.decision_attempt_id
             WHERE pa.job_id=?1 AND pa.validation_run_id=?2
               AND pa.accepted_source_file=?3 AND pa.source_ordinal=?4
               AND pa.canonical_entity_id=?5 AND pa.canonical_field_id=?6
               AND prior.effective_value_hash=?7",
                    params![
                        scope.job_id,
                        scope.validation_run_id,
                        scope.accepted_source_file,
                        sqlite_i64(scope.source_ordinal, "overlay ordinal")?,
                        scope.canonical_entity_id,
                        scope.canonical_field_id,
                        row.prior_effective_value_hash
                    ],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| {
                    KernelError::DecisionIntegrity(
                        "correction prior effective value is absent from its hash chain".to_owned(),
                    )
                })?
        };
        let superseded_by = if let Some(effective_hash) = row.effective_value_hash.as_deref() {
            let mut successor_statement = connection.prepare(
                "SELECT next.correction_overlay_id FROM correction_effectivations next
                 JOIN issue_decisions nd ON nd.decision_id=next.decision_id
                 JOIN decision_attempts na ON na.attempt_id=nd.decision_attempt_id
                 WHERE na.job_id=?1 AND na.validation_run_id=?2
                   AND na.accepted_source_file=?3 AND na.source_ordinal=?4
                   AND na.canonical_entity_id=?5 AND na.canonical_field_id=?6
                   AND next.prior_effective_value_hash=?7
                 ORDER BY next.correction_overlay_id LIMIT 2",
            )?;
            let successors = successor_statement
                .query_map(
                    params![
                        scope.job_id,
                        scope.validation_run_id,
                        scope.accepted_source_file,
                        sqlite_i64(scope.source_ordinal, "overlay ordinal")?,
                        scope.canonical_entity_id,
                        scope.canonical_field_id,
                        effective_hash
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if successors.len() > 1 {
                return Err(KernelError::DecisionIntegrity(
                    "correction effectivation history forks".to_owned(),
                ));
            }
            successors.into_iter().next()
        } else {
            None
        };
        let active = has_authority && current.active_overlay_id.as_deref() == Some(overlay_id);
        let reevaluation_state = reevaluation_state_from_evidence(
            "CORRECTED",
            row.decision_id.as_deref(),
            row.reevaluation_state.as_deref(),
            row.reevaluation_state.as_deref(),
            &row.attempt_state,
        );
        let lifecycle_outcome = if has_authority {
            if active { "EFFECTIVE" } else { "SUPERSEDED" }.to_owned()
        } else {
            lifecycle_outcome_from_evidence(None, &reevaluation_state)
        };
        let original_lineage = practitioner_lineage(&current.original_lineage)?;
        let mut response = CorrectionOverlayDetail {
            schema_version: CORRECTION_DETAIL_SCHEMA.to_owned(),
            data: CorrectionOverlayData {
                overlay_id: overlay_id.to_owned(),
                decision_attempt_id: row.attempt_id,
                decision_id: row.decision_id,
                job_id: row.job_id,
                validation_run_id: row.validation_run_id,
                issue_id: row.issue_id,
                source_file: row.source_file,
                source_ordinal: scope.source_ordinal,
                entity_id: row.entity_id,
                field_id: row.field_id,
                profile_id: row.profile_id,
                profile_version: row.profile_version,
                profile_hash: row.profile_hash,
                mapping_id: row.mapping_id,
                mapping_version: row.mapping_version,
                mapping_hash: row.mapping_hash,
                operation: row.operation,
                original_canonical_value: current.original_value.clone(),
                original_lineage,
                prior_effective_value: prior,
                overlay_value: specific_value,
                effective_canonical_value: row.effective_value_text,
                effective_value_hash: row.effective_value_hash,
                practitioner_identifier: row.practitioner_id,
                reason: row.reason,
                attempted_at: row.attempted_at,
                applied_at: row.applied_at,
                active,
                superseded_by_overlay_id: superseded_by,
                reevaluation_id: row.reevaluation_id,
                attempt_state: row.attempt_state,
                lifecycle_outcome,
                error_code: row.error_code,
                error_message: row.error_message,
                integrity_status: integrity_status_for_state(&reevaluation_state).to_owned(),
                reevaluation_state,
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    pub fn correction_detail_for_job(
        &self,
        job_id: &str,
        overlay_id: &str,
    ) -> Result<CorrectionOverlayDetail> {
        let response = self.correction_detail(overlay_id)?;
        if response.data.job_id != job_id {
            return Err(KernelError::NotFound(format!(
                "correction overlay {overlay_id}"
            )));
        }
        Ok(response)
    }

    pub fn decision_result(&self, decision_id: &str) -> Result<DecisionCommandResult> {
        let connection = self.open_snapshot()?;
        let (job_id, validation_run_id) = connection
            .query_row(
                "SELECT a.job_id,a.validation_run_id
                 FROM issue_decisions d
                 JOIN decision_attempts a ON a.attempt_id=d.decision_attempt_id
                 WHERE d.decision_id=?1",
                [decision_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| KernelError::NotFound(format!("decision {decision_id}")))?;
        verify_scope(&connection, &job_id, &validation_run_id)?;
        let mut result = load_command_result(&connection, decision_id, false)?;
        DECISION_RESULT_SCHEMA.clone_into(&mut result.schema_version);
        result.canonical_hash = response_hash(&result)?;
        Ok(result)
    }

    pub fn decision_result_for_job(
        &self,
        job_id: &str,
        decision_id: &str,
    ) -> Result<DecisionCommandResult> {
        let connection = self.open_snapshot()?;
        let validation_run_id: Option<String> = connection
            .query_row(
                "SELECT a.validation_run_id FROM issue_decisions d
                 JOIN decision_attempts a ON a.attempt_id = d.decision_attempt_id
                 WHERE d.decision_id = ?1 AND a.job_id = ?2",
                params![decision_id, job_id],
                |row| row.get(0),
            )
            .optional()?;
        let validation_run_id = validation_run_id
            .ok_or_else(|| KernelError::NotFound(format!("decision {decision_id}")))?;
        verify_scope(&connection, job_id, &validation_run_id)?;
        let mut result = load_command_result(&connection, decision_id, false)?;
        DECISION_RESULT_SCHEMA.clone_into(&mut result.schema_version);
        result.canonical_hash = response_hash(&result)?;
        Ok(result)
    }

    fn open_snapshot(&self) -> Result<Connection> {
        let mut connection = crate::state::open_read_only(&self.workspace.join("state.sqlite3"))?;
        let started = Instant::now();
        let timeout = self.query_timeout;
        connection.progress_handler(100, Some(move || started.elapsed() >= timeout))?;
        connection.execute_batch("BEGIN DEFERRED;")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != REQUIRED_SCHEMA_VERSION {
            return Err(KernelError::DecisionIntegrity(format!(
                "decision reads require schema version {REQUIRED_SCHEMA_VERSION}; found {version}"
            )));
        }
        let _ = crate::decision_integrity::verify_decision_integrity_connection(&connection)?;
        Ok(connection)
    }
}

fn verify_scope(connection: &Connection, job_id: &str, run_id: &str) -> Result<()> {
    verify_validation_run_for_read(connection, run_id)?;
    let found: Option<(String, String)> = connection
        .query_row(
            "SELECT job_id,source_hash FROM validation_runs
             WHERE id=?1 AND state='COMPLETE'",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((run_job_id, run_source_hash)) = found else {
        return Err(KernelError::DecisionInvalid(
            "validation run is not COMPLETE in the selected job".to_owned(),
        ));
    };
    if run_job_id != job_id {
        return Err(KernelError::DecisionInvalid(
            "validation run is not COMPLETE in the selected job".to_owned(),
        ));
    }
    verify_accepted_job(connection, job_id, &run_source_hash)?;
    Ok(())
}

fn verify_accepted_job(
    connection: &Connection,
    job_id: &str,
    expected_source_hash: &str,
) -> Result<()> {
    let job: Option<(String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT state,source_hash,stored_archive FROM jobs WHERE id=?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((state, source_hash, stored_archive)) = job else {
        return Err(KernelError::DecisionIntegrity(
            "governed read has no accepted source job".to_owned(),
        ));
    };
    let source_hash = source_hash.ok_or_else(|| {
        KernelError::DecisionIntegrity("accepted source job has no source hash".to_owned())
    })?;
    let stored_archive = stored_archive.ok_or_else(|| {
        KernelError::DecisionIntegrity("accepted source job has no archive identity".to_owned())
    })?;
    if state != "COMPLETE" || source_hash != expected_source_hash {
        return Err(KernelError::DecisionIntegrity(
            "governed read source differs from its complete validation authority".to_owned(),
        ));
    }
    validate_accepted_archive(&source_hash, &stored_archive)
        .map_err(|error| KernelError::DecisionIntegrity(error.to_string()))
}

fn decision_queue_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, DecisionQueueItem)> {
    let generation: Option<i64> = row.get(18)?;
    let active_decision = if let Some(generation) = generation {
        Some(decision_summary_from_columns(
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
            row.get(14)?,
            row.get(15)?,
            row.get(16)?,
            row.get(17)?,
            generation,
            row.get(19)?,
        ))
    } else {
        None
    };
    let latest_attempt = if let Some(attempt_id) = row.get::<_, Option<String>>(20)? {
        let disposition = row.get::<_, String>(21)?;
        let attempt_state = row.get::<_, String>(24)?;
        let run_state = row.get::<_, Option<String>>(25)?;
        let decision_id = row.get::<_, Option<String>>(26)?;
        let decision_reevaluation_state = row.get::<_, Option<String>>(27)?;
        let decision_outcome = row.get::<_, Option<String>>(28)?;
        let reevaluation_state = reevaluation_state_from_evidence(
            &disposition,
            decision_id.as_deref(),
            decision_reevaluation_state.as_deref(),
            run_state.as_deref(),
            &attempt_state,
        );
        Some(DecisionAttemptSummary {
            decision_attempt_id: attempt_id,
            disposition,
            correction_overlay_id: row.get(22)?,
            reevaluation_id: row.get(23)?,
            attempt_state,
            lifecycle_outcome: lifecycle_outcome_from_evidence(
                decision_outcome.as_deref(),
                &reevaluation_state,
            ),
            integrity_status: integrity_status_for_state(&reevaluation_state).to_owned(),
            reevaluation_state,
            active: row.get::<_, i64>(29)? == 1,
        })
    } else {
        None
    };
    Ok((
        row.get::<_, String>(0)?,
        DecisionQueueItem {
            issue_id: row.get(0)?,
            severity: row.get(1)?,
            rule_id: row.get(2)?,
            issue_code: row.get(3)?,
            display_message: row.get(4)?,
            entity_id: row.get(5)?,
            field_id: row.get(6)?,
            source_file: row.get(7)?,
            source_ordinal: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(u64::MAX),
            active_decision,
            latest_attempt,
        },
    ))
}

fn queue_sql(
    run_id: &str,
    filters: &DecisionQueueFilters,
    after: Option<&str>,
    limit: u32,
) -> (String, Vec<SqlValue>) {
    let mut sql = "SELECT
            vi.issue_id,vi.severity,vi.rule_id,vi.issue_code,vi.display_message,
            vi.entity_id,vi.field_id,vi.source_file,vi.source_ordinal,vi.issue_hash,
            d.decision_id,d.disposition,d.practitioner_id,d.committed_at,d.reason,
            d.correction_overlay_id,d.reevaluation_id,d.reevaluation_state,
            d.generation,d.lifecycle_outcome,
            a.attempt_id,a.disposition,o.overlay_id,r.reevaluation_id,ae.state,r.state,
            ad.decision_id,ad.reevaluation_state,ad.lifecycle_outcome,
            CASE WHEN ad.decision_id = d.decision_id THEN 1 ELSE 0 END
         FROM validation_issues vi
         LEFT JOIN issue_decisions d
           ON d.validation_run_id=vi.run_id AND d.issue_id=vi.issue_id
          AND d.generation=(
              SELECT MAX(head.generation) FROM issue_decisions head
              WHERE head.validation_run_id=vi.run_id AND head.issue_id=vi.issue_id
          )
         LEFT JOIN decision_attempts a
           ON a.attempt_id=(
              SELECT latest.attempt_id FROM decision_attempts latest
              WHERE latest.validation_run_id=vi.run_id AND latest.issue_id=vi.issue_id
              ORDER BY latest.created_at DESC,latest.attempt_id DESC LIMIT 1
          )
         LEFT JOIN correction_overlays o ON o.decision_attempt_id=a.attempt_id
         LEFT JOIN issue_decisions ad ON ad.decision_attempt_id=a.attempt_id
         LEFT JOIN reevaluation_runs r
           ON r.reevaluation_id=(
              SELECT latest.reevaluation_id FROM reevaluation_runs latest
              WHERE latest.decision_attempt_id=a.attempt_id
              ORDER BY latest.created_at DESC,latest.reevaluation_id DESC LIMIT 1
          )
         LEFT JOIN decision_attempt_events ae
           ON ae.sequence=(
              SELECT MAX(latest.sequence) FROM decision_attempt_events latest
              WHERE latest.attempt_id=a.attempt_id
          )
         WHERE vi.run_id=?"
        .to_owned();
    let mut values = vec![SqlValue::Text(run_id.to_owned())];
    if let Some(value) = &filters.disposition {
        if value == "UNRESOLVED" {
            sql.push_str(" AND d.decision_id IS NULL");
        } else {
            push_sql_value(&mut sql, &mut values, " AND d.disposition=?", value.clone());
        }
    }
    if let Some(value) = &filters.severity {
        push_sql_value(&mut sql, &mut values, " AND vi.severity=?", value.clone());
    }
    if let Some(value) = &filters.entity_id {
        push_sql_value(&mut sql, &mut values, " AND vi.entity_id=?", value.clone());
    }
    if let Some(value) = &filters.field_id {
        push_sql_value(&mut sql, &mut values, " AND vi.field_id=?", value.clone());
    }
    if let Some(value) = &filters.rule_id {
        push_sql_value(&mut sql, &mut values, " AND vi.rule_id=?", value.clone());
    }
    if let Some(value) = &filters.source_file {
        push_sql_value(
            &mut sql,
            &mut values,
            " AND vi.source_file=?",
            value.clone(),
        );
    }
    if let Some(value) = &filters.reevaluation_state {
        sql.push_str(" AND COALESCE(");
        sql.push_str(reevaluation_state_sql());
        sql.push_str(", 'NOT_REQUIRED')=?");
        values.push(SqlValue::Text(value.clone()));
    }
    if let Some(value) = after {
        push_sql_value(
            &mut sql,
            &mut values,
            " AND vi.issue_id>?",
            value.to_owned(),
        );
    }
    sql.push_str(" ORDER BY vi.issue_id LIMIT ?");
    values.push(SqlValue::Integer(i64::from(limit)));
    (sql, values)
}

fn push_sql_value(sql: &mut String, values: &mut Vec<SqlValue>, clause: &str, value: String) {
    sql.push_str(clause);
    values.push(SqlValue::Text(value));
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn decision_summary_from_columns(
    id: String,
    disposition: String,
    actor: String,
    applied: String,
    reason: String,
    overlay: Option<String>,
    reeval: Option<String>,
    reeval_state: Option<String>,
    generation: i64,
    outcome: String,
) -> DecisionSummary {
    let generation = u64::try_from(generation).unwrap_or(u64::MAX);
    DecisionSummary {
        decision_id: id,
        disposition,
        practitioner_identifier: actor,
        applied_at: applied,
        reason_summary: reason.chars().take(MAXIMUM_REASON_SUMMARY_CHARS).collect(),
        correction_overlay_id: overlay,
        reevaluation_id: reeval,
        reevaluation_state: reeval_state.unwrap_or_else(|| "NOT_REQUIRED".to_owned()),
        generation,
        supersession_count: generation.saturating_sub(1),
        lifecycle_outcome: outcome,
        integrity_status: "VERIFIED".to_owned(),
        active: true,
    }
}

fn active_summary(
    connection: &Connection,
    run_id: &str,
    issue_id: &str,
) -> Result<Option<DecisionSummary>> {
    connection.query_row("SELECT decision_id,disposition,practitioner_id,committed_at,reason,correction_overlay_id,reevaluation_id,reevaluation_state,generation,lifecycle_outcome FROM issue_decisions WHERE validation_run_id=?1 AND issue_id=?2 ORDER BY generation DESC LIMIT 1",params![run_id,issue_id],|r|Ok(decision_summary_from_columns(r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional().map_err(Into::into)
}

fn latest_attempt_summary(
    connection: &Connection,
    run_id: &str,
    issue_id: &str,
    active_decision_id: Option<&str>,
) -> Result<Option<DecisionAttemptSummary>> {
    type AttemptRow = (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<AttemptRow> = connection
        .query_row(
            "SELECT a.attempt_id,a.disposition,o.overlay_id,r.reevaluation_id,
                    ae.state,r.state,ad.decision_id,ad.reevaluation_state,
                    ad.lifecycle_outcome
             FROM decision_attempts a
             JOIN decision_attempt_events ae
               ON ae.sequence=(
                  SELECT MAX(latest.sequence) FROM decision_attempt_events latest
                  WHERE latest.attempt_id=a.attempt_id
               )
             LEFT JOIN correction_overlays o ON o.decision_attempt_id=a.attempt_id
             LEFT JOIN issue_decisions ad ON ad.decision_attempt_id=a.attempt_id
             LEFT JOIN reevaluation_runs r
               ON r.reevaluation_id=(
                  SELECT latest.reevaluation_id FROM reevaluation_runs latest
                  WHERE latest.decision_attempt_id=a.attempt_id
                  ORDER BY latest.created_at DESC,latest.reevaluation_id DESC LIMIT 1
               )
             WHERE a.validation_run_id=?1 AND a.issue_id=?2
             ORDER BY a.created_at DESC,a.attempt_id DESC LIMIT 1",
            params![run_id, issue_id],
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
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let reevaluation_state = reevaluation_state_from_evidence(
        &row.1,
        row.6.as_deref(),
        row.7.as_deref(),
        row.5.as_deref(),
        &row.4,
    );
    Ok(Some(DecisionAttemptSummary {
        decision_attempt_id: row.0,
        disposition: row.1,
        correction_overlay_id: row.2,
        reevaluation_id: row.3,
        attempt_state: row.4,
        lifecycle_outcome: lifecycle_outcome_from_evidence(row.8.as_deref(), &reevaluation_state),
        integrity_status: integrity_status_for_state(&reevaluation_state).to_owned(),
        reevaluation_state,
        active: row
            .6
            .as_deref()
            .is_some_and(|decision_id| active_decision_id == Some(decision_id)),
    }))
}

fn reevaluation_state_sql() -> &'static str {
    "CASE
       WHEN a.attempt_id IS NULL THEN NULL
       WHEN ad.decision_id IS NOT NULL
         THEN COALESCE(ad.reevaluation_state,'NOT_REQUIRED')
       WHEN a.disposition <> 'CORRECTED' THEN 'NOT_REQUIRED'
       WHEN r.state IS NOT NULL THEN r.state
       WHEN ae.state = 'REEVALUATION_RUNNING' THEN 'RUNNING'
       WHEN ae.state IN ('REEVALUATION_PLANNED','OVERLAY_PERSISTED') THEN 'PLANNED'
       WHEN ae.state = 'REEVALUATION_COMPLETE' THEN 'COMPLETE'
       WHEN ae.state = 'REEVALUATION_INTERRUPTED' THEN 'INTERRUPTED'
       WHEN ae.state = 'REEVALUATION_BOUND_EXCEEDED' THEN 'BOUND_EXCEEDED'
       WHEN ae.state = 'REEVALUATION_INTEGRITY_BLOCKED' THEN 'INTEGRITY_BLOCKED'
       WHEN ae.state = 'REEVALUATION_STALE_INPUT' THEN 'STALE_INPUT'
       WHEN ae.state = 'REEVALUATION_FAILED' THEN 'FAILED'
       ELSE 'PLANNED'
     END"
}

fn reevaluation_state_from_evidence(
    disposition: &str,
    decision_id: Option<&str>,
    decision_state: Option<&str>,
    run_state: Option<&str>,
    attempt_state: &str,
) -> String {
    if decision_id.is_some() {
        return decision_state.unwrap_or("NOT_REQUIRED").to_owned();
    }
    if disposition != "CORRECTED" {
        return "NOT_REQUIRED".to_owned();
    }
    if let Some(state) = run_state {
        return state.to_owned();
    }
    match attempt_state {
        "REEVALUATION_RUNNING" => "RUNNING",
        "REEVALUATION_COMPLETE" => "COMPLETE",
        "REEVALUATION_INTERRUPTED" => "INTERRUPTED",
        "REEVALUATION_FAILED" => "FAILED",
        "REEVALUATION_BOUND_EXCEEDED" => "BOUND_EXCEEDED",
        "REEVALUATION_INTEGRITY_BLOCKED" => "INTEGRITY_BLOCKED",
        "REEVALUATION_STALE_INPUT" => "STALE_INPUT",
        _ => "PLANNED",
    }
    .to_owned()
}

fn lifecycle_outcome_from_evidence(
    decision_outcome: Option<&str>,
    reevaluation_state: &str,
) -> String {
    if let Some(outcome) = decision_outcome {
        return outcome.to_owned();
    }
    match reevaluation_state {
        "INTERRUPTED" => "INTERRUPTED",
        "INTEGRITY_BLOCKED" => "INTEGRITY_BLOCKED",
        "STALE_INPUT" => "STALE_REJECTED",
        "FAILED" | "BOUND_EXCEEDED" => "FAILED",
        _ => "PENDING_REEVALUATION",
    }
    .to_owned()
}

fn integrity_status_for_state(reevaluation_state: &str) -> &'static str {
    if reevaluation_state == "INTEGRITY_BLOCKED" {
        "INTEGRITY_BLOCKED"
    } else {
        "VERIFIED"
    }
}

fn read_eligibility(
    profile: &Value,
    mapping: &MappingDocument,
    scope: &crate::decisions::DecisionScope,
    profile_can_start_new_runs: bool,
) -> (bool, Vec<String>, CorrectionConstraints) {
    let field = profile
        .get("entities")
        .and_then(Value::as_array)
        .and_then(|entities| {
            entities.iter().find(|entity| {
                entity.get("entityId").and_then(Value::as_str)
                    == Some(scope.canonical_entity_id.as_str())
                    && entity.get("root").and_then(Value::as_bool) == Some(true)
            })
        })
        .and_then(|entity| entity.get("fields"))
        .and_then(Value::as_array)
        .and_then(|fields| {
            fields.iter().find(|field| {
                field.get("fieldId").and_then(Value::as_str)
                    == Some(scope.canonical_field_id.as_str())
            })
        });
    let count = mapping
        .bindings
        .iter()
        .filter(|b| b.target_field_id == scope.canonical_field_id)
        .count();
    let eligible = profile_can_start_new_runs
        && field.is_some_and(|f| {
            f.get("logicalType").and_then(Value::as_str) == Some("text")
                && !f.get("sensitive").and_then(Value::as_bool).unwrap_or(false)
        })
        && count == 1
        && mapping.root_entity_id == scope.canonical_entity_id;
    let required = field
        .and_then(|f| f.get("required"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let maximum = field
        .and_then(|f| f.get("maxLength"))
        .and_then(Value::as_u64);
    let operations = if eligible {
        let mut values = vec!["REPLACE_EFFECTIVE_TEXT".to_owned()];
        if !required {
            values.push("SET_EFFECTIVE_EMPTY".to_owned());
            values.push("SET_EFFECTIVE_NULL".to_owned());
        }
        values
    } else {
        Vec::new()
    };
    (
        eligible,
        operations,
        CorrectionConstraints {
            maximum_characters: maximum,
            allow_empty: eligible && !required,
            allow_null: eligible && !required,
        },
    )
}

fn operation_value(operation: &str, replacement: Option<String>) -> Result<Option<String>> {
    match operation {
        "REPLACE_EFFECTIVE_TEXT" => replacement
            .ok_or_else(|| {
                KernelError::DecisionIntegrity("replacement overlay lost its text".to_owned())
            })
            .map(Some),
        "SET_EFFECTIVE_EMPTY" => Ok(Some(String::new())),
        "SET_EFFECTIVE_NULL" => Ok(None),
        _ => Err(KernelError::DecisionIntegrity(
            "unknown correction operation".to_owned(),
        )),
    }
}

fn validate_filters(filters: &DecisionQueueFilters) -> Result<()> {
    if filters.disposition.as_deref().is_some_and(|v| {
        !matches!(
            v,
            "UNRESOLVED" | "CORRECTED" | "WAIVED" | "ACCEPTED_AS_IS" | "EXCLUDED"
        )
    }) {
        return Err(KernelError::DecisionInvalid(
            "unsupported queue disposition".to_owned(),
        ));
    }
    if filters
        .severity
        .as_deref()
        .is_some_and(|v| !matches!(v, "ERROR" | "WARNING" | "INFORMATION"))
    {
        return Err(KernelError::DecisionInvalid(
            "unsupported queue severity".to_owned(),
        ));
    }
    if filters.reevaluation_state.as_deref().is_some_and(|v| {
        !matches!(
            v,
            "NOT_REQUIRED"
                | "PLANNED"
                | "RUNNING"
                | "COMPLETE"
                | "INTERRUPTED"
                | "FAILED"
                | "BOUND_EXCEEDED"
                | "INTEGRITY_BLOCKED"
                | "STALE_INPUT"
        )
    }) {
        return Err(KernelError::DecisionInvalid(
            "unsupported re-evaluation state".to_owned(),
        ));
    }
    for value in [
        &filters.entity_id,
        &filters.field_id,
        &filters.rule_id,
        &filters.source_file,
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(KernelError::DecisionInvalid(
                "queue filter exceeds its bound".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_page(page: &PageRequest) -> Result<u32> {
    if page.page_size == 0 || page.page_size > MAXIMUM_PAGE_SIZE {
        return Err(KernelError::DecisionInvalid(
            "page size must be in 1..=200".to_owned(),
        ));
    }
    if page.cursor.as_ref().is_some_and(|v| v.len() > 8192) {
        return Err(KernelError::DecisionInvalid(
            "cursor exceeds 8192 bytes".to_owned(),
        ));
    }
    Ok(page.page_size)
}
fn page_info(size: u32, returned: usize, next_cursor: Option<String>) -> Result<PageInfo> {
    Ok(PageInfo {
        page_size: size,
        returned: u32::try_from(returned)
            .map_err(|_| KernelError::DecisionBound("page count overflow".to_owned()))?,
        next_cursor,
    })
}
fn response_hash<T: Serialize>(response: &T) -> Result<String> {
    let mut value = serde_json::to_value(response)?;
    let object = value.as_object_mut().ok_or_else(|| {
        KernelError::DecisionIntegrity("read response must be an object".to_owned())
    })?;
    object.insert("canonical_hash".to_owned(), Value::String(String::new()));
    canonical_hash(&value)
}
fn cursor_string(value: &Value, label: &str) -> Result<String> {
    value
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v[0].as_str())
        .map(str::to_owned)
        .ok_or_else(|| KernelError::DecisionInvalid(format!("invalid {label} cursor")))
}
fn cursor_u64(value: &Value, label: &str) -> Result<u64> {
    value
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v[0].as_u64())
        .ok_or_else(|| KernelError::DecisionInvalid(format!("invalid {label} cursor")))
}
fn increment(map: &mut BTreeMap<String, u64>, key: String) -> Result<()> {
    let value = map.entry(key).or_default();
    *value = value
        .checked_add(1)
        .ok_or_else(|| KernelError::DecisionBound("count overflow".to_owned()))?;
    Ok(())
}
fn count_group(map: BTreeMap<String, u64>) -> CountGroup {
    let truncated = map.len() > MAXIMUM_GROUP_ITEMS;
    CountGroup {
        items: map
            .into_iter()
            .take(MAXIMUM_GROUP_ITEMS)
            .map(|(key, count)| CountItem { key, count })
            .collect(),
        truncated,
    }
}
fn nonnegative_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| KernelError::DecisionIntegrity(format!("{label} is negative")))
}
fn sqlite_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::DecisionBound(format!("{label} does not fit SQLite INTEGER")))
}

#[cfg(test)]
mod tests {
    use super::{lifecycle_outcome_from_evidence, reevaluation_state_from_evidence};

    #[test]
    fn incomplete_and_nonqualifying_attempt_states_never_derive_effective_authority() {
        for (event, run, expected_state, expected_outcome) in [
            (
                "REEVALUATION_RUNNING",
                None,
                "RUNNING",
                "PENDING_REEVALUATION",
            ),
            ("REEVALUATION_RUNNING", Some("FAILED"), "FAILED", "FAILED"),
            (
                "REEVALUATION_RUNNING",
                Some("BOUND_EXCEEDED"),
                "BOUND_EXCEEDED",
                "FAILED",
            ),
            (
                "REEVALUATION_INTERRUPTED",
                Some("INTERRUPTED"),
                "INTERRUPTED",
                "INTERRUPTED",
            ),
            (
                "REEVALUATION_COMPLETE",
                Some("COMPLETE"),
                "COMPLETE",
                "PENDING_REEVALUATION",
            ),
        ] {
            let state = reevaluation_state_from_evidence("CORRECTED", None, None, run, event);
            assert_eq!(state, expected_state);
            assert_eq!(
                lifecycle_outcome_from_evidence(None, &state),
                expected_outcome
            );
        }

        let completed = reevaluation_state_from_evidence(
            "CORRECTED",
            Some("decision-1"),
            Some("COMPLETE"),
            Some("COMPLETE"),
            "EFFECTIVE",
        );
        assert_eq!(completed, "COMPLETE");
        assert_eq!(
            lifecycle_outcome_from_evidence(Some("EFFECTIVE"), &completed),
            "EFFECTIVE"
        );
    }
}

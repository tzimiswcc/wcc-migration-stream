//! Bounded immutable replay for WP-005 decision and correction evidence.
//!
//! This verifier is read-only. It derives authority from canonical bytes,
//! append-only chains, exact inherited evidence, and qualifying effectivations;
//! merely finding a persisted overlay or re-evaluation never grants authority.

use std::collections::{HashMap, HashSet};

use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json},
    decisions::{
        CorrectionLogicalType, CorrectionOverlay, CorrectionOverlayRequest, DecisionScope,
        IssueDecision, IssueDecisionRequest, IssueDisposition, TextualCorrectionEligibility,
        canonical_decision_material, canonical_decision_request_hash,
        canonical_decision_request_material, canonical_overlay_hash, canonical_overlay_material,
    },
    effective_values::{
        effectivation_material, effective_lineage, effective_value_hash, original_value_hash,
        value_kind,
    },
    mapping::{MappingDocument, load_stored_mapping, project_record},
    profile_registry::load_verified_profile_document,
    validation::{
        EffectiveOverlayInput, IssueEvidence, ValidationControl, ValidationPolicy,
        evaluate_effective_overlay_before_commit, verify_validation_issue_page_for_read,
        verify_validation_run_for_read,
    },
};

const REQUIRED_SCHEMA_VERSION: i64 = crate::state::SCHEMA_VERSION;
const MAXIMUM_REPLAY_ROWS_PER_TABLE: u64 = 1_000_000;
const MAXIMUM_REPLAY_EVIDENCE_BYTES: u64 = 512 * 1024 * 1024;
const NON_CORRECTION_WRITE_RESERVATION_BYTES: u64 = 16 * 1024 * 1024;
const CORRECTION_WRITE_RESERVATION_BYTES: u64 = 224 * 1024 * 1024;
const DECISION_EVENT_CONTRACT: &str = "migration-stream-decision-attempt-event/v1";
const REEVALUATION_EVENT_CONTRACT: &str = "migration-stream-reevaluation-event/v1";
const REEVALUATION_SCOPE_CONTRACT: &str = "migration-stream-reevaluation-scope/v1";
const REEVALUATION_IDENTITY_CONTRACT: &str = "migration-stream-reevaluation-identity/v1";
const REEVALUATION_ISSUE_CONTRACT: &str = "migration-stream-reevaluation-issue/v1";
const REEVALUATION_RESULT_CONTRACT: &str = "migration-stream-reevaluation-result/v1";
const REEVALUATION_TERMINAL_CONTRACT: &str = "migration-stream-reevaluation-terminal/v1";
const VALIDATION_ISSUE_CONTRACT: &str = "migration-stream-validation-issue/v1";

const IMMUTABLE_TABLES: [(&str, &str); 8] = [
    ("decision_attempts", "decision attempt history is immutable"),
    (
        "decision_attempt_events",
        "decision attempt event history is immutable",
    ),
    (
        "correction_overlays",
        "correction overlay history is immutable",
    ),
    (
        "reevaluation_runs",
        "re-evaluation run history is immutable",
    ),
    (
        "reevaluation_events",
        "re-evaluation event history is immutable",
    ),
    (
        "reevaluation_issues",
        "re-evaluation issue history is immutable",
    ),
    ("issue_decisions", "issue decision history is immutable"),
    (
        "correction_effectivations",
        "correction effectivation history is immutable",
    ),
];

/// Compact counts returned only after every WP-005 invariant replays cleanly.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DecisionIntegritySummary {
    pub(crate) decision_attempts: u64,
    pub(crate) decision_attempt_events: u64,
    pub(crate) correction_overlays: u64,
    pub(crate) reevaluation_runs: u64,
    pub(crate) reevaluation_events: u64,
    pub(crate) reevaluation_issues: u64,
    pub(crate) issue_decisions: u64,
    pub(crate) correction_effectivations: u64,
    pub(crate) active_decision_heads: u64,
    pub(crate) inert_overlays: u64,
    pub(crate) incomplete_attempts: u64,
}

#[derive(Clone)]
struct AttemptEvidence {
    request: IssueDecisionRequest,
    created_at: String,
}

#[derive(Clone)]
struct AttemptEventHead {
    state: String,
    error_code: Option<String>,
}

#[derive(Clone)]
struct OverlayEvidence {
    overlay: CorrectionOverlay,
    attempt_id: String,
}

#[derive(Clone)]
struct ReevaluationRecord {
    reevaluation_id: String,
    identity_hash: String,
    attempt_id: String,
    overlay_id: String,
    job_id: String,
    source_validation_run_id: String,
    source_issue_id: String,
    dependency_scope_hash: String,
    state: String,
    result_json: Option<String>,
    result_hash: Option<String>,
    terminal_hash: Option<String>,
    issue_count: u64,
    rows_processed: u64,
    error_code: Option<String>,
    error_message: Option<String>,
    created_at: String,
    completed_at: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReevaluationIssueDigest {
    issue_id: String,
    issue_hash: String,
    outcome: String,
}

struct SourceIssueEvidence {
    severity: String,
    rule_id: String,
    issue_code: String,
    display_message: String,
    entity_id: String,
    field_id: String,
    source_file: String,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    lineage: Value,
    issue_hash: String,
}

struct EffectivationRecord {
    effectivation_id: String,
    decision_id: String,
    attempt_id: String,
    overlay_id: String,
    reevaluation_id: String,
    decision_disposition: String,
    reevaluation_state: String,
    generation: u64,
    original_value_hash: String,
    prior_effective_value_hash: String,
    effective_value_kind: String,
    effective_value: Option<String>,
    effective_value_hash: String,
    effective_lineage_json: String,
    effective_lineage_hash: String,
    effectivation_hash: String,
    effectivated_at: String,
}

#[derive(Clone)]
struct OriginalEvidence {
    lineage: Value,
    value_hash: String,
    initial_effective_hash: String,
}

struct HeadState {
    decision_id: String,
    generation: u64,
}

struct FieldHead {
    original: OriginalEvidence,
    current_effective_hash: String,
}

struct ReplayAuthority {
    source_hash: String,
    mapping: MappingDocument,
    mapping_hash: String,
    profile_document: Value,
    rules: Value,
    policy: ValidationPolicy,
}

struct PersistedReplayIssue {
    outcome: String,
    severity: String,
    rule_id: String,
    issue_code: String,
    display_message: String,
    entity_id: String,
    field_id: String,
    source_file: String,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    lineage_json: String,
}

type FieldKey = (
    String,
    String,
    String,
    u64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

/// Replay every immutable WP-005 evidence row and derived authority boundary.
///
/// # Errors
///
/// Fails closed for an altered/missing append-only guard, foreign-key break,
/// noncanonical or hash-mismatched material, broken event/decision chain,
/// cross-scope binding, invalid re-evaluation count, partial authority, or
/// effective-value/effectivation mismatch.
#[allow(clippy::too_many_lines)]
pub(crate) fn verify_decision_integrity_connection(
    connection: &Connection,
) -> Result<DecisionIntegritySummary> {
    verify_database_preflight(connection)?;
    let mut summary = load_bounded_counts(connection)?;
    let attempts = verify_attempts(connection)?;
    let attempt_event_heads = verify_attempt_events(connection, &attempts)?;
    let overlays = verify_overlays(connection, &attempts)?;
    let reevaluations = verify_reevaluations(connection, &attempts, &overlays)?;
    let reevaluation_event_heads = verify_reevaluation_events(connection, &reevaluations)?;
    let reevaluation_issues = verify_reevaluation_issues(connection, &reevaluations)?;
    verify_reevaluation_results(
        &reevaluations,
        &reevaluation_event_heads,
        &reevaluation_issues,
    )?;
    let effectivations = load_effectivations(connection)?;
    let DecisionReplay {
        decisions,
        corrected_decisions,
        active_heads,
        consumed_effectivations,
        consumed_reevaluations,
        consumed_overlays,
    } = verify_decisions_and_effectivations(
        connection,
        &attempts,
        &attempt_event_heads,
        &overlays,
        &reevaluations,
        &effectivations,
    )?;

    for (attempt_id, head) in &attempt_event_heads {
        if head.state == "EFFECTIVE" && !decisions.contains(attempt_id) {
            return integrity(format!(
                "decision attempt {attempt_id} claims EFFECTIVE without an immutable decision"
            ));
        }
    }
    verify_attempt_reevaluation_bindings(
        &attempts,
        &attempt_event_heads,
        &overlays,
        &reevaluations,
    )?;
    for (reevaluation_id, run) in &reevaluations {
        if run.state == "COMPLETE" && !consumed_reevaluations.contains(reevaluation_id) {
            return integrity(format!(
                "COMPLETE re-evaluation {reevaluation_id} is not atomically bound to effective authority"
            ));
        }
        if run.state != "COMPLETE" && consumed_reevaluations.contains(reevaluation_id) {
            return integrity(format!(
                "nonqualifying re-evaluation {reevaluation_id} granted decision authority"
            ));
        }
    }
    if consumed_effectivations.len() != effectivations.len()
        || corrected_decisions != effectivations.len()
    {
        return integrity(
            "corrected decisions and consumed immutable effectivations do not reconcile",
        );
    }
    if consumed_overlays.len() > overlays.len() {
        return integrity("consumed overlay count exceeds immutable overlay evidence");
    }

    summary.active_decision_heads = to_u64(active_heads, "active decision heads")?;
    summary.inert_overlays = to_u64(
        overlays.len().saturating_sub(consumed_overlays.len()),
        "inert overlays",
    )?;
    summary.incomplete_attempts = to_u64(
        attempts.len().saturating_sub(decisions.len()),
        "incomplete attempts",
    )?;
    if summary.decision_attempts != to_u64(attempts.len(), "decision attempts")?
        || summary.correction_overlays != to_u64(overlays.len(), "correction overlays")?
        || summary.reevaluation_runs != to_u64(reevaluations.len(), "re-evaluation runs")?
        || summary.issue_decisions != to_u64(decisions.len(), "issue decisions")?
        || summary.correction_effectivations
            != to_u64(effectivations.len(), "correction effectivations")?
    {
        return integrity("replayed WP-005 row counts differ from durable table counts");
    }
    Ok(summary)
}

fn verify_database_preflight(connection: &Connection) -> Result<()> {
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != REQUIRED_SCHEMA_VERSION {
        return integrity(format!(
            "decision replay requires schema version {REQUIRED_SCHEMA_VERSION}; found {schema_version}"
        ));
    }
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return integrity(format!("SQLite quick-check failed: {quick_check}"));
    }
    let foreign_key_break: Option<(String, Option<i64>, String, i64)> = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    if let Some((table, rowid, parent, foreign_key)) = foreign_key_break {
        return integrity(format!(
            "foreign-key replay failed at {table} row {rowid:?} -> {parent} constraint {foreign_key}"
        ));
    }
    verify_append_only_guards(connection)
}

fn verify_append_only_guards(connection: &Connection) -> Result<()> {
    for (table, message) in IMMUTABLE_TABLES {
        for operation in ["update", "delete"] {
            let name = format!("{table}_no_{operation}");
            let verb = operation.to_ascii_uppercase();
            let expected = normalized_schema_sql(&format!(
                "CREATE TRIGGER {name} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, '{message}'); END"
            ));
            let sql: Option<String> = connection
                .query_row(
                    "SELECT sql FROM sqlite_schema
                     WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                    params![name, table],
                    |row| row.get(0),
                )
                .optional()?;
            if sql.as_deref().map(normalized_schema_sql).as_deref() != Some(expected.as_str()) {
                return integrity(format!("append-only guard {name} is missing or altered"));
            }
        }
    }
    Ok(())
}

fn load_bounded_counts(connection: &Connection) -> Result<DecisionIntegritySummary> {
    let summary = DecisionIntegritySummary {
        decision_attempts: bounded_table_count(connection, "decision_attempts")?,
        decision_attempt_events: bounded_table_count(connection, "decision_attempt_events")?,
        correction_overlays: bounded_table_count(connection, "correction_overlays")?,
        reevaluation_runs: bounded_table_count(connection, "reevaluation_runs")?,
        reevaluation_events: bounded_table_count(connection, "reevaluation_events")?,
        reevaluation_issues: bounded_table_count(connection, "reevaluation_issues")?,
        issue_decisions: bounded_table_count(connection, "issue_decisions")?,
        correction_effectivations: bounded_table_count(connection, "correction_effectivations")?,
        ..DecisionIntegritySummary::default()
    };
    let evidence_bytes = current_replay_evidence_bytes(connection)?;
    if evidence_bytes > MAXIMUM_REPLAY_EVIDENCE_BYTES {
        return integrity(format!(
            "WP-005 immutable evidence is {evidence_bytes} bytes; replay permits {MAXIMUM_REPLAY_EVIDENCE_BYTES}"
        ));
    }
    Ok(summary)
}

fn bounded_table_count(connection: &Connection, table: &str) -> Result<u64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    let count = nonnegative(count, &format!("{table} row count"))?;
    if count > MAXIMUM_REPLAY_ROWS_PER_TABLE {
        return integrity(format!(
            "{table} has {count} rows; immutable replay permits {MAXIMUM_REPLAY_ROWS_PER_TABLE}"
        ));
    }
    Ok(count)
}

#[allow(clippy::too_many_lines)]
fn current_replay_evidence_bytes(connection: &Connection) -> Result<u64> {
    let queries = [
        "SELECT COALESCE(SUM(
            length(CAST(attempt_id AS BLOB))
          + length(CAST(idempotency_key AS BLOB))
          + length(CAST(request_hash AS BLOB))
          + length(CAST(canonical_request_json AS BLOB))
          + length(CAST(job_id AS BLOB))
          + length(CAST(validation_run_id AS BLOB))
          + length(CAST(issue_id AS BLOB))
          + length(CAST(issue_hash AS BLOB))
          + length(CAST(accepted_source_file AS BLOB))
          + length(CAST(canonical_entity_id AS BLOB))
          + length(CAST(canonical_field_id AS BLOB))
          + length(CAST(profile_id AS BLOB))
          + length(CAST(profile_version AS BLOB))
          + length(CAST(profile_hash AS BLOB))
          + length(CAST(mapping_id AS BLOB))
          + length(CAST(mapping_version AS BLOB))
          + length(CAST(mapping_hash AS BLOB))
          + length(CAST(disposition AS BLOB))
          + length(CAST(practitioner_id AS BLOB))
          + length(CAST(reason AS BLOB))
          + COALESCE(length(CAST(evidence_reference AS BLOB)), 0)
          + COALESCE(length(CAST(expected_active_decision_id AS BLOB)), 0)
          + COALESCE(length(CAST(expected_effective_value_hash AS BLOB)), 0)
          + length(CAST(created_at AS BLOB))
        ), 0) FROM decision_attempts",
        "SELECT COALESCE(SUM(
            length(CAST(attempt_id AS BLOB)) + length(CAST(state AS BLOB))
          + COALESCE(length(CAST(error_code AS BLOB)), 0)
          + COALESCE(length(CAST(detail AS BLOB)), 0)
          + COALESCE(length(CAST(previous_event_hash AS BLOB)), 0)
          + length(CAST(event_hash AS BLOB)) + length(CAST(occurred_at AS BLOB))
        ), 0) FROM decision_attempt_events",
        "SELECT COALESCE(SUM(
            length(CAST(overlay_id AS BLOB))
          + length(CAST(decision_attempt_id AS BLOB))
          + length(CAST(decision_request_hash AS BLOB))
          + length(CAST(disposition AS BLOB))
          + length(CAST(prior_effective_value_hash AS BLOB))
          + length(CAST(operation AS BLOB))
          + COALESCE(length(CAST(replacement_text AS BLOB)), 0)
          + length(CAST(practitioner_id AS BLOB)) + length(CAST(reason AS BLOB))
          + length(CAST(canonical_overlay_json AS BLOB))
          + length(CAST(overlay_hash AS BLOB)) + length(CAST(created_at AS BLOB))
        ), 0) FROM correction_overlays",
        "SELECT COALESCE(SUM(
            length(CAST(reevaluation_id AS BLOB)) + length(CAST(identity_hash AS BLOB))
          + length(CAST(decision_attempt_id AS BLOB))
          + length(CAST(correction_overlay_id AS BLOB))
          + length(CAST(disposition AS BLOB)) + length(CAST(job_id AS BLOB))
          + length(CAST(source_validation_run_id AS BLOB))
          + length(CAST(source_issue_id AS BLOB)) + length(CAST(rule_set_hash AS BLOB))
          + length(CAST(engine_version AS BLOB)) + length(CAST(policy_json AS BLOB))
          + length(CAST(policy_hash AS BLOB)) + length(CAST(dependency_scope_json AS BLOB))
          + length(CAST(dependency_scope_hash AS BLOB)) + length(CAST(state AS BLOB))
          + COALESCE(length(CAST(result_json AS BLOB)), 0)
          + COALESCE(length(CAST(result_hash AS BLOB)), 0)
          + COALESCE(length(CAST(terminal_hash AS BLOB)), 0)
          + COALESCE(length(CAST(error_code AS BLOB)), 0)
          + COALESCE(length(CAST(error_message AS BLOB)), 0)
          + length(CAST(created_at AS BLOB))
          + COALESCE(length(CAST(completed_at AS BLOB)), 0)
        ), 0) FROM reevaluation_runs",
        "SELECT COALESCE(SUM(
            length(CAST(reevaluation_id AS BLOB)) + length(CAST(state AS BLOB))
          + COALESCE(length(CAST(error_code AS BLOB)), 0)
          + COALESCE(length(CAST(detail AS BLOB)), 0)
          + COALESCE(length(CAST(previous_event_hash AS BLOB)), 0)
          + length(CAST(event_hash AS BLOB)) + length(CAST(occurred_at AS BLOB))
        ), 0) FROM reevaluation_events",
        "SELECT COALESCE(SUM(
            length(CAST(reevaluation_id AS BLOB)) + length(CAST(issue_id AS BLOB))
          + length(CAST(outcome AS BLOB))
          + COALESCE(length(CAST(source_validation_run_id AS BLOB)), 0)
          + COALESCE(length(CAST(source_issue_id AS BLOB)), 0)
          + length(CAST(severity AS BLOB)) + length(CAST(rule_id AS BLOB))
          + length(CAST(issue_code AS BLOB)) + length(CAST(display_message AS BLOB))
          + length(CAST(entity_id AS BLOB)) + length(CAST(field_id AS BLOB))
          + length(CAST(source_file AS BLOB)) + length(CAST(lineage_json AS BLOB))
          + length(CAST(issue_hash AS BLOB))
        ), 0) FROM reevaluation_issues",
        "SELECT COALESCE(SUM(
            length(CAST(decision_id AS BLOB)) + length(CAST(decision_attempt_id AS BLOB))
          + length(CAST(job_id AS BLOB)) + length(CAST(validation_run_id AS BLOB))
          + length(CAST(issue_id AS BLOB)) + length(CAST(disposition AS BLOB))
          + COALESCE(length(CAST(predecessor_decision_id AS BLOB)), 0)
          + COALESCE(length(CAST(correction_overlay_id AS BLOB)), 0)
          + COALESCE(length(CAST(reevaluation_id AS BLOB)), 0)
          + COALESCE(length(CAST(reevaluation_state AS BLOB)), 0)
          + length(CAST(practitioner_id AS BLOB)) + length(CAST(reason AS BLOB))
          + COALESCE(length(CAST(evidence_reference AS BLOB)), 0)
          + length(CAST(lifecycle_outcome AS BLOB))
          + length(CAST(canonical_decision_json AS BLOB))
          + length(CAST(decision_hash AS BLOB)) + length(CAST(committed_at AS BLOB))
        ), 0) FROM issue_decisions",
        "SELECT COALESCE(SUM(
            length(CAST(effectivation_id AS BLOB)) + length(CAST(decision_id AS BLOB))
          + length(CAST(decision_attempt_id AS BLOB))
          + length(CAST(correction_overlay_id AS BLOB))
          + length(CAST(reevaluation_id AS BLOB))
          + length(CAST(decision_disposition AS BLOB))
          + length(CAST(reevaluation_state AS BLOB))
          + length(CAST(original_value_hash AS BLOB))
          + length(CAST(prior_effective_value_hash AS BLOB))
          + length(CAST(effective_value_kind AS BLOB))
          + COALESCE(length(CAST(effective_value_text AS BLOB)), 0)
          + length(CAST(effective_value_hash AS BLOB))
          + length(CAST(effective_lineage_json AS BLOB))
          + length(CAST(effective_lineage_hash AS BLOB))
          + length(CAST(effectivation_hash AS BLOB))
          + length(CAST(effectivated_at AS BLOB))
        ), 0) FROM correction_effectivations",
    ];
    let mut total = 0_u64;
    for query in queries {
        let bytes: i64 = connection.query_row(query, [], |row| row.get(0))?;
        total = total
            .checked_add(nonnegative(bytes, "WP-005 evidence byte count")?)
            .ok_or_else(|| integrity_error("WP-005 immutable evidence byte count overflowed"))?;
    }
    Ok(total)
}

pub(crate) fn ensure_non_correction_write_capacity(connection: &Connection) -> Result<()> {
    ensure_write_capacity(
        connection,
        &[
            ("decision_attempts", 1),
            ("decision_attempt_events", 3),
            ("issue_decisions", 1),
        ],
        NON_CORRECTION_WRITE_RESERVATION_BYTES,
    )
}

pub(crate) fn ensure_correction_write_capacity(
    connection: &Connection,
    maximum_reevaluation_issues: u64,
) -> Result<()> {
    ensure_write_capacity(
        connection,
        &[
            ("decision_attempts", 1),
            ("decision_attempt_events", 7),
            ("correction_overlays", 1),
            ("reevaluation_runs", 1),
            ("reevaluation_events", 3),
            ("reevaluation_issues", maximum_reevaluation_issues),
            ("issue_decisions", 1),
            ("correction_effectivations", 1),
        ],
        CORRECTION_WRITE_RESERVATION_BYTES,
    )
}

fn ensure_write_capacity(
    connection: &Connection,
    reservations: &[(&str, u64)],
    reserved_bytes: u64,
) -> Result<()> {
    for (table, reserved_rows) in reservations {
        let current = bounded_table_count(connection, table)?;
        if current
            .checked_add(*reserved_rows)
            .is_none_or(|required| required > MAXIMUM_REPLAY_ROWS_PER_TABLE)
        {
            return Err(KernelError::DecisionBound(format!(
                "{table} lacks immutable replay row capacity for this command"
            )));
        }
    }
    let current_bytes = current_replay_evidence_bytes(connection)?;
    if current_bytes
        .checked_add(reserved_bytes)
        .is_none_or(|required| required > MAXIMUM_REPLAY_EVIDENCE_BYTES)
    {
        return Err(KernelError::DecisionBound(
            "WP-005 immutable evidence lacks byte capacity for this command".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_attempts(connection: &Connection) -> Result<HashMap<String, AttemptEvidence>> {
    let mut statement = connection.prepare(
        "SELECT attempt_id, schema_version, idempotency_key, request_hash,
                canonical_request_json, job_id, validation_run_id, issue_id,
                issue_hash, accepted_source_file, source_ordinal, line_start, line_end,
                canonical_entity_id, canonical_field_id,
                profile_id, profile_version, profile_hash,
                mapping_id, mapping_version, mapping_hash, disposition,
                practitioner_id, reason, evidence_reference,
                authority_separation_acknowledged,
                row_scope_exclusion_acknowledged,
                expected_active_decision_id, expected_active_generation,
                expected_effective_value_hash, created_at
         FROM decision_attempts ORDER BY attempt_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, String>(16)?,
            row.get::<_, String>(17)?,
            row.get::<_, String>(18)?,
            row.get::<_, String>(19)?,
            row.get::<_, String>(20)?,
            row.get::<_, String>(21)?,
            row.get::<_, String>(22)?,
            row.get::<_, String>(23)?,
            row.get::<_, Option<String>>(24)?,
            row.get::<_, i64>(25)?,
            row.get::<_, i64>(26)?,
            row.get::<_, Option<String>>(27)?,
            row.get::<_, i64>(28)?,
            row.get::<_, Option<String>>(29)?,
            row.get::<_, String>(30)?,
        ))
    })?;
    let mut attempts = HashMap::new();
    let mut checked_runs = HashSet::new();
    for row in rows {
        let (
            attempt_id,
            schema_version,
            idempotency_key,
            request_hash,
            request_json,
            job_id,
            validation_run_id,
            issue_id,
            issue_hash,
            source_file,
            source_ordinal,
            line_start,
            line_end,
            entity_id,
            field_id,
            profile_id,
            profile_version,
            profile_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
            disposition,
            practitioner_id,
            reason,
            evidence_reference,
            authority_acknowledged,
            row_acknowledged,
            expected_decision_id,
            expected_generation,
            expected_effective_hash,
            created_at,
        ) = row?;
        let material = parse_canonical_json(&request_json, "decision request material")?;
        let request_value = material
            .get("request")
            .cloned()
            .ok_or_else(|| integrity_error("decision request material lacks request"))?;
        let request: IssueDecisionRequest =
            serde_json::from_value(request_value).map_err(|error| {
                integrity_error(format!(
                    "decision attempt {attempt_id} contract is invalid: {error}"
                ))
            })?;
        request.validate().map_err(|error| {
            integrity_error(format!("decision attempt {attempt_id} is invalid: {error}"))
        })?;
        if canonical_decision_request_material(&request)? != material
            || canonical_decision_request_hash(&request)? != request_hash
        {
            return integrity(format!(
                "decision attempt {attempt_id} canonical request bytes/hash mismatch"
            ));
        }
        let sql_scope = DecisionScope {
            job_id,
            validation_run_id: validation_run_id.clone(),
            issue_id: issue_id.clone(),
            issue_hash,
            accepted_source_file: source_file,
            source_ordinal: nonnegative(source_ordinal, "attempt source ordinal")?,
            line_start: nonnegative(line_start, "attempt line start")?,
            line_end: nonnegative(line_end, "attempt line end")?,
            canonical_entity_id: entity_id,
            canonical_field_id: field_id,
            profile_id,
            profile_version,
            profile_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
        };
        if schema_version != i64::from(request.schema_version)
            || idempotency_key != request.idempotency_key
            || sql_scope != request.scope
            || disposition != request.disposition.as_str()
            || practitioner_id != request.practitioner_id
            || reason != request.reason
            || evidence_reference != request.evidence_reference
            || (authority_acknowledged == 1) != request.authority_separation_acknowledged
            || (row_acknowledged == 1) != request.row_scope_exclusion_acknowledged
            || expected_decision_id != request.concurrency.expected_active_decision_id
            || nonnegative(expected_generation, "expected active generation")?
                != request.concurrency.expected_active_decision_generation
            || expected_effective_hash != request.concurrency.expected_effective_value_hash
        {
            return integrity(format!(
                "decision attempt {attempt_id} columns differ from canonical request evidence"
            ));
        }
        require_timestamp(&created_at, "decision attempt timestamp")?;
        if checked_runs.insert(validation_run_id.clone()) {
            verify_validation_run_for_read(connection, &validation_run_id).map_err(|error| {
                integrity_error(format!(
                    "decision attempt {attempt_id} source validation run replay failed: {error}"
                ))
            })?;
        }
        verify_validation_issue_page_for_read(connection, &validation_run_id, &[issue_id])
            .map_err(|error| {
                integrity_error(format!(
                    "decision attempt {attempt_id} source issue replay failed: {error}"
                ))
            })?;
        if attempts
            .insert(
                attempt_id.clone(),
                AttemptEvidence {
                    request,
                    created_at,
                },
            )
            .is_some()
        {
            return integrity(format!("duplicate decision attempt {attempt_id}"));
        }
    }
    Ok(attempts)
}

fn verify_attempt_events(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
) -> Result<HashMap<String, AttemptEventHead>> {
    let mut statement = connection.prepare(
        "SELECT attempt_id, state, error_code, detail,
                previous_event_hash, event_hash, occurred_at
         FROM decision_attempt_events ORDER BY attempt_id, sequence",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut previous = HashMap::<String, String>::new();
    let mut heads = HashMap::new();
    let mut sequences = HashMap::<String, Vec<String>>::new();
    let mut timestamps = HashMap::new();
    for row in rows {
        let (attempt_id, state, error_code, detail, stored_previous, event_hash, occurred_at) =
            row?;
        if !attempts.contains_key(&attempt_id) {
            return integrity(format!(
                "decision event refers to missing attempt {attempt_id}"
            ));
        }
        let expected_previous = previous.get(&attempt_id).cloned();
        if stored_previous != expected_previous {
            return integrity(format!(
                "decision attempt {attempt_id} event hash chain is broken"
            ));
        }
        require_timestamp(&occurred_at, "decision attempt event timestamp")?;
        let parsed_timestamp = DateTime::parse_from_rfc3339(&occurred_at).map_err(|error| {
            integrity_error(format!(
                "decision attempt {attempt_id} event timestamp is invalid: {error}"
            ))
        })?;
        if timestamps
            .insert(attempt_id.clone(), parsed_timestamp)
            .is_some_and(|previous| previous > parsed_timestamp)
        {
            return integrity(format!(
                "decision attempt {attempt_id} event timestamps move backwards"
            ));
        }
        let expected_hash = decision_attempt_event_hash(
            &attempt_id,
            &state,
            error_code.as_deref(),
            detail.as_deref(),
            stored_previous.as_deref(),
            &occurred_at,
        )?;
        if event_hash != expected_hash {
            return integrity(format!("decision attempt {attempt_id} event hash mismatch"));
        }
        let diagnostic_terminal = matches!(
            state.as_str(),
            "REJECTED"
                | "CONFLICT"
                | "REEVALUATION_INTERRUPTED"
                | "REEVALUATION_FAILED"
                | "REEVALUATION_BOUND_EXCEEDED"
                | "REEVALUATION_INTEGRITY_BLOCKED"
                | "REEVALUATION_STALE_INPUT"
        );
        if (diagnostic_terminal && (error_code.is_none() || detail.is_none()))
            || (!diagnostic_terminal && (error_code.is_some() || detail.is_some()))
        {
            return integrity(format!(
                "decision attempt {attempt_id} event diagnostics do not match state {state}"
            ));
        }
        previous.insert(attempt_id.clone(), event_hash);
        sequences
            .entry(attempt_id.clone())
            .or_default()
            .push(state.clone());
        heads.insert(attempt_id, AttemptEventHead { state, error_code });
    }
    for (attempt_id, attempt) in attempts {
        let sequence = sequences.get(attempt_id).ok_or_else(|| {
            integrity_error(format!(
                "decision attempt {attempt_id} has no immutable lifecycle event"
            ))
        })?;
        if !valid_attempt_event_sequence(attempt.request.disposition, sequence) {
            return integrity(format!(
                "decision attempt {attempt_id} has an invalid lifecycle transition sequence"
            ));
        }
    }
    Ok(heads)
}

fn valid_attempt_event_sequence(disposition: IssueDisposition, sequence: &[String]) -> bool {
    let states = sequence.iter().map(String::as_str).collect::<Vec<_>>();
    if states == ["RECEIVED", "REJECTED"] || states == ["RECEIVED", "VALIDATED", "CONFLICT"] {
        return true;
    }
    if disposition != IssueDisposition::Corrected {
        return states == ["RECEIVED", "VALIDATED", "EFFECTIVE"];
    }
    let running = [
        "RECEIVED",
        "VALIDATED",
        "OVERLAY_PERSISTED",
        "REEVALUATION_PLANNED",
        "REEVALUATION_RUNNING",
    ];
    if states == running {
        return true;
    }
    let Some((&terminal, prefix)) = states.split_last() else {
        return false;
    };
    prefix == running
        && matches!(
            terminal,
            "REEVALUATION_INTERRUPTED"
                | "REEVALUATION_FAILED"
                | "REEVALUATION_BOUND_EXCEEDED"
                | "REEVALUATION_INTEGRITY_BLOCKED"
                | "REEVALUATION_STALE_INPUT"
        )
        || states
            == [
                "RECEIVED",
                "VALIDATED",
                "OVERLAY_PERSISTED",
                "REEVALUATION_PLANNED",
                "REEVALUATION_RUNNING",
                "REEVALUATION_COMPLETE",
                "EFFECTIVE",
            ]
}

fn verify_attempt_reevaluation_bindings(
    attempts: &HashMap<String, AttemptEvidence>,
    attempt_heads: &HashMap<String, AttemptEventHead>,
    overlays: &HashMap<String, OverlayEvidence>,
    reevaluations: &HashMap<String, ReevaluationRecord>,
) -> Result<()> {
    let mut overlays_by_attempt = HashMap::new();
    for (overlay_id, overlay) in overlays {
        if overlays_by_attempt
            .insert(overlay.attempt_id.clone(), overlay_id)
            .is_some()
        {
            return integrity(format!(
                "decision attempt {} has multiple immutable overlays",
                overlay.attempt_id
            ));
        }
    }
    let mut runs_by_attempt = HashMap::new();
    for (reevaluation_id, run) in reevaluations {
        let expected_attempt_state = match run.state.as_str() {
            "PLANNED" => "REEVALUATION_PLANNED",
            "RUNNING" => "REEVALUATION_RUNNING",
            "COMPLETE" => "EFFECTIVE",
            "INTERRUPTED" => "REEVALUATION_INTERRUPTED",
            "FAILED" => "REEVALUATION_FAILED",
            "BOUND_EXCEEDED" => "REEVALUATION_BOUND_EXCEEDED",
            "INTEGRITY_BLOCKED" => "REEVALUATION_INTEGRITY_BLOCKED",
            "STALE_INPUT" => "REEVALUATION_STALE_INPUT",
            _ => {
                return integrity(format!(
                    "re-evaluation {reevaluation_id} has an unknown binding state"
                ));
            }
        };
        let head = attempt_heads.get(&run.attempt_id).ok_or_else(|| {
            integrity_error(format!(
                "re-evaluation {reevaluation_id} has no decision-attempt event head"
            ))
        })?;
        if head.state != expected_attempt_state
            || (run.state == "COMPLETE" && head.error_code.is_some())
            || (run.state != "COMPLETE" && head.error_code != run.error_code)
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} does not match its attempt terminal state/error"
            ));
        }
        if runs_by_attempt
            .insert(run.attempt_id.clone(), reevaluation_id)
            .is_some()
        {
            return integrity(format!(
                "decision attempt {} has multiple immutable re-evaluations",
                run.attempt_id
            ));
        }
    }
    for (attempt_id, head) in attempt_heads {
        let attempt = attempts.get(attempt_id).ok_or_else(|| {
            integrity_error(format!("event head refers to missing attempt {attempt_id}"))
        })?;
        let progressed_correction = matches!(
            head.state.as_str(),
            "REEVALUATION_RUNNING"
                | "REEVALUATION_INTERRUPTED"
                | "REEVALUATION_FAILED"
                | "REEVALUATION_BOUND_EXCEEDED"
                | "REEVALUATION_INTEGRITY_BLOCKED"
                | "REEVALUATION_STALE_INPUT"
                | "EFFECTIVE"
        ) && attempt.request.disposition == IssueDisposition::Corrected;
        if progressed_correction != overlays_by_attempt.contains_key(attempt_id) {
            return integrity(format!(
                "decision attempt {attempt_id} overlay evidence does not match its lifecycle"
            ));
        }
        if matches!(
            head.state.as_str(),
            "REEVALUATION_INTERRUPTED"
                | "REEVALUATION_FAILED"
                | "REEVALUATION_BOUND_EXCEEDED"
                | "REEVALUATION_INTEGRITY_BLOCKED"
                | "REEVALUATION_STALE_INPUT"
        ) && !runs_by_attempt.contains_key(attempt_id)
        {
            return integrity(format!(
                "decision attempt {attempt_id} has a terminal re-evaluation state without a run"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_overlays(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
) -> Result<HashMap<String, OverlayEvidence>> {
    let mut statement = connection.prepare(
        "SELECT overlay_id, schema_version, decision_attempt_id,
                decision_request_hash, disposition, prior_effective_value_hash,
                operation, replacement_text, practitioner_id, reason,
                canonical_overlay_json, overlay_hash, created_at
         FROM correction_overlays ORDER BY overlay_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
        ))
    })?;
    let mut overlays = HashMap::new();
    for row in rows {
        let (
            overlay_id,
            schema_version,
            attempt_id,
            request_hash,
            disposition,
            prior_hash,
            operation,
            replacement_text,
            practitioner_id,
            reason,
            overlay_json,
            overlay_hash,
            created_at,
        ) = row?;
        let attempt = attempts.get(&attempt_id).ok_or_else(|| {
            integrity_error(format!("overlay {overlay_id} has no decision attempt"))
        })?;
        let material = parse_canonical_json(&overlay_json, "correction overlay material")?;
        let mut record = material
            .get("record")
            .cloned()
            .ok_or_else(|| integrity_error("correction overlay material lacks record"))?;
        record
            .as_object_mut()
            .ok_or_else(|| integrity_error("correction overlay record is not an object"))?
            .insert(
                "overlayHash".to_owned(),
                Value::String(overlay_hash.clone()),
            );
        let overlay: CorrectionOverlay = serde_json::from_value(record).map_err(|error| {
            integrity_error(format!("overlay {overlay_id} contract is invalid: {error}"))
        })?;
        overlay.scope.validate().map_err(|error| {
            integrity_error(format!("overlay {overlay_id} scope is invalid: {error}"))
        })?;
        CorrectionOverlayRequest {
            operation: overlay.operation,
            replacement_text: overlay.replacement_text.clone(),
        }
        .validate()
        .map_err(|error| {
            integrity_error(format!("overlay {overlay_id} value is invalid: {error}"))
        })?;
        let expected_storage_value = match overlay.operation {
            crate::decisions::CorrectionOperation::ReplaceEffectiveText => {
                overlay.replacement_text.clone()
            }
            crate::decisions::CorrectionOperation::SetEffectiveEmpty => Some(String::new()),
            crate::decisions::CorrectionOperation::SetEffectiveNull => None,
        };
        if canonical_overlay_material(&overlay)? != material
            || canonical_overlay_hash(&overlay)? != overlay_hash
            || schema_version != i64::from(overlay.schema_version)
            || overlay_id != overlay.overlay_id
            || request_hash != overlay.decision_request_hash
            || prior_hash != overlay.prior_effective_value_hash
            || operation != overlay.operation.as_str()
            || replacement_text != expected_storage_value
            || practitioner_id != overlay.practitioner_id
            || reason != overlay.reason
            || created_at != overlay.created_at
            || disposition != IssueDisposition::Corrected.as_str()
            || overlay.scope != attempt.request.scope
            || request_hash != canonical_decision_request_hash(&attempt.request)?
            || practitioner_id != attempt.request.practitioner_id
            || reason != attempt.request.reason
            || attempt.request.disposition != IssueDisposition::Corrected
            || attempt
                .request
                .concurrency
                .expected_effective_value_hash
                .as_deref()
                != Some(prior_hash.as_str())
        {
            return integrity(format!(
                "overlay {overlay_id} columns, scope, attribution, or canonical hash mismatch"
            ));
        }
        let requested_overlay = attempt.request.correction_overlay.as_ref().ok_or_else(|| {
            integrity_error(format!("overlay {overlay_id} is absent from its request"))
        })?;
        if requested_overlay.operation != overlay.operation
            || requested_overlay.replacement_text != overlay.replacement_text
        {
            return integrity(format!(
                "overlay {overlay_id} differs from its immutable decision request"
            ));
        }
        if created_at != attempt.created_at {
            return integrity(format!(
                "overlay {overlay_id} creation time differs from its immutable attempt"
            ));
        }
        require_timestamp(&created_at, "correction overlay timestamp")?;
        overlays.insert(
            overlay_id,
            OverlayEvidence {
                overlay,
                attempt_id,
            },
        );
    }
    Ok(overlays)
}

#[allow(clippy::too_many_lines)]
fn verify_reevaluations(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
    overlays: &HashMap<String, OverlayEvidence>,
) -> Result<HashMap<String, ReevaluationRecord>> {
    let mut statement = connection.prepare(
        "SELECT reevaluation_id, identity_hash, decision_attempt_id,
                correction_overlay_id, disposition, job_id,
                source_validation_run_id, source_issue_id, rule_set_hash,
                engine_version, policy_json, policy_hash,
                dependency_scope_json, dependency_scope_hash, state,
                result_json, result_hash, terminal_hash,
                issue_count, rows_processed, error_code, error_message,
                created_at, completed_at
         FROM reevaluation_runs ORDER BY reevaluation_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, i64>(18)?,
            row.get::<_, i64>(19)?,
            row.get::<_, Option<String>>(20)?,
            row.get::<_, Option<String>>(21)?,
            row.get::<_, String>(22)?,
            row.get::<_, Option<String>>(23)?,
        ))
    })?;
    let mut reevaluations = HashMap::new();
    for row in rows {
        let (
            reevaluation_id,
            identity_hash,
            attempt_id,
            overlay_id,
            disposition,
            job_id,
            source_run_id,
            source_issue_id,
            rule_set_hash,
            engine_version,
            policy_json,
            policy_hash,
            scope_json,
            scope_hash,
            state,
            result_json,
            result_hash,
            terminal_hash,
            issue_count,
            rows_processed,
            error_code,
            error_message,
            created_at,
            completed_at,
        ) = row?;
        let attempt = attempts.get(&attempt_id).ok_or_else(|| {
            integrity_error(format!("re-evaluation {reevaluation_id} has no attempt"))
        })?;
        let overlay = overlays.get(&overlay_id).ok_or_else(|| {
            integrity_error(format!("re-evaluation {reevaluation_id} has no overlay"))
        })?;
        let source_authority: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT rule_set_hash, engine_version, policy_json, policy_hash
                 FROM validation_runs WHERE id = ?1 AND job_id = ?2",
                params![source_run_id, job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let source_authority = source_authority.ok_or_else(|| {
            integrity_error(format!(
                "re-evaluation {reevaluation_id} source validation authority is missing"
            ))
        })?;
        let policy = parse_canonical_json(&policy_json, "re-evaluation policy")?;
        let scope = parse_canonical_json(&scope_json, "re-evaluation dependency scope")?;
        let expected_scope = reevaluation_scope_material(&attempt.request.scope);
        let expected_identity = reevaluation_identity_hash(
            &attempt_id,
            &overlay_id,
            &source_run_id,
            &source_issue_id,
            &rule_set_hash,
            &engine_version,
            &policy_hash,
            &scope_hash,
        )?;
        if canonical_hash(&policy)? != policy_hash
            || scope != expected_scope
            || canonical_hash(&scope)? != scope_hash
            || identity_hash != expected_identity
            || disposition != IssueDisposition::Corrected.as_str()
            || overlay.attempt_id != attempt_id
            || job_id != attempt.request.scope.job_id
            || source_run_id != attempt.request.scope.validation_run_id
            || source_issue_id != attempt.request.scope.issue_id
            || (
                rule_set_hash,
                engine_version,
                policy_json.clone(),
                policy_hash.clone(),
            ) != source_authority
            || created_at != attempt.created_at
            || created_at != overlay.overlay.created_at
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} identity, policy, scope, or source binding mismatch"
            ));
        }
        require_timestamp(&created_at, "re-evaluation created timestamp")?;
        if let Some(completed_at) = completed_at.as_deref() {
            require_timestamp(completed_at, "re-evaluation completed timestamp")?;
            let completed = DateTime::parse_from_rfc3339(completed_at).map_err(|error| {
                integrity_error(format!(
                    "re-evaluation completed timestamp is invalid: {error}"
                ))
            })?;
            let created = DateTime::parse_from_rfc3339(&created_at).map_err(|error| {
                integrity_error(format!(
                    "re-evaluation created timestamp is invalid: {error}"
                ))
            })?;
            if completed < created {
                return integrity(format!(
                    "re-evaluation {reevaluation_id} completes before creation"
                ));
            }
        }
        verify_reevaluation_state_shape(
            &reevaluation_id,
            &state,
            result_json.as_deref(),
            result_hash.as_deref(),
            terminal_hash.as_deref(),
            error_code.as_deref(),
            completed_at.as_deref(),
        )?;
        reevaluations.insert(
            reevaluation_id.clone(),
            ReevaluationRecord {
                reevaluation_id,
                identity_hash,
                attempt_id,
                overlay_id,
                job_id,
                source_validation_run_id: source_run_id,
                source_issue_id,
                dependency_scope_hash: scope_hash,
                state,
                result_json,
                result_hash,
                terminal_hash,
                issue_count: nonnegative(issue_count, "re-evaluation issue count")?,
                rows_processed: nonnegative(rows_processed, "re-evaluation row count")?,
                error_code,
                error_message,
                created_at,
                completed_at,
            },
        );
    }
    Ok(reevaluations)
}

#[allow(clippy::too_many_lines)]
fn verify_reevaluation_events(
    connection: &Connection,
    reevaluations: &HashMap<String, ReevaluationRecord>,
) -> Result<HashMap<String, String>> {
    let mut statement = connection.prepare(
        "SELECT reevaluation_id, state, error_code, detail,
                previous_event_hash, event_hash, occurred_at
         FROM reevaluation_events ORDER BY reevaluation_id, sequence",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut previous = HashMap::<String, String>::new();
    let mut events =
        HashMap::<String, Vec<(String, Option<String>, Option<String>, String)>>::new();
    for row in rows {
        let (reevaluation_id, state, error_code, detail, stored_previous, event_hash, occurred_at) =
            row?;
        if !reevaluations.contains_key(&reevaluation_id) {
            return integrity(format!(
                "re-evaluation event refers to missing run {reevaluation_id}"
            ));
        }
        let expected_previous = previous.get(&reevaluation_id).cloned();
        if stored_previous != expected_previous {
            return integrity(format!(
                "re-evaluation {reevaluation_id} event hash chain is broken"
            ));
        }
        require_timestamp(&occurred_at, "re-evaluation event timestamp")?;
        let expected_hash = reevaluation_event_hash(
            &reevaluation_id,
            &state,
            error_code.as_deref(),
            detail.as_deref(),
            stored_previous.as_deref(),
            &occurred_at,
        )?;
        if event_hash != expected_hash {
            return integrity(format!(
                "re-evaluation {reevaluation_id} event hash mismatch"
            ));
        }
        previous.insert(reevaluation_id.clone(), event_hash);
        events
            .entry(reevaluation_id)
            .or_default()
            .push((state, error_code, detail, occurred_at));
    }
    let mut heads = HashMap::new();
    for (reevaluation_id, run) in reevaluations {
        let run_events = events.get(reevaluation_id).ok_or_else(|| {
            integrity_error(format!(
                "re-evaluation {reevaluation_id} has no event chain"
            ))
        })?;
        let chain = run_events
            .iter()
            .map(|(state, _, _, _)| state.as_str())
            .collect::<Vec<_>>();
        let valid_chain = match run.state.as_str() {
            "PLANNED" => chain == ["PLANNED"],
            "RUNNING" => chain == ["PLANNED", "RUNNING"],
            terminal => {
                chain.len() == 3
                    && chain[0] == "PLANNED"
                    && chain[1] == "RUNNING"
                    && chain[2] == terminal
            }
        };
        if !valid_chain {
            return integrity(format!(
                "re-evaluation {reevaluation_id} event states do not match immutable run state {}",
                run.state
            ));
        }
        if run_events[0].1.is_some()
            || run_events[0].2.is_some()
            || run_events[0].3 != run.created_at
            || run_events.get(1).is_some_and(|event| {
                event.1.is_some() || event.2.is_some() || event.3 != run.created_at
            })
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} planning/running events differ from creation evidence"
            ));
        }
        if let Some(final_event) = run_events.get(2) {
            let expected_timestamp = run.completed_at.as_ref().ok_or_else(|| {
                integrity_error(format!(
                    "terminal re-evaluation {reevaluation_id} has no completion time"
                ))
            })?;
            let expected_detail = (run.state != "COMPLETE")
                .then_some("bounded re-evaluation did not qualify for authority");
            if final_event.1 != run.error_code
                || final_event.2.as_deref() != expected_detail
                || &final_event.3 != expected_timestamp
            {
                return integrity(format!(
                    "re-evaluation {reevaluation_id} final event does not bind terminal diagnostics/time"
                ));
            }
        }
        heads.insert(
            reevaluation_id.clone(),
            chain.last().copied().unwrap_or_default().to_owned(),
        );
    }
    Ok(heads)
}

#[allow(clippy::too_many_lines)]
fn verify_reevaluation_issues(
    connection: &Connection,
    reevaluations: &HashMap<String, ReevaluationRecord>,
) -> Result<HashMap<String, Vec<ReevaluationIssueDigest>>> {
    let mut statement = connection.prepare(
        "SELECT reevaluation_id, issue_id, outcome,
                source_validation_run_id, source_issue_id,
                severity, rule_id, issue_code, display_message,
                entity_id, field_id, source_file, source_ordinal,
                line_start, line_end, lineage_json, issue_hash
         FROM reevaluation_issues ORDER BY reevaluation_id, issue_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, i64>(13)?,
            row.get::<_, i64>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, String>(16)?,
        ))
    })?;
    let mut issues = HashMap::<String, Vec<ReevaluationIssueDigest>>::new();
    let mut anchors = HashMap::<String, (String, String, String)>::new();
    for row in rows {
        let (
            reevaluation_id,
            issue_id,
            outcome,
            source_run_id,
            source_issue_id,
            severity,
            rule_id,
            issue_code,
            display_message,
            entity_id,
            field_id,
            source_file,
            source_ordinal,
            line_start,
            line_end,
            lineage_json,
            stored_issue_hash,
        ) = row?;
        let run = reevaluations.get(&reevaluation_id).ok_or_else(|| {
            integrity_error(format!(
                "issue belongs to missing re-evaluation {reevaluation_id}"
            ))
        })?;
        let lineage = parse_canonical_json(&lineage_json, "re-evaluation issue lineage")?;
        let source_ordinal_value = nonnegative(source_ordinal, "re-evaluation issue ordinal")?;
        let line_start_value = nonnegative(line_start, "re-evaluation issue line start")?;
        let line_end_value = nonnegative(line_end, "re-evaluation issue line end")?;
        let anchors_key = run.source_validation_run_id.clone();
        if !anchors.contains_key(&anchors_key) {
            let loaded: Option<(String, String, String)> = connection
                .query_row(
                    "SELECT source_hash, profile_hash, mapping_hash
                     FROM validation_runs WHERE id = ?1",
                    [&run.source_validation_run_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            anchors.insert(
                anchors_key.clone(),
                loaded.ok_or_else(|| {
                    integrity_error(format!(
                        "re-evaluation {reevaluation_id} source validation anchors are missing"
                    ))
                })?,
            );
        }
        let (source_hash, profile_hash, mapping_hash) = anchors
            .get(&anchors_key)
            .cloned()
            .ok_or_else(|| integrity_error("re-evaluation anchor cache lost an entry"))?;
        let engine_issue_hash = if outcome == "RESOLVED" {
            let source = load_source_issue(
                connection,
                source_run_id.as_deref(),
                source_issue_id.as_deref(),
            )?;
            if severity != source.severity
                || rule_id != source.rule_id
                || issue_code != source.issue_code
                || display_message != source.display_message
                || entity_id != source.entity_id
                || field_id != source.field_id
                || source_file != source.source_file
                || source_ordinal_value != source.source_ordinal
                || line_start_value != source.line_start
                || line_end_value != source.line_end
                || lineage != source.lineage
            {
                return integrity(format!(
                    "resolved re-evaluation issue {reevaluation_id}/{issue_id} differs from source evidence"
                ));
            }
            source.issue_hash
        } else {
            canonical_hash(&json!({
                "contract": VALIDATION_ISSUE_CONTRACT,
                "sourceHash": source_hash,
                "profileHash": profile_hash,
                "mappingHash": mapping_hash,
                "severity": severity,
                "ruleId": rule_id,
                "issueCode": issue_code,
                "displayMessage": display_message,
                "entityId": entity_id,
                "fieldId": field_id,
                "sourceFile": source_file,
                "sourceOrdinal": source_ordinal_value,
                "lineStart": line_start_value,
                "lineEnd": line_end_value,
                "lineage": lineage,
            }))?
        };
        let material = reevaluation_issue_material(
            &reevaluation_id,
            &issue_id,
            &outcome,
            source_run_id.as_deref(),
            source_issue_id.as_deref(),
            &severity,
            &rule_id,
            &issue_code,
            &display_message,
            &entity_id,
            &field_id,
            &source_file,
            source_ordinal_value,
            line_start_value,
            line_end_value,
            &lineage,
            &engine_issue_hash,
        );
        let existed_in_source: i64 = connection.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM validation_issues WHERE run_id = ?1 AND issue_id = ?2
             )",
            params![run.source_validation_run_id, issue_id],
            |row| row.get(0),
        )?;
        if canonical_hash(&material)? != stored_issue_hash
            || issue_id != format!("issue-{engine_issue_hash}")
            || !matches!(outcome.as_str(), "RESOLVED" | "RESIDUAL" | "NEW")
            || source_run_id
                .as_deref()
                .is_some_and(|value| value != run.source_validation_run_id)
            || (outcome != "NEW" && source_issue_id.as_deref() != Some(issue_id.as_str()))
            || (outcome == "NEW" && (source_run_id.is_some() || source_issue_id.is_some()))
            || (outcome == "NEW" && existed_in_source != 0)
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} issue {issue_id} binding/hash mismatch"
            ));
        }
        let accepted_row: i64 = connection.query_row(
            "SELECT COUNT(*) FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
            params![run.job_id, source_file, source_ordinal],
            |row| row.get(0),
        )?;
        if accepted_row != 1 {
            return integrity(format!(
                "re-evaluation {reevaluation_id} issue {issue_id} has no exact accepted row"
            ));
        }
        issues
            .entry(reevaluation_id)
            .or_default()
            .push(ReevaluationIssueDigest {
                issue_id,
                issue_hash: stored_issue_hash,
                outcome,
            });
    }
    for values in issues.values_mut() {
        values.sort();
    }
    for (reevaluation_id, run) in reevaluations {
        if run.state != "COMPLETE" {
            continue;
        }
        let source_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
            [&run.source_validation_run_id],
            |row| row.get(0),
        )?;
        let source_row_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
            [&run.job_id],
            |row| row.get(0),
        )?;
        let represented = issues
            .get(reevaluation_id)
            .into_iter()
            .flatten()
            .filter(|issue| issue.outcome != "NEW")
            .count();
        if nonnegative(source_count, "source validation issue count")?
            != to_u64(represented, "represented source issues")?
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} does not preserve every source issue as residual or resolved evidence"
            ));
        }
        if run.rows_processed != nonnegative(source_row_count, "re-evaluated source row count")? {
            return integrity(format!(
                "re-evaluation {reevaluation_id} rows processed differ from the full inherited source scope"
            ));
        }
    }
    Ok(issues)
}

fn verify_reevaluation_results(
    reevaluations: &HashMap<String, ReevaluationRecord>,
    event_heads: &HashMap<String, String>,
    issues: &HashMap<String, Vec<ReevaluationIssueDigest>>,
) -> Result<()> {
    for (reevaluation_id, run) in reevaluations {
        if event_heads.get(reevaluation_id) != Some(&run.state) {
            return integrity(format!(
                "re-evaluation {reevaluation_id} event head differs from run state"
            ));
        }
        let evidence = issues.get(reevaluation_id).cloned().unwrap_or_default();
        if run.issue_count != to_u64(evidence.len(), "re-evaluation evidence issues")? {
            return integrity(format!(
                "re-evaluation {reevaluation_id} issue count differs from immutable evidence"
            ));
        }
        if run.state != "COMPLETE"
            && (run.issue_count != 0 || run.rows_processed != 0 || !evidence.is_empty())
        {
            return integrity(format!(
                "non-COMPLETE re-evaluation {reevaluation_id} contains partial result authority"
            ));
        }
        let result_material = reevaluation_result_material(run, &evidence)?;
        if run.state == "COMPLETE" {
            let raw = run.result_json.as_deref().ok_or_else(|| {
                integrity_error(format!(
                    "COMPLETE re-evaluation {reevaluation_id} lacks result"
                ))
            })?;
            let stored = parse_canonical_json(raw, "re-evaluation result")?;
            let expected_result_hash = canonical_hash(&result_material)?;
            if stored != result_material
                || run.result_hash.as_deref() != Some(expected_result_hash.as_str())
            {
                return integrity(format!(
                    "re-evaluation {reevaluation_id} result bytes/hash mismatch"
                ));
            }
        }
        if !matches!(run.state.as_str(), "PLANNED" | "RUNNING") {
            let completed_at = run.completed_at.as_deref().ok_or_else(|| {
                integrity_error(format!(
                    "terminal re-evaluation {reevaluation_id} lacks timestamp"
                ))
            })?;
            let terminal = reevaluation_terminal_material(run, &evidence, completed_at);
            let expected_terminal_hash = canonical_hash(&terminal)?;
            if run.terminal_hash.as_deref() != Some(expected_terminal_hash.as_str()) {
                return integrity(format!(
                    "re-evaluation {reevaluation_id} terminal hash mismatch"
                ));
            }
        }
    }
    Ok(())
}

fn load_effectivations(connection: &Connection) -> Result<HashMap<String, EffectivationRecord>> {
    let mut statement = connection.prepare(
        "SELECT effectivation_id, decision_id, decision_attempt_id,
                correction_overlay_id, reevaluation_id,
                decision_disposition, reevaluation_state, decision_generation,
                original_value_hash, prior_effective_value_hash,
                effective_value_kind, effective_value_text, effective_value_hash,
                effective_lineage_json, effective_lineage_hash,
                effectivation_hash, effectivated_at
         FROM correction_effectivations ORDER BY decision_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(EffectivationRecord {
            effectivation_id: row.get(0)?,
            decision_id: row.get(1)?,
            attempt_id: row.get(2)?,
            overlay_id: row.get(3)?,
            reevaluation_id: row.get(4)?,
            decision_disposition: row.get(5)?,
            reevaluation_state: row.get(6)?,
            generation: nonnegative_sql(row.get(7)?)?,
            original_value_hash: row.get(8)?,
            prior_effective_value_hash: row.get(9)?,
            effective_value_kind: row.get(10)?,
            effective_value: row.get(11)?,
            effective_value_hash: row.get(12)?,
            effective_lineage_json: row.get(13)?,
            effective_lineage_hash: row.get(14)?,
            effectivation_hash: row.get(15)?,
            effectivated_at: row.get(16)?,
        })
    })?;
    let mut effectivations = HashMap::new();
    for row in rows {
        let row = row?;
        let decision_id = row.decision_id.clone();
        if effectivations.insert(decision_id.clone(), row).is_some() {
            return integrity(format!(
                "decision {decision_id} has duplicate effectivations"
            ));
        }
    }
    Ok(effectivations)
}

struct DecisionReplay {
    decisions: HashSet<String>,
    corrected_decisions: usize,
    active_heads: usize,
    consumed_effectivations: HashSet<String>,
    consumed_reevaluations: HashSet<String>,
    consumed_overlays: HashSet<String>,
}

#[allow(clippy::too_many_lines)]
fn verify_decisions_and_effectivations(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
    attempt_event_heads: &HashMap<String, AttemptEventHead>,
    overlays: &HashMap<String, OverlayEvidence>,
    reevaluations: &HashMap<String, ReevaluationRecord>,
    effectivations: &HashMap<String, EffectivationRecord>,
) -> Result<DecisionReplay> {
    let mut statement = connection.prepare(
        "SELECT commit_sequence, decision_id, schema_version, decision_attempt_id,
                job_id, validation_run_id, issue_id, disposition,
                predecessor_decision_id, predecessor_generation, generation,
                correction_overlay_id, reevaluation_id, reevaluation_state,
                practitioner_id, reason, evidence_reference, lifecycle_outcome,
                canonical_decision_json, decision_hash, committed_at
         FROM issue_decisions
         ORDER BY commit_sequence",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, String>(17)?,
            row.get::<_, String>(18)?,
            row.get::<_, String>(19)?,
            row.get::<_, String>(20)?,
        ))
    })?;
    let mut heads = HashMap::<(String, String), HeadState>::new();
    let mut field_heads = HashMap::<FieldKey, FieldHead>::new();
    let mut replay_authorities = HashMap::<String, ReplayAuthority>::new();
    let mut last_commit_sequence = 0_u64;
    let mut decisions = HashSet::new();
    let mut corrected_decisions = 0_usize;
    let mut consumed_effectivations = HashSet::new();
    let mut consumed_reevaluations = HashSet::new();
    let mut consumed_overlays = HashSet::new();
    for row in rows {
        let (
            commit_sequence,
            decision_id,
            schema_version,
            attempt_id,
            job_id,
            validation_run_id,
            issue_id,
            disposition,
            predecessor_id,
            predecessor_generation,
            generation,
            overlay_id,
            reevaluation_id,
            reevaluation_state,
            practitioner_id,
            reason,
            evidence_reference,
            lifecycle_outcome,
            decision_json,
            decision_hash,
            committed_at,
        ) = row?;
        let commit_sequence = nonnegative(commit_sequence, "decision commit sequence")?;
        if commit_sequence == 0 || commit_sequence <= last_commit_sequence {
            return integrity(format!(
                "decision {decision_id} has a nonpositive or nonmonotonic commit sequence"
            ));
        }
        last_commit_sequence = commit_sequence;
        let attempt = attempts.get(&attempt_id).ok_or_else(|| {
            integrity_error(format!("decision {decision_id} has no immutable attempt"))
        })?;
        if attempt_event_heads
            .get(&attempt_id)
            .map(|head| head.state.as_str())
            != Some("EFFECTIVE")
        {
            return integrity(format!(
                "decision {decision_id} is not bound to an EFFECTIVE attempt event head"
            ));
        }
        let material = parse_canonical_json(&decision_json, "issue decision material")?;
        let mut record = material
            .get("record")
            .cloned()
            .ok_or_else(|| integrity_error("issue decision material lacks record"))?;
        record
            .as_object_mut()
            .ok_or_else(|| integrity_error("issue decision record is not an object"))?
            .insert(
                "decisionHash".to_owned(),
                Value::String(decision_hash.clone()),
            );
        let decision: IssueDecision = serde_json::from_value(record).map_err(|error| {
            integrity_error(format!(
                "decision {decision_id} contract is invalid: {error}"
            ))
        })?;
        decision.validate().map_err(|error| {
            integrity_error(format!("decision {decision_id} is invalid: {error}"))
        })?;
        let generation = nonnegative(generation, "decision generation")?;
        let predecessor_generation =
            nonnegative(predecessor_generation, "decision predecessor generation")?;
        if canonical_decision_material(&decision)? != material
            || schema_version != i64::from(decision.schema_version)
            || decision_id != decision.decision_id
            || decision.request_hash != canonical_decision_request_hash(&attempt.request)?
            || decision.idempotency_key != attempt.request.idempotency_key
            || decision.disposition != attempt.request.disposition
            || decision.scope != attempt.request.scope
            || decision.concurrency != attempt.request.concurrency
            || decision.practitioner_id != attempt.request.practitioner_id
            || decision.reason != attempt.request.reason
            || decision.evidence_reference != attempt.request.evidence_reference
            || decision.authority_separation_acknowledged
                != attempt.request.authority_separation_acknowledged
            || decision.row_scope_exclusion_acknowledged
                != attempt.request.row_scope_exclusion_acknowledged
            || disposition != decision.disposition.as_str()
            || practitioner_id != decision.practitioner_id
            || reason != decision.reason
            || evidence_reference != decision.evidence_reference
            || decision.active_generation != generation
            || predecessor_id != decision.concurrency.expected_active_decision_id
            || predecessor_generation != decision.concurrency.expected_active_decision_generation
            || overlay_id != decision.correction_overlay_id
            || reevaluation_id != decision.reevaluation_id
            || lifecycle_outcome != "EFFECTIVE"
            || committed_at != decision.application_timestamp
            || job_id != decision.scope.job_id
            || validation_run_id != decision.scope.validation_run_id
            || issue_id != decision.scope.issue_id
            || attempt_event_heads
                .get(&attempt_id)
                .is_some_and(|head| head.error_code.is_some())
        {
            return integrity(format!(
                "decision {decision_id} columns differ from canonical decision/attempt evidence"
            ));
        }
        require_timestamp(&committed_at, "decision commit timestamp")?;
        let key = (validation_run_id.clone(), issue_id.clone());
        let previous = heads.get(&key);
        match previous {
            None if generation == 1 && predecessor_id.is_none() => {}
            Some(previous)
                if generation == previous.generation + 1
                    && predecessor_id.as_deref() == Some(previous.decision_id.as_str())
                    && predecessor_generation == previous.generation => {}
            _ => {
                return integrity(format!(
                    "decision {decision_id} has a gap, fork, cycle, or wrong predecessor"
                ));
            }
        }
        let original = replay_original_evidence(connection, &decision.scope)?;
        let field_key = field_key(&decision.scope);
        let field_head = field_heads.entry(field_key).or_insert_with(|| FieldHead {
            current_effective_hash: original.initial_effective_hash.clone(),
            original: original.clone(),
        });
        if field_head.original.lineage != original.lineage
            || field_head.original.value_hash != original.value_hash
            || field_head.original.initial_effective_hash != original.initial_effective_hash
        {
            return integrity(format!(
                "decision {decision_id} replays against inconsistent immutable original evidence"
            ));
        }
        if let Some(expected_hash) = decision.concurrency.expected_effective_value_hash.as_ref()
            && expected_hash != &field_head.current_effective_hash
        {
            return integrity(format!(
                "decision {decision_id} does not bind to the exact effective field head at commit"
            ));
        }
        if decision.disposition == IssueDisposition::Corrected {
            corrected_decisions += 1;
            let overlay_id = overlay_id.as_deref().ok_or_else(|| {
                integrity_error(format!("corrected decision {decision_id} lacks overlay"))
            })?;
            let reevaluation_id = reevaluation_id.as_deref().ok_or_else(|| {
                integrity_error(format!(
                    "corrected decision {decision_id} lacks re-evaluation"
                ))
            })?;
            let overlay = overlays.get(overlay_id).ok_or_else(|| {
                integrity_error(format!(
                    "corrected decision {decision_id} overlay is missing"
                ))
            })?;
            let reevaluation = reevaluations.get(reevaluation_id).ok_or_else(|| {
                integrity_error(format!(
                    "corrected decision {decision_id} re-evaluation is missing"
                ))
            })?;
            let effectivation = effectivations.get(&decision_id).ok_or_else(|| {
                integrity_error(format!(
                    "corrected decision {decision_id} lacks atomic effectivation"
                ))
            })?;
            let expected_before = decision
                .concurrency
                .expected_effective_value_hash
                .as_deref()
                .ok_or_else(|| {
                    integrity_error(format!(
                        "corrected decision {decision_id} lacks an expected effective hash"
                    ))
                })?;
            if reevaluation.state != "COMPLETE"
                || reevaluation_state.as_deref() != Some("COMPLETE")
                || reevaluation.attempt_id != attempt_id
                || reevaluation.overlay_id != overlay_id
                || overlay.attempt_id != attempt_id
                || reevaluation.completed_at.as_deref()
                    != Some(decision.application_timestamp.as_str())
            {
                return integrity(format!(
                    "corrected decision {decision_id} is not bound to one qualifying COMPLETE re-evaluation"
                ));
            }
            if !replay_authorities.contains_key(&decision.scope.validation_run_id) {
                let authority = load_replay_authority(connection, &decision.scope)?;
                replay_authorities.insert(decision.scope.validation_run_id.clone(), authority);
            }
            let replay_authority = replay_authorities
                .get(&decision.scope.validation_run_id)
                .ok_or_else(|| integrity_error("decision replay authority cache lost an entry"))?;
            let eligibility = replay_correction_eligibility(&decision.scope, replay_authority)?;
            overlay.overlay.validate(&eligibility).map_err(|error| {
                integrity_error(format!(
                    "corrected decision {decision_id} no longer replays as eligible: {error}"
                ))
            })?;
            let evaluation = evaluate_effective_overlay_before_commit(
                connection,
                &decision.scope.job_id,
                &decision.scope.validation_run_id,
                &replay_authority.source_hash,
                &replay_authority.mapping,
                &replay_authority.mapping_hash,
                &replay_authority.rules,
                &replay_authority.policy,
                &EffectiveOverlayInput {
                    source_file: decision.scope.accepted_source_file.clone(),
                    source_ordinal: decision.scope.source_ordinal,
                    entity_id: decision.scope.canonical_entity_id.clone(),
                    field_id: decision.scope.canonical_field_id.clone(),
                    value: overlay.overlay.effective_value().map(str::to_owned),
                },
                &ValidationControl::default(),
                Some(commit_sequence),
            )
            .map_err(|error| {
                integrity_error(format!(
                    "corrected decision {decision_id} deterministic re-evaluation failed: {error}"
                ))
            })?;
            if evaluation.scope_kind != "FULL_INHERITED_VALIDATION_SCOPE"
                || evaluation.rows_processed != reevaluation.rows_processed
            {
                return integrity(format!(
                    "corrected decision {decision_id} re-evaluation scope or row count does not replay"
                ));
            }
            verify_deterministic_reevaluation_issues(
                connection,
                reevaluation_id,
                &decision.scope.validation_run_id,
                &evaluation.issues,
            )?;
            verify_effectivation(
                effectivation,
                &decision,
                overlay,
                &original,
                expected_before,
            )?;
            field_head
                .current_effective_hash
                .clone_from(&effectivation.effective_value_hash);
            consumed_effectivations.insert(effectivation.effectivation_id.clone());
            consumed_reevaluations.insert(reevaluation_id.to_owned());
            consumed_overlays.insert(overlay_id.to_owned());
        } else if reevaluation_state.is_some()
            || decision.application_timestamp != attempt.created_at
            || effectivations.contains_key(&decision_id)
        {
            return integrity(format!(
                "non-correction decision {decision_id} has correction authority or mismatched time"
            ));
        }
        heads.insert(
            key,
            HeadState {
                decision_id: decision_id.clone(),
                generation,
            },
        );
        decisions.insert(attempt_id);
    }
    Ok(DecisionReplay {
        decisions,
        corrected_decisions,
        active_heads: heads.len(),
        consumed_effectivations,
        consumed_reevaluations,
        consumed_overlays,
    })
}

fn field_key(scope: &DecisionScope) -> FieldKey {
    (
        scope.job_id.clone(),
        scope.validation_run_id.clone(),
        scope.accepted_source_file.clone(),
        scope.source_ordinal,
        scope.canonical_entity_id.clone(),
        scope.canonical_field_id.clone(),
        scope.profile_id.clone(),
        scope.profile_version.clone(),
        scope.profile_hash.clone(),
        scope.mapping_id.clone(),
        scope.mapping_version.clone(),
        scope.mapping_hash.clone(),
    )
}

fn load_replay_authority(
    connection: &Connection,
    scope: &DecisionScope,
) -> Result<ReplayAuthority> {
    type ValidationAuthorityRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let row: ValidationAuthorityRow = connection.query_row(
        "SELECT state, source_hash,
                profile_id, profile_version, profile_hash,
                mapping_id, mapping_version, mapping_hash,
                rule_set_hash, engine_version, policy_json, policy_hash
         FROM validation_runs WHERE id = ?1 AND job_id = ?2",
        params![scope.validation_run_id, scope.job_id],
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
            ))
        },
    )?;
    if row.0 != "COMPLETE"
        || row.2 != scope.profile_id
        || row.3 != scope.profile_version
        || row.4 != scope.profile_hash
        || row.5 != scope.mapping_id
        || row.6 != scope.mapping_version
        || row.7 != scope.mapping_hash
    {
        return integrity("decision replay authority differs from its immutable validation scope");
    }
    let mapping = load_stored_mapping(connection, &scope.mapping_id, &scope.mapping_version)?;
    if mapping.mapping_hash != scope.mapping_hash
        || mapping.document.profile.profile_id != scope.profile_id
        || mapping.document.profile.version != scope.profile_version
        || mapping.document.profile.sha256 != scope.profile_hash
    {
        return integrity("decision replay mapping differs from its immutable scope");
    }
    let profile_document = load_verified_profile_document(
        connection,
        &scope.profile_id,
        &scope.profile_version,
        &scope.profile_hash,
    )?;
    let rules = profile_document
        .get("rules")
        .cloned()
        .ok_or_else(|| integrity_error("decision replay profile has no rules"))?;
    if canonical_hash(&rules)? != row.8 {
        return integrity("decision replay rule set differs from validation authority");
    }
    let policy_value = parse_unique_json(row.10.as_bytes()).map_err(|error| {
        integrity_error(format!("decision replay policy JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&policy_value)? != row.10 || canonical_hash(&policy_value)? != row.11 {
        return integrity("decision replay policy bytes/hash differ from validation authority");
    }
    let policy: ValidationPolicy = serde_json::from_value(policy_value).map_err(|error| {
        integrity_error(format!(
            "decision replay policy contract is invalid: {error}"
        ))
    })?;
    Ok(ReplayAuthority {
        source_hash: row.1,
        mapping: mapping.document,
        mapping_hash: mapping.mapping_hash,
        profile_document,
        rules,
        policy,
    })
}

fn replay_correction_eligibility(
    scope: &DecisionScope,
    authority: &ReplayAuthority,
) -> Result<TextualCorrectionEligibility> {
    let entities = authority
        .profile_document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| integrity_error("decision replay profile entities are absent"))?;
    let matching_entities = entities
        .iter()
        .filter(|entity| {
            entity.get("entityId").and_then(Value::as_str)
                == Some(scope.canonical_entity_id.as_str())
        })
        .collect::<Vec<_>>();
    let entity = matching_entities
        .first()
        .copied()
        .ok_or_else(|| integrity_error("decision replay correction entity is absent"))?;
    let fields = entity
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| integrity_error("decision replay profile fields are absent"))?;
    let matching_fields = fields
        .iter()
        .filter(|field| {
            field.get("fieldId").and_then(Value::as_str) == Some(scope.canonical_field_id.as_str())
        })
        .collect::<Vec<_>>();
    let field = matching_fields
        .first()
        .copied()
        .ok_or_else(|| integrity_error("decision replay correction field is absent"))?;
    let binding_count = authority
        .mapping
        .bindings
        .iter()
        .filter(|binding| binding.target_field_id == scope.canonical_field_id)
        .count();
    let maximum_length = field
        .get("maxLength")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .map_err(|_| integrity_error("decision replay maxLength exceeds usize"))?;
    Ok(TextualCorrectionEligibility {
        exact_scope_match: matching_entities.len() == 1
            && matching_fields.len() == 1
            && binding_count == 1,
        root_entity: entity.get("root").and_then(Value::as_bool) == Some(true)
            && authority.mapping.root_entity_id == scope.canonical_entity_id,
        singular: matching_fields.len() == 1 && binding_count == 1,
        logical_type: if field.get("logicalType").and_then(Value::as_str) == Some("text") {
            CorrectionLogicalType::Text
        } else {
            CorrectionLogicalType::NonText
        },
        sensitive: field
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required: field
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        maximum_length,
    })
}

#[allow(clippy::too_many_lines)]
fn verify_deterministic_reevaluation_issues(
    connection: &Connection,
    reevaluation_id: &str,
    validation_run_id: &str,
    evaluated: &[IssueEvidence],
) -> Result<()> {
    let mut source_statement = connection
        .prepare("SELECT issue_id FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id")?;
    let source_ids = source_statement
        .query_map([validation_run_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;

    let mut statement = connection.prepare(
        "SELECT issue_id, outcome, severity, rule_id, issue_code, display_message,
                entity_id, field_id, source_file, source_ordinal,
                line_start, line_end, lineage_json
         FROM reevaluation_issues WHERE reevaluation_id = ?1 ORDER BY issue_id",
    )?;
    let rows = statement.query_map([reevaluation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PersistedReplayIssue {
                outcome: row.get(1)?,
                severity: row.get(2)?,
                rule_id: row.get(3)?,
                issue_code: row.get(4)?,
                display_message: row.get(5)?,
                entity_id: row.get(6)?,
                field_id: row.get(7)?,
                source_file: row.get(8)?,
                source_ordinal: nonnegative_sql(row.get(9)?)?,
                line_start: nonnegative_sql(row.get(10)?)?,
                line_end: nonnegative_sql(row.get(11)?)?,
                lineage_json: row.get(12)?,
            },
        ))
    })?;
    let mut persisted = HashMap::new();
    for row in rows {
        let (issue_id, issue) = row?;
        if persisted.insert(issue_id.clone(), issue).is_some() {
            return integrity(format!(
                "re-evaluation {reevaluation_id} has duplicate issue {issue_id}"
            ));
        }
    }
    let mut evaluated_ids = HashSet::new();
    for issue in evaluated {
        if !evaluated_ids.insert(issue.issue_id.clone()) {
            return integrity(format!(
                "re-evaluation {reevaluation_id} engine replay produced a duplicate issue"
            ));
        }
        let stored = persisted.get(&issue.issue_id).ok_or_else(|| {
            integrity_error(format!(
                "re-evaluation {reevaluation_id} omits deterministic issue {}",
                issue.issue_id
            ))
        })?;
        let expected_outcome = if source_ids.contains(&issue.issue_id) {
            "RESIDUAL"
        } else {
            "NEW"
        };
        if stored.outcome != expected_outcome
            || stored.severity != issue.severity
            || stored.rule_id != issue.rule_id
            || stored.issue_code != issue.issue_code
            || stored.display_message != issue.display_message
            || stored.entity_id != issue.entity_id
            || stored.field_id != issue.field_id
            || stored.source_file != issue.source_file
            || stored.source_ordinal != issue.source_ordinal
            || stored.line_start != issue.line_start
            || stored.line_end != issue.line_end
            || stored.lineage_json != issue.lineage_json
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} differs from deterministic issue {}",
                issue.issue_id
            ));
        }
    }
    for source_id in &source_ids {
        if !evaluated_ids.contains(source_id)
            && persisted.get(source_id).map(|issue| issue.outcome.as_str()) != Some("RESOLVED")
        {
            return integrity(format!(
                "re-evaluation {reevaluation_id} omits deterministic resolution {source_id}"
            ));
        }
    }
    let expected_count = source_ids.union(&evaluated_ids).count();
    if persisted.len() != expected_count {
        return integrity(format!(
            "re-evaluation {reevaluation_id} contains evidence outside deterministic replay"
        ));
    }
    Ok(())
}

fn verify_effectivation(
    effectivation: &EffectivationRecord,
    decision: &IssueDecision,
    overlay: &OverlayEvidence,
    original: &OriginalEvidence,
    current_before: &str,
) -> Result<()> {
    let effective_value = overlay.overlay.effective_value().map(str::to_owned);
    let expected_kind = value_kind(&effective_value);
    let expected_lineage = effective_lineage(
        &original.lineage,
        &overlay.overlay.overlay_id,
        &overlay.overlay.overlay_hash,
        current_before,
        decision.active_generation,
    );
    let lineage = parse_canonical_json(
        &effectivation.effective_lineage_json,
        "effective correction lineage",
    )?;
    let expected_lineage_hash = canonical_hash(&expected_lineage)?;
    let expected_effective_hash = effective_value_hash(
        &decision.scope,
        &original.value_hash,
        Some(current_before),
        Some(&overlay.overlay.overlay_hash),
        decision.active_generation,
        &effective_value,
    )?;
    let expected_effectivation_hash = canonical_hash(&effectivation_material(
        &effectivation.effectivation_id,
        &overlay.overlay.overlay_id,
        decision.active_generation,
        &original.value_hash,
        current_before,
        expected_kind,
        &effective_value,
        &expected_effective_hash,
        &expected_lineage_hash,
    ))?;
    if effectivation.decision_id != decision.decision_id
        || effectivation.attempt_id != overlay.attempt_id
        || effectivation.overlay_id != overlay.overlay.overlay_id
        || decision.reevaluation_id.as_deref() != Some(effectivation.reevaluation_id.as_str())
        || effectivation.decision_disposition != "CORRECTED"
        || effectivation.reevaluation_state != "COMPLETE"
        || effectivation.generation != decision.active_generation
        || effectivation.original_value_hash != original.value_hash
        || effectivation.prior_effective_value_hash != current_before
        || overlay.overlay.prior_effective_value_hash != current_before
        || effectivation.effective_value_kind != expected_kind
        || effectivation.effective_value != effective_value
        || effectivation.effective_value_hash != expected_effective_hash
        || lineage != expected_lineage
        || effectivation.effective_lineage_hash != expected_lineage_hash
        || effectivation.effectivation_hash != expected_effectivation_hash
        || effectivation.effectivated_at != decision.application_timestamp
    {
        return integrity(format!(
            "decision {} correction effectivation value, lineage, generation, or hash mismatch",
            decision.decision_id
        ));
    }
    require_timestamp(&effectivation.effectivated_at, "effectivation timestamp")
}

fn replay_original_evidence(
    connection: &Connection,
    scope: &DecisionScope,
) -> Result<OriginalEvidence> {
    let mapping = load_stored_mapping(connection, &scope.mapping_id, &scope.mapping_version)
        .map_err(|error| integrity_error(format!("decision mapping replay failed: {error}")))?;
    if mapping.mapping_hash != scope.mapping_hash {
        return integrity("decision scope mapping hash differs from immutable mapping");
    }
    let row: Option<(i64, i64, String)> = connection
        .query_row(
            "SELECT line_start, line_end, fields_json FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
            params![
                scope.job_id,
                scope.accepted_source_file,
                i64::try_from(scope.source_ordinal).map_err(|_| {
                    integrity_error("decision source ordinal does not fit SQLite INTEGER")
                })?
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (line_start, line_end, fields_json) =
        row.ok_or_else(|| integrity_error("decision scope has no immutable accepted source row"))?;
    if nonnegative(line_start, "source line start")? != scope.line_start
        || nonnegative(line_end, "source line end")? != scope.line_end
    {
        return integrity("decision scope line identity differs from accepted source row");
    }
    let source_values: Vec<String> = serde_json::from_str(&fields_json).map_err(|error| {
        integrity_error(format!("accepted source row cannot be replayed: {error}"))
    })?;
    let projected = project_record(
        &mapping.document,
        &scope.accepted_source_file,
        scope.source_ordinal,
        scope.line_start,
        scope.line_end,
        source_values,
    )?;
    if projected.entity_id != scope.canonical_entity_id {
        return integrity("decision entity differs from immutable canonical projection");
    }
    let value = projected
        .canonical_values
        .get(&scope.canonical_field_id)
        .cloned()
        .ok_or_else(|| integrity_error("decision field is absent from canonical projection"))?;
    let lineage = projected
        .lineage
        .get(&scope.canonical_field_id)
        .cloned()
        .ok_or_else(|| integrity_error("decision field has no canonical lineage"))?;
    let value_hash = original_value_hash(scope, &value, &lineage)?;
    let initial_effective_hash = effective_value_hash(scope, &value_hash, None, None, 0, &value)?;
    Ok(OriginalEvidence {
        lineage,
        value_hash,
        initial_effective_hash,
    })
}

/// Canonical decision-attempt lifecycle event hash shared with persistence.
pub(crate) fn decision_attempt_event_hash(
    attempt_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "attemptId": attempt_id,
        "contract": DECISION_EVENT_CONTRACT,
        "detail": detail,
        "errorCode": error_code,
        "occurredAt": occurred_at,
        "previousEventHash": previous_event_hash,
        "state": state,
    }))
}

/// Canonical re-evaluation lifecycle event hash shared with persistence.
pub(crate) fn reevaluation_event_hash(
    reevaluation_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": REEVALUATION_EVENT_CONTRACT,
        "detail": detail,
        "errorCode": error_code,
        "occurredAt": occurred_at,
        "previousEventHash": previous_event_hash,
        "reevaluationId": reevaluation_id,
        "state": state,
    }))
}

/// Canonical conservative dependency scope for the WP-005 MVP.
pub(crate) fn reevaluation_scope_material(scope: &DecisionScope) -> Value {
    json!({
        "contract": REEVALUATION_SCOPE_CONTRACT,
        "entityId": scope.canonical_entity_id,
        "fieldId": scope.canonical_field_id,
        "scopeKind": "FULL_INHERITED_VALIDATION_SCOPE",
        "sourceFile": scope.accepted_source_file,
        "sourceIssueId": scope.issue_id,
        "sourceOrdinal": scope.source_ordinal,
        "sourceValidationRunId": scope.validation_run_id,
    })
}

/// Canonical re-evaluation identity hash shared with persistence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reevaluation_identity_hash(
    attempt_id: &str,
    overlay_id: &str,
    source_validation_run_id: &str,
    source_issue_id: &str,
    rule_set_hash: &str,
    engine_version: &str,
    policy_hash: &str,
    dependency_scope_hash: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "attemptId": attempt_id,
        "contract": REEVALUATION_IDENTITY_CONTRACT,
        "dependencyScopeHash": dependency_scope_hash,
        "engineVersion": engine_version,
        "correctionOverlayId": overlay_id,
        "policyHash": policy_hash,
        "ruleSetHash": rule_set_hash,
        "sourceIssueId": source_issue_id,
        "sourceValidationRunId": source_validation_run_id,
    }))
}

#[allow(clippy::too_many_arguments)]
fn reevaluation_issue_material(
    reevaluation_id: &str,
    issue_id: &str,
    outcome: &str,
    source_validation_run_id: Option<&str>,
    source_issue_id: Option<&str>,
    severity: &str,
    rule_id: &str,
    issue_code: &str,
    display_message: &str,
    entity_id: &str,
    field_id: &str,
    source_file: &str,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    lineage: &Value,
    engine_issue_hash: &str,
) -> Value {
    json!({
        "contract": REEVALUATION_ISSUE_CONTRACT,
        "displayMessage": display_message,
        "engineIssueHash": engine_issue_hash,
        "entityId": entity_id,
        "fieldId": field_id,
        "issueCode": issue_code,
        "issueId": issue_id,
        "lineEnd": line_end,
        "lineStart": line_start,
        "lineage": lineage,
        "outcome": outcome,
        "reevaluationId": reevaluation_id,
        "ruleId": rule_id,
        "severity": severity,
        "sourceFile": source_file,
        "sourceIssueId": source_issue_id,
        "sourceOrdinal": source_ordinal,
        "sourceValidationRunId": source_validation_run_id,
    })
}

fn reevaluation_result_material(
    run: &ReevaluationRecord,
    issues: &[ReevaluationIssueDigest],
) -> Result<Value> {
    let resolved = issues
        .iter()
        .filter(|issue| issue.outcome == "RESOLVED")
        .count();
    let residual = issues
        .iter()
        .filter(|issue| issue.outcome == "RESIDUAL")
        .count();
    let new = issues.iter().filter(|issue| issue.outcome == "NEW").count();
    let issue_values = issue_digest_values(issues);
    Ok(json!({
        "contract": REEVALUATION_RESULT_CONTRACT,
        "currentIssueCount": residual.checked_add(new).ok_or_else(|| integrity_error("re-evaluation issue count overflow"))?,
        "dependencyScopeHash": run.dependency_scope_hash,
        "identityHash": run.identity_hash,
        "issues": issue_values,
        "newCount": new,
        "reevaluationId": run.reevaluation_id,
        "residualCount": residual,
        "resolvedCount": resolved,
        "rowsProcessed": run.rows_processed,
        "sourceIssueId": run.source_issue_id,
        "sourceValidationRunId": run.source_validation_run_id,
    }))
}

fn reevaluation_terminal_material(
    run: &ReevaluationRecord,
    issues: &[ReevaluationIssueDigest],
    completed_at: &str,
) -> Value {
    json!({
        "completedAt": completed_at,
        "contract": REEVALUATION_TERMINAL_CONTRACT,
        "createdAt": run.created_at,
        "errorCode": run.error_code,
        "errorMessage": run.error_message,
        "issues": issue_digest_values(issues),
        "identityHash": run.identity_hash,
        "issueCount": run.issue_count,
        "reevaluationId": run.reevaluation_id,
        "resultHash": run.result_hash,
        "rowsProcessed": run.rows_processed,
        "state": run.state,
    })
}

fn issue_digest_values(issues: &[ReevaluationIssueDigest]) -> Vec<Value> {
    issues
        .iter()
        .map(|issue| {
            json!({
                "issueHash": issue.issue_hash,
                "issueId": issue.issue_id,
                "outcome": issue.outcome,
            })
        })
        .collect()
}

#[allow(clippy::type_complexity)]
fn load_source_issue(
    connection: &Connection,
    run_id: Option<&str>,
    issue_id: Option<&str>,
) -> Result<SourceIssueEvidence> {
    let (run_id, issue_id) = run_id.zip(issue_id).ok_or_else(|| {
        integrity_error("resolved/residual re-evaluation issue lacks source identity")
    })?;
    let row: Option<(
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
    )> = connection
        .query_row(
            "SELECT severity, rule_id, issue_code, display_message,
                    entity_id, field_id, source_file, source_ordinal,
                    line_start, line_end, lineage_json, issue_hash
             FROM validation_issues WHERE run_id = ?1 AND issue_id = ?2",
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
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .optional()?;
    let row =
        row.ok_or_else(|| integrity_error("re-evaluation issue source evidence is missing"))?;
    Ok(SourceIssueEvidence {
        severity: row.0,
        rule_id: row.1,
        issue_code: row.2,
        display_message: row.3,
        entity_id: row.4,
        field_id: row.5,
        source_file: row.6,
        source_ordinal: nonnegative(row.7, "source issue ordinal")?,
        line_start: nonnegative(row.8, "source issue line start")?,
        line_end: nonnegative(row.9, "source issue line end")?,
        lineage: parse_canonical_json(&row.10, "source issue lineage")?,
        issue_hash: row.11,
    })
}

fn verify_reevaluation_state_shape(
    reevaluation_id: &str,
    state: &str,
    result_json: Option<&str>,
    result_hash: Option<&str>,
    terminal_hash: Option<&str>,
    error_code: Option<&str>,
    completed_at: Option<&str>,
) -> Result<()> {
    let valid = match state {
        "PLANNED" | "RUNNING" => {
            result_json.is_none()
                && result_hash.is_none()
                && terminal_hash.is_none()
                && error_code.is_none()
                && completed_at.is_none()
        }
        "COMPLETE" => {
            result_json.is_some()
                && result_hash.is_some()
                && terminal_hash.is_some()
                && error_code.is_none()
                && completed_at.is_some()
        }
        "INTERRUPTED" | "FAILED" | "BOUND_EXCEEDED" | "INTEGRITY_BLOCKED" | "STALE_INPUT" => {
            result_json.is_none()
                && result_hash.is_none()
                && terminal_hash.is_some()
                && error_code.is_some()
                && completed_at.is_some()
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        integrity(format!(
            "re-evaluation {reevaluation_id} has an invalid lifecycle/result shape"
        ))
    }
}

fn parse_canonical_json(raw: &str, label: &str) -> Result<Value> {
    let value = parse_unique_json(raw.as_bytes())
        .map_err(|error| integrity_error(format!("{label} is not strict JSON: {error}")))?;
    if canonical_json_string(&value)? != raw {
        return integrity(format!("{label} is not canonical JSON v1"));
    }
    Ok(value)
}

fn require_timestamp(value: &str, label: &str) -> Result<()> {
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|error| integrity_error(format!("{label} is invalid: {error}")))
}

fn normalized_schema_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn nonnegative(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| integrity_error(format!("{label} is negative")))
}

fn nonnegative_sql(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn to_u64(value: usize, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| integrity_error(format!("{label} count overflow")))
}

fn integrity<T>(detail: impl Into<String>) -> Result<T> {
    Err(integrity_error(detail))
}

fn integrity_error(detail: impl Into<String>) -> KernelError {
    KernelError::DecisionIntegrity(detail.into())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{
        IMMUTABLE_TABLES, MAXIMUM_REPLAY_EVIDENCE_BYTES, MAXIMUM_REPLAY_ROWS_PER_TABLE,
        decision_attempt_event_hash, ensure_write_capacity, reevaluation_event_hash,
        reevaluation_identity_hash, verify_decision_integrity_connection,
    };
    use crate::{
        KernelError,
        canonical::canonical_hash,
        decision_service::{
            decision_attempt_event_material, reevaluation_event_material,
            reevaluation_identity_material,
        },
        state::{DATABASE_FILENAME, recover_workspace},
    };

    fn empty_workspace_connection() -> (tempfile::TempDir, Connection) {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        recover_workspace(workspace.path()).expect("initialize current workspace schema");
        let connection = Connection::open(workspace.path().join(DATABASE_FILENAME))
            .expect("open workspace database");
        (workspace, connection)
    }

    fn governed_row_counts(connection: &Connection) -> Vec<i64> {
        IMMUTABLE_TABLES
            .iter()
            .map(|(table, _)| {
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("count governed evidence rows")
            })
            .collect()
    }

    #[test]
    fn empty_current_workspace_replays_without_authority() {
        let (_workspace, connection) = empty_workspace_connection();

        let summary =
            verify_decision_integrity_connection(&connection).expect("verify empty workspace");

        assert_eq!(summary.decision_attempts, 0);
        assert_eq!(summary.issue_decisions, 0);
        assert_eq!(summary.correction_effectivations, 0);
        assert_eq!(summary.active_decision_heads, 0);
        assert_eq!(summary.inert_overlays, 0);
        assert_eq!(summary.incomplete_attempts, 0);
    }

    #[test]
    fn altered_append_only_guard_fails_closed() {
        let (_workspace, connection) = empty_workspace_connection();
        connection
            .execute_batch("DROP TRIGGER decision_attempt_events_no_update;")
            .expect("remove append-only guard");

        let error = verify_decision_integrity_connection(&connection)
            .expect_err("missing append-only guard must fail");

        assert!(matches!(error, KernelError::DecisionIntegrity(_)));
        assert!(
            error
                .to_string()
                .contains("decision_attempt_events_no_update")
        );
    }

    #[test]
    fn prewrite_capacity_accepts_exact_limits_and_rejects_over_before_any_governed_write() {
        let (_workspace, connection) = empty_workspace_connection();
        let before = governed_row_counts(&connection);
        assert!(before.iter().all(|count| *count == 0));

        ensure_write_capacity(
            &connection,
            &[("decision_attempts", MAXIMUM_REPLAY_ROWS_PER_TABLE)],
            MAXIMUM_REPLAY_EVIDENCE_BYTES,
        )
        .expect("exact row and evidence-byte capacity must remain available");
        assert_eq!(governed_row_counts(&connection), before);

        let byte_error = ensure_write_capacity(
            &connection,
            &[("decision_attempts", MAXIMUM_REPLAY_ROWS_PER_TABLE)],
            MAXIMUM_REPLAY_EVIDENCE_BYTES + 1,
        )
        .expect_err("one byte over the immutable evidence limit must fail closed");
        assert!(matches!(byte_error, KernelError::DecisionBound(_)));
        assert!(byte_error.to_string().contains("byte capacity"));
        assert_eq!(governed_row_counts(&connection), before);

        let row_error = ensure_write_capacity(
            &connection,
            &[("decision_attempts", MAXIMUM_REPLAY_ROWS_PER_TABLE + 1)],
            0,
        )
        .expect_err("one row over the immutable replay limit must fail closed");
        assert!(matches!(row_error, KernelError::DecisionBound(_)));
        assert!(row_error.to_string().contains("row capacity"));
        assert_eq!(governed_row_counts(&connection), before);
    }

    #[test]
    fn replay_hash_helpers_match_persistence_material() {
        let occurred_at = "2026-07-22T00:00:00Z";
        let attempt_material = decision_attempt_event_material(
            "attempt-1",
            "FAILED",
            Some("BOUNDED_FAILURE"),
            Some("bounded detail"),
            Some("previous-attempt-event"),
            occurred_at,
        );
        assert_eq!(
            decision_attempt_event_hash(
                "attempt-1",
                "FAILED",
                Some("BOUNDED_FAILURE"),
                Some("bounded detail"),
                Some("previous-attempt-event"),
                occurred_at,
            )
            .expect("attempt event hash"),
            canonical_hash(&attempt_material).expect("persistence attempt event hash")
        );

        let reevaluation_material = reevaluation_event_material(
            "reevaluation-1",
            "INTEGRITY_BLOCKED",
            Some("DECISION_INTEGRITY_FAILED"),
            Some("bounded detail"),
            Some("previous-reevaluation-event"),
            occurred_at,
        );
        assert_eq!(
            reevaluation_event_hash(
                "reevaluation-1",
                "INTEGRITY_BLOCKED",
                Some("DECISION_INTEGRITY_FAILED"),
                Some("bounded detail"),
                Some("previous-reevaluation-event"),
                occurred_at,
            )
            .expect("re-evaluation event hash"),
            canonical_hash(&reevaluation_material).expect("persistence re-evaluation event hash")
        );

        let identity_material = reevaluation_identity_material(
            "attempt-1",
            "overlay-1",
            "validation-1",
            "issue-1",
            "rule-set-hash",
            "engine-version",
            "policy-hash",
            "scope-hash",
        );
        assert_eq!(
            reevaluation_identity_hash(
                "attempt-1",
                "overlay-1",
                "validation-1",
                "issue-1",
                "rule-set-hash",
                "engine-version",
                "policy-hash",
                "scope-hash",
            )
            .expect("re-evaluation identity hash"),
            canonical_hash(&identity_material).expect("persistence identity hash")
        );
    }
}

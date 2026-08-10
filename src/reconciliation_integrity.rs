//! Bounded, read-only integrity replay for WP-006 reconciliation and approval.
//!
//! The verifier rebuilds the compact, value-free reconciliation evidence from
//! `SQLite`, replays every canonical hash and predecessor chain, and confirms
//! the append-only guards before any persisted result can be treated as local
//! readiness authority. It deliberately never reads source/effective values.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{CANONICAL_JSON_VERSION, canonical_hash, canonical_json_string, parse_unique_json},
    reconciliation_approval::{
        AcceptedRowIdentity, CategoryCounts, EvidenceIdentity, GoverningIdentities,
        MigrationResultCandidate, RECONCILIATION_ENGINE_VERSION, RECONCILIATION_POLICY_IDENTITY,
        ReconciliationCategory, ReconciliationPolicyV1, ReconciliationSnapshot,
        ReconciliationSnapshotDraft, ResultAction, RowClassification, VersionedEvidenceIdentity,
        canonical_policy_hash,
    },
};

const REQUIRED_SCHEMA_VERSION: i64 = crate::state::SCHEMA_VERSION;
const MAXIMUM_REPLAY_ROWS_PER_TABLE: u64 = 1_000_000;
const MAXIMUM_REPLAY_EVIDENCE_BYTES: u64 = 1024 * 1024 * 1024;

const ATTEMPT_EVENT_CONTRACT: &str = "migration-stream/reconciliation-attempt-event/v1";
const ROW_IDENTITY_CONTRACT: &str = "migration-stream/reconciliation-row-identity/v1";
const LINEAGE_REFERENCES_CONTRACT: &str = "migration-stream/reconciliation-lineage-references/v1";
const ROW_CLASSIFICATION_CONTRACT: &str = "migration-stream/reconciliation-row-classification/v1";
const ROW_SET_CONTRACT: &str = "migration-stream/reconciliation-classification-row-set/v1";
const CATEGORY_COUNTS_CONTRACT: &str = "migration-stream/reconciliation-category-counts/v1";
const CODE_SET_CONTRACT: &str = "migration-stream/reconciliation-code-list/v1";
const EVIDENCE_SET_CONTRACT: &str = "migration-stream/reconciliation-evidence-set/v1";

const POLICY_ACTIVATION_CONTRACT: &str = "migration-stream/reconciliation-policy-activation/v1";

const IMMUTABLE_TABLES: [(&str, &str); 10] = [
    (
        "reconciliation_policy_documents",
        "reconciliation policy history is immutable",
    ),
    (
        "reconciliation_policy_activations",
        "reconciliation policy activation history is immutable",
    ),
    (
        "reconciliation_attempts",
        "reconciliation attempt history is immutable",
    ),
    (
        "reconciliation_attempt_events",
        "reconciliation attempt event history is immutable",
    ),
    (
        "reconciliation_snapshots",
        "reconciliation snapshot history is immutable",
    ),
    (
        "reconciliation_category_counts",
        "reconciliation category-count history is immutable",
    ),
    (
        "reconciliation_row_classifications",
        "reconciliation row-classification history is immutable",
    ),
    (
        "reconciliation_row_lineage_references",
        "reconciliation row-lineage history is immutable",
    ),
    (
        "migration_result_candidates",
        "migration-result candidate history is immutable",
    ),
    (
        "migration_result_actions",
        "migration-result action history is immutable",
    ),
];

const REQUIRED_INDEXES: [(&str, &str); 17] = [
    (
        "reconciliation_policy_activation_idempotency",
        "reconciliation_policy_activations",
    ),
    (
        "reconciliation_policy_activation_history",
        "reconciliation_policy_activations",
    ),
    (
        "reconciliation_attempts_idempotency",
        "reconciliation_attempts",
    ),
    (
        "reconciliation_attempts_job_history",
        "reconciliation_attempts",
    ),
    (
        "reconciliation_attempt_events_history",
        "reconciliation_attempt_events",
    ),
    (
        "reconciliation_snapshots_job_history",
        "reconciliation_snapshots",
    ),
    (
        "reconciliation_rows_category_page",
        "reconciliation_row_classifications",
    ),
    (
        "reconciliation_rows_entity_page",
        "reconciliation_row_classifications",
    ),
    (
        "reconciliation_row_lineage_filter",
        "reconciliation_row_lineage_references",
    ),
    (
        "migration_result_candidates_idempotency",
        "migration_result_candidates",
    ),
    (
        "migration_result_candidates_generation",
        "migration_result_candidates",
    ),
    (
        "migration_result_candidates_job_history",
        "migration_result_candidates",
    ),
    (
        "migration_result_actions_idempotency",
        "migration_result_actions",
    ),
    (
        "migration_result_actions_generation",
        "migration_result_actions",
    ),
    (
        "migration_result_actions_predecessor",
        "migration_result_actions",
    ),
    (
        "migration_result_actions_job_history",
        "migration_result_actions",
    ),
    (
        "migration_result_actions_candidate_history",
        "migration_result_actions",
    ),
];

/// Compact normalized lineage reference persisted beside one classified row.
///
/// Its identity/hash pair is evidence only. It never contains a source or an
/// effective value.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReconciliationLineageReference {
    pub(crate) kind: String,
    pub(crate) identity: String,
    pub(crate) hash: String,
}

/// Shared persistence/replay name for compact lineage evidence.
pub(crate) type LineageReference = ReconciliationLineageReference;

#[derive(Clone)]
struct PolicyEvidence {
    policy: ReconciliationPolicyV1,
    policy_hash: String,
}

#[derive(Clone)]
struct ActivationEvidence {
    policy_identity: String,
    policy_version: String,
    policy_hash: String,
    generation: u64,
    activation_hash: String,
}

#[derive(Clone)]
struct AttemptEvidence {
    job_id: String,
    validation_run_id: String,
    policy_identity: String,
    policy_version: String,
    policy_hash: String,
    policy_activation_id: String,
    policy_activation_hash: String,
    governing_input_hash: String,
}

#[derive(Clone)]
struct AttemptHead {
    state: String,
    rows_processed: u64,
    resulting_snapshot_hash: Option<String>,
}

#[derive(Clone)]
struct SnapshotHeader {
    snapshot_id: String,
    schema_version: i64,
    attempt_id: String,
    state: String,
    job_id: String,
    accepted_source_hash: String,
    accepted_row_set_hash: String,
    accepted_row_count: u64,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    validation_run_id: String,
    validation_result_hash: String,
    active_decision_set_hash: String,
    active_overlay_set_hash: String,
    effective_value_set_hash: String,
    qualifying_reevaluation_set_hash: String,
    policy_identity: String,
    policy_version: String,
    policy_hash: String,
    policy_activation_id: String,
    policy_activation_hash: String,
    engine_version: String,
    canonicalization_version: String,
    governing_input_hash: String,
    category_counts_hash: String,
    warning_codes_json: String,
    warning_codes_hash: String,
    blocker_codes_json: String,
    blocker_codes_hash: String,
    row_set_hash: String,
    snapshot_hash: String,
    completed_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivationMaterial {
    contract: String,
    activation_id: String,
    request_hash: String,
    policy_identity: String,
    policy_version: String,
    policy_hash: String,
    predecessor_activation_id: Option<String>,
    predecessor_generation: u64,
    predecessor_activation_hash: Option<String>,
    generation: u64,
    actor: String,
    reason: String,
    activated_at: String,
}

/// Replay every immutable WP-006 policy, reconciliation, candidate, and action
/// row in one read-only pass.
///
/// # Errors
///
/// Fails closed for a schema/guard alteration, foreign-key break, replay bound,
/// noncanonical JSON, hash mismatch, incomplete snapshot closure, invalid
/// compact lineage, candidate mismatch, or broken action predecessor chain.
pub(crate) fn verify_reconciliation_approval_integrity_connection(
    connection: &Connection,
) -> Result<()> {
    verify_database_preflight(connection)?;
    verify_replay_bounds(connection)?;
    let policies = verify_policy_documents(connection)?;
    let activations = verify_policy_activations(connection, &policies)?;
    let attempts = verify_attempts(connection, &activations)?;
    let attempt_heads = verify_attempt_events(connection, &attempts)?;
    let snapshots = verify_snapshots(connection, &attempts, &attempt_heads)?;
    verify_attempt_snapshot_closure(&attempts, &attempt_heads, &snapshots)?;
    let candidates = verify_candidates(connection, &snapshots)?;
    verify_actions(connection, &snapshots, &candidates)
}

/// Canonical lifecycle-event hash shared by persistence and integrity replay.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconciliation_attempt_event_hash(
    attempt_id: &str,
    state: &str,
    rows_processed: u64,
    error_code: Option<&str>,
    detail: Option<&str>,
    resulting_snapshot_hash: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "attemptId": attempt_id,
        "contract": ATTEMPT_EVENT_CONTRACT,
        "detail": detail,
        "errorCode": error_code,
        "occurredAt": occurred_at,
        "previousEventHash": previous_event_hash,
        "resultingSnapshotHash": resulting_snapshot_hash,
        "rowsProcessed": rows_processed,
        "state": state,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn reconciliation_event_hash(
    attempt_id: &str,
    state: &str,
    rows_processed: u64,
    error_code: Option<&str>,
    detail: Option<&str>,
    resulting_snapshot_hash: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    reconciliation_attempt_event_hash(
        attempt_id,
        state,
        rows_processed,
        error_code,
        detail,
        resulting_snapshot_hash,
        previous_event_hash,
        occurred_at,
    )
}

pub(crate) fn canonical_reconciliation_row_identity_hash(
    row: &AcceptedRowIdentity,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": ROW_IDENTITY_CONTRACT,
        "row": row,
    }))
}

pub(crate) fn canonical_reconciliation_explanation_codes_hash(codes: &[String]) -> Result<String> {
    canonical_reconciliation_code_set_hash("EXPLANATION_CODES", codes)
}

pub(crate) fn canonical_reconciliation_lineage_references_hash(
    references: &[ReconciliationLineageReference],
) -> Result<String> {
    canonical_hash(&json!({
        "contract": LINEAGE_REFERENCES_CONTRACT,
        "references": references,
    }))
}

pub(crate) fn canonical_reconciliation_classification_hash(
    classification: &RowClassification,
) -> Result<String> {
    canonical_hash(&json!({
        "classification": classification,
        "contract": ROW_CLASSIFICATION_CONTRACT,
    }))
}

pub(crate) fn canonical_reconciliation_row_set_hash(rows: &[RowClassification]) -> Result<String> {
    canonical_hash(&json!({
        "contract": ROW_SET_CONTRACT,
        "rows": rows,
    }))
}

pub(crate) fn canonical_reconciliation_category_counts_hash(
    counts: &CategoryCounts,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": CATEGORY_COUNTS_CONTRACT,
        "counts": counts,
    }))
}

pub(crate) fn canonical_reconciliation_code_set_hash(
    kind: &str,
    codes: &[String],
) -> Result<String> {
    canonical_hash(&json!({
        "codes": codes,
        "contract": CODE_SET_CONTRACT,
        "kind": kind,
    }))
}

pub(crate) fn canonical_reconciliation_evidence_set_hash(
    kind: &str,
    evidence: &[EvidenceIdentity],
) -> Result<String> {
    canonical_hash(&json!({
        "contract": EVIDENCE_SET_CONTRACT,
        "evidence": evidence,
        "kind": kind,
    }))
}

pub(crate) fn evidence_set_hash(kind: &str, evidence: &[EvidenceIdentity]) -> Result<String> {
    canonical_reconciliation_evidence_set_hash(kind, evidence)
}

pub(crate) fn category_counts_hash(counts: &CategoryCounts) -> Result<String> {
    canonical_reconciliation_category_counts_hash(counts)
}

pub(crate) fn code_list_hash(kind: &str, codes: &[String]) -> Result<String> {
    canonical_reconciliation_code_set_hash(kind, codes)
}

pub(crate) fn row_identity_hash(row: &AcceptedRowIdentity) -> Result<String> {
    canonical_reconciliation_row_identity_hash(row)
}

pub(crate) fn lineage_references_hash(references: &[LineageReference]) -> Result<String> {
    canonical_reconciliation_lineage_references_hash(references)
}

pub(crate) fn row_classification_hash(classification: &RowClassification) -> Result<String> {
    canonical_reconciliation_classification_hash(classification)
}

pub(crate) fn classification_row_set_hash(rows: &[RowClassification]) -> Result<String> {
    canonical_reconciliation_row_set_hash(rows)
}

fn verify_database_preflight(connection: &Connection) -> Result<()> {
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != REQUIRED_SCHEMA_VERSION {
        return integrity(format!(
            "WP-006 replay requires schema version {REQUIRED_SCHEMA_VERSION}; found {schema_version}"
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
    verify_append_only_schema(connection)
}

fn verify_append_only_schema(connection: &Connection) -> Result<()> {
    for (table, message) in IMMUTABLE_TABLES {
        let table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if !table_exists {
            return integrity(format!("required immutable table {table} is missing"));
        }
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
    for (index, table) in REQUIRED_INDEXES {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema
                 WHERE type = 'index' AND name = ?1 AND tbl_name = ?2
             )",
            params![index, table],
            |row| row.get(0),
        )?;
        if !exists {
            return integrity(format!("required WP-006 index {index} is missing"));
        }
    }
    Ok(())
}

fn verify_replay_bounds(connection: &Connection) -> Result<()> {
    for (table, _) in IMMUTABLE_TABLES {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
        let count = nonnegative(count, &format!("{table} row count"))?;
        if count > MAXIMUM_REPLAY_ROWS_PER_TABLE {
            return integrity(format!(
                "{table} has {count} rows; replay permits {MAXIMUM_REPLAY_ROWS_PER_TABLE}"
            ));
        }
    }
    let evidence_bytes: i64 = connection.query_row(
        "SELECT COALESCE(SUM(bytes), 0) FROM (
             SELECT length(CAST(canonical_policy_json AS BLOB))
                    + length(CAST(provenance_json AS BLOB)) AS bytes
             FROM reconciliation_policy_documents
             UNION ALL
             SELECT length(CAST(canonical_activation_json AS BLOB))
             FROM reconciliation_policy_activations
             UNION ALL
             SELECT length(CAST(canonical_request_json AS BLOB))
             FROM reconciliation_attempts
             UNION ALL
             SELECT length(CAST(warning_codes_json AS BLOB))
                    + length(CAST(blocker_codes_json AS BLOB))
             FROM reconciliation_snapshots
             UNION ALL
             SELECT length(CAST(explanation_codes_json AS BLOB))
                    + length(CAST(lineage_references_json AS BLOB))
             FROM reconciliation_row_classifications
             UNION ALL
             SELECT length(CAST(canonical_candidate_json AS BLOB))
                    + length(CAST(warning_codes_json AS BLOB))
                    + length(CAST(required_acknowledgments_json AS BLOB))
             FROM migration_result_candidates
             UNION ALL
             SELECT length(CAST(canonical_action_json AS BLOB))
                    + length(CAST(warning_acknowledgments_json AS BLOB))
             FROM migration_result_actions
         )",
        [],
        |row| row.get(0),
    )?;
    let evidence_bytes = nonnegative(evidence_bytes, "WP-006 replay evidence bytes")?;
    if evidence_bytes > MAXIMUM_REPLAY_EVIDENCE_BYTES {
        return integrity(format!(
            "WP-006 canonical evidence is {evidence_bytes} bytes; replay permits {MAXIMUM_REPLAY_EVIDENCE_BYTES}"
        ));
    }
    Ok(())
}

fn verify_policy_documents(connection: &Connection) -> Result<HashMap<String, PolicyEvidence>> {
    let mut statement = connection.prepare(
        "SELECT policy_identity, policy_version, schema_version,
                canonicalization_version, engine_version,
                canonical_policy_json, policy_hash,
                provenance_json, provenance_hash,
                registered_at
         FROM reconciliation_policy_documents
         ORDER BY policy_identity, policy_version",
    )?;
    let mut rows = statement.query([])?;
    let mut policies = HashMap::new();
    while let Some(row) = rows.next()? {
        let identity: String = row.get(0)?;
        let version: String = row.get(1)?;
        let schema_version: i64 = row.get(2)?;
        let canonicalization_version: String = row.get(3)?;
        let engine_version: String = row.get(4)?;
        let policy_json: String = row.get(5)?;
        let stored_hash: String = row.get(6)?;
        let provenance_json: String = row.get(7)?;
        let provenance_hash: String = row.get(8)?;
        let registered_at: String = row.get(9)?;
        if schema_version != 1
            || identity != RECONCILIATION_POLICY_IDENTITY
            || version != "1"
            || canonicalization_version != CANONICAL_JSON_VERSION
            || engine_version != RECONCILIATION_ENGINE_VERSION
        {
            return integrity("persisted reconciliation policy has unsupported identity metadata");
        }
        require_timestamp(&registered_at, "policy registration timestamp")?;
        let policy_value = parse_canonical_json(&policy_json, "reconciliation policy")?;
        let policy: ReconciliationPolicyV1 = serde_json::from_value(policy_value)
            .map_err(|error| integrity_error(format!("policy shape is invalid: {error}")))?;
        policy
            .validate()
            .map_err(|error| integrity_error(format!("policy is invalid: {error}")))?;
        let replayed_hash = canonical_policy_hash(&policy)
            .map_err(|error| integrity_error(format!("policy hash replay failed: {error}")))?;
        if stored_hash != replayed_hash {
            return integrity("reconciliation policy hash does not replay");
        }
        let provenance = parse_canonical_json(&provenance_json, "policy provenance")?;
        if canonical_hash(&provenance)? != provenance_hash {
            return integrity("reconciliation policy provenance hash does not replay");
        }
        let key = policy_key(&identity, &version);
        if policies
            .insert(
                key,
                PolicyEvidence {
                    policy,
                    policy_hash: stored_hash,
                },
            )
            .is_some()
        {
            return integrity("duplicate reconciliation policy identity/version");
        }
    }
    Ok(policies)
}

#[allow(clippy::too_many_lines)]
fn verify_policy_activations(
    connection: &Connection,
    policies: &HashMap<String, PolicyEvidence>,
) -> Result<HashMap<String, ActivationEvidence>> {
    let mut statement = connection.prepare(
        "SELECT activation_id, schema_version, policy_identity, policy_version,
                policy_hash, predecessor_activation_id, predecessor_generation,
                predecessor_activation_hash, observed_generation, generation,
                actor, reason, idempotency_key, request_hash,
                canonical_activation_json, activation_hash, activated_at
         FROM reconciliation_policy_activations
         ORDER BY generation, activation_sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut activations: HashMap<String, ActivationEvidence> = HashMap::new();
    let mut previous: Option<(String, u64, String)> = None;
    while let Some(row) = rows.next()? {
        let activation_id: String = row.get(0)?;
        let schema_version: i64 = row.get(1)?;
        let policy_identity: String = row.get(2)?;
        let policy_version: String = row.get(3)?;
        let policy_hash: String = row.get(4)?;
        let predecessor_id: Option<String> = row.get(5)?;
        let predecessor_generation = nonnegative(row.get(6)?, "activation predecessor generation")?;
        let predecessor_hash: Option<String> = row.get(7)?;
        let observed_generation = nonnegative(row.get(8)?, "activation observed generation")?;
        let generation = positive(row.get(9)?, "activation generation")?;
        let actor: String = row.get(10)?;
        let reason: String = row.get(11)?;
        let idempotency_key: String = row.get(12)?;
        let request_hash: String = row.get(13)?;
        let activation_json: String = row.get(14)?;
        let stored_hash: String = row.get(15)?;
        let activated_at: String = row.get(16)?;
        validate_sha256(&request_hash, "policy activation request hash")?;
        validate_token(&idempotency_key, "policy activation idempotency key")?;
        require_timestamp(&activated_at, "policy activation timestamp")?;
        if schema_version != 1 || generation != observed_generation.saturating_add(1) {
            return integrity(format!(
                "policy activation {activation_id} has invalid generation"
            ));
        }
        let policy = policies
            .get(&policy_key(&policy_identity, &policy_version))
            .ok_or_else(|| integrity_error("policy activation references an absent policy"))?;
        if policy.policy_hash != policy_hash
            || policy.policy.identity != policy_identity
            || policy.policy.policy_version.to_string() != policy_version
        {
            return integrity("policy activation does not bind its exact policy document");
        }
        match (&previous, &predecessor_id, &predecessor_hash) {
            (None, None, None)
                if predecessor_generation == 0 && observed_generation == 0 && generation == 1 => {}
            (Some((id, prior_generation, hash)), Some(stored_id), Some(stored_hash))
                if id == stored_id
                    && prior_generation == &predecessor_generation
                    && hash == stored_hash
                    && observed_generation == predecessor_generation
                    && generation == predecessor_generation + 1 => {}
            _ => {
                return integrity(format!(
                    "policy activation {activation_id} has a broken predecessor chain"
                ));
            }
        }
        let activation_value =
            parse_canonical_json(&activation_json, "policy activation material")?;
        if canonical_hash(&activation_value)? != stored_hash {
            return integrity(format!(
                "policy activation {activation_id} hash does not replay"
            ));
        }
        let material: ActivationMaterial = serde_json::from_value(activation_value)
            .map_err(|error| integrity_error(format!("activation shape is invalid: {error}")))?;
        if material.contract != POLICY_ACTIVATION_CONTRACT
            || material.activation_id != activation_id
            || material.request_hash != request_hash
            || material.policy_identity != policy_identity
            || material.policy_version != policy_version
            || material.policy_hash != policy_hash
            || material.predecessor_activation_id != predecessor_id
            || material.predecessor_generation != predecessor_generation
            || material.predecessor_activation_hash != predecessor_hash
            || material.generation != generation
            || material.actor != actor
            || material.reason != reason
            || material.activated_at != activated_at
        {
            return integrity(format!(
                "policy activation {activation_id} canonical fields differ from durable columns"
            ));
        }
        previous = Some((activation_id.clone(), generation, stored_hash.clone()));
        if activations
            .insert(
                activation_id,
                ActivationEvidence {
                    policy_identity,
                    policy_version,
                    policy_hash,
                    generation,
                    activation_hash: stored_hash,
                },
            )
            .is_some()
        {
            return integrity("duplicate policy activation identity");
        }
    }
    Ok(activations)
}

#[allow(clippy::too_many_lines)]
fn verify_attempts(
    connection: &Connection,
    activations: &HashMap<String, ActivationEvidence>,
) -> Result<HashMap<String, AttemptEvidence>> {
    let mut statement = connection.prepare(
        "SELECT attempt_id, schema_version, job_id, validation_run_id,
                policy_identity, policy_version, policy_hash,
                policy_activation_id, policy_activation_hash,
                engine_version, canonicalization_version,
                governing_input_hash, idempotency_key, request_hash,
                canonical_request_json, created_at
         FROM reconciliation_attempts ORDER BY created_at, attempt_id",
    )?;
    let mut rows = statement.query([])?;
    let mut attempts = HashMap::new();
    while let Some(row) = rows.next()? {
        let attempt_id: String = row.get(0)?;
        let schema_version: i64 = row.get(1)?;
        let job_id: String = row.get(2)?;
        let validation_run_id: String = row.get(3)?;
        let policy_identity: String = row.get(4)?;
        let policy_version: String = row.get(5)?;
        let policy_hash: String = row.get(6)?;
        let activation_id: String = row.get(7)?;
        let activation_hash: String = row.get(8)?;
        let engine_version: String = row.get(9)?;
        let canonicalization_version: String = row.get(10)?;
        let governing_input_hash: String = row.get(11)?;
        let idempotency_key: String = row.get(12)?;
        let request_hash: String = row.get(13)?;
        let request_json: String = row.get(14)?;
        let created_at: String = row.get(15)?;
        if schema_version != 1
            || policy_identity != RECONCILIATION_POLICY_IDENTITY
            || policy_version != "1"
            || engine_version != RECONCILIATION_ENGINE_VERSION
            || canonicalization_version != CANONICAL_JSON_VERSION
        {
            return integrity(format!(
                "reconciliation attempt {attempt_id} has invalid metadata"
            ));
        }
        validate_sha256(&governing_input_hash, "attempt governing-input hash")?;
        validate_token(&idempotency_key, "reconciliation idempotency key")?;
        require_timestamp(&created_at, "reconciliation attempt timestamp")?;
        let activation = activations
            .get(&activation_id)
            .ok_or_else(|| integrity_error("attempt references an absent policy activation"))?;
        if activation.policy_identity != policy_identity
            || activation.policy_version != policy_version
            || activation.policy_hash != policy_hash
            || activation.activation_hash != activation_hash
            || activation.generation == 0
        {
            return integrity(format!(
                "reconciliation attempt {attempt_id} policy activation does not replay"
            ));
        }
        let request = parse_canonical_json(&request_json, "reconciliation request")?;
        if canonical_hash(&request)? != request_hash {
            return integrity(format!(
                "reconciliation attempt {attempt_id} request hash differs"
            ));
        }
        require_json_string(&request, &["request", "jobId"], &job_id)?;
        require_json_string(
            &request,
            &["request", "validationRunId"],
            &validation_run_id,
        )?;
        require_json_string(&request, &["request", "idempotencyKey"], &idempotency_key)?;
        require_json_string(&request, &["policyIdentity"], &policy_identity)?;
        require_json_string(&request, &["policyHash"], &policy_hash)?;
        require_json_string(&request, &["policyActivationId"], &activation_id)?;
        require_json_string(&request, &["policyActivationHash"], &activation_hash)?;
        if attempts
            .insert(
                attempt_id,
                AttemptEvidence {
                    job_id,
                    validation_run_id,
                    policy_identity,
                    policy_version,
                    policy_hash,
                    policy_activation_id: activation_id,
                    policy_activation_hash: activation_hash,
                    governing_input_hash,
                },
            )
            .is_some()
        {
            return integrity("duplicate reconciliation attempt identity");
        }
    }
    Ok(attempts)
}

#[allow(clippy::too_many_lines)]
fn verify_attempt_events(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
) -> Result<HashMap<String, AttemptHead>> {
    let mut statement = connection.prepare(
        "SELECT attempt_id, state, rows_processed, error_code, detail,
                resulting_snapshot_hash, previous_event_hash, event_hash, occurred_at
         FROM reconciliation_attempt_events ORDER BY attempt_id, sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut previous: HashMap<String, (String, String, u64)> = HashMap::new();
    let mut heads = HashMap::new();
    while let Some(row) = rows.next()? {
        let attempt_id: String = row.get(0)?;
        let state: String = row.get(1)?;
        let rows_processed = nonnegative(row.get(2)?, "reconciliation rows processed")?;
        let error_code: Option<String> = row.get(3)?;
        let detail: Option<String> = row.get(4)?;
        let resulting_snapshot_hash: Option<String> = row.get(5)?;
        let stored_previous: Option<String> = row.get(6)?;
        let event_hash: String = row.get(7)?;
        let occurred_at: String = row.get(8)?;
        if !attempts.contains_key(&attempt_id) {
            return integrity(format!(
                "event references absent reconciliation attempt {attempt_id}"
            ));
        }
        require_timestamp(&occurred_at, "reconciliation event timestamp")?;
        let expected_previous = previous.get(&attempt_id).map(|value| value.1.as_str());
        if stored_previous.as_deref() != expected_previous {
            return integrity(format!(
                "reconciliation attempt {attempt_id} event chain is broken"
            ));
        }
        let expected_hash = reconciliation_attempt_event_hash(
            &attempt_id,
            &state,
            rows_processed,
            error_code.as_deref(),
            detail.as_deref(),
            resulting_snapshot_hash.as_deref(),
            stored_previous.as_deref(),
            &occurred_at,
        )?;
        if event_hash != expected_hash {
            return integrity(format!(
                "reconciliation attempt {attempt_id} event hash differs"
            ));
        }
        let prior_state = previous.get(&attempt_id).map(|value| value.0.as_str());
        if !valid_attempt_transition(prior_state, &state)
            || previous
                .get(&attempt_id)
                .is_some_and(|value| rows_processed < value.2)
        {
            return integrity(format!(
                "reconciliation attempt {attempt_id} has an invalid lifecycle transition"
            ));
        }
        verify_attempt_event_shape(
            &attempt_id,
            &state,
            error_code.as_deref(),
            resulting_snapshot_hash.as_deref(),
        )?;
        previous.insert(
            attempt_id.clone(),
            (state.clone(), event_hash, rows_processed),
        );
        heads.insert(
            attempt_id,
            AttemptHead {
                state,
                rows_processed,
                resulting_snapshot_hash,
            },
        );
    }
    for attempt_id in attempts.keys() {
        if !heads.contains_key(attempt_id) {
            return integrity(format!(
                "reconciliation attempt {attempt_id} has no lifecycle event"
            ));
        }
    }
    Ok(heads)
}

fn verify_snapshots(
    connection: &Connection,
    attempts: &HashMap<String, AttemptEvidence>,
    attempt_heads: &HashMap<String, AttemptHead>,
) -> Result<HashMap<String, ReconciliationSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT snapshot_id, schema_version, attempt_id, state, job_id,
                accepted_source_hash, accepted_row_set_hash, accepted_row_count,
                profile_id, profile_version, profile_hash,
                mapping_id, mapping_version, mapping_hash,
                validation_run_id, validation_result_hash,
                active_decision_set_hash, active_overlay_set_hash,
                effective_value_set_hash, qualifying_reevaluation_set_hash,
                policy_identity, policy_version, policy_hash,
                policy_activation_id, policy_activation_hash,
                engine_version, canonicalization_version, governing_input_hash,
                category_counts_hash, warning_codes_json, warning_codes_hash,
                blocker_codes_json, blocker_codes_hash,
                row_set_hash, snapshot_hash, completed_at
         FROM reconciliation_snapshots ORDER BY completed_at, snapshot_id",
    )?;
    let mut rows = statement.query([])?;
    let mut snapshots = HashMap::new();
    while let Some(row) = rows.next()? {
        let header = SnapshotHeader {
            snapshot_id: row.get(0)?,
            schema_version: row.get(1)?,
            attempt_id: row.get(2)?,
            state: row.get(3)?,
            job_id: row.get(4)?,
            accepted_source_hash: row.get(5)?,
            accepted_row_set_hash: row.get(6)?,
            accepted_row_count: nonnegative(row.get(7)?, "snapshot accepted row count")?,
            profile_id: row.get(8)?,
            profile_version: row.get(9)?,
            profile_hash: row.get(10)?,
            mapping_id: row.get(11)?,
            mapping_version: row.get(12)?,
            mapping_hash: row.get(13)?,
            validation_run_id: row.get(14)?,
            validation_result_hash: row.get(15)?,
            active_decision_set_hash: row.get(16)?,
            active_overlay_set_hash: row.get(17)?,
            effective_value_set_hash: row.get(18)?,
            qualifying_reevaluation_set_hash: row.get(19)?,
            policy_identity: row.get(20)?,
            policy_version: row.get(21)?,
            policy_hash: row.get(22)?,
            policy_activation_id: row.get(23)?,
            policy_activation_hash: row.get(24)?,
            engine_version: row.get(25)?,
            canonicalization_version: row.get(26)?,
            governing_input_hash: row.get(27)?,
            category_counts_hash: row.get(28)?,
            warning_codes_json: row.get(29)?,
            warning_codes_hash: row.get(30)?,
            blocker_codes_json: row.get(31)?,
            blocker_codes_hash: row.get(32)?,
            row_set_hash: row.get(33)?,
            snapshot_hash: row.get(34)?,
            completed_at: row.get(35)?,
        };
        let snapshot = replay_snapshot(connection, &header, attempts, attempt_heads)?;
        if snapshots.insert(header.snapshot_id, snapshot).is_some() {
            return integrity("duplicate reconciliation snapshot identity");
        }
    }
    Ok(snapshots)
}

#[allow(clippy::too_many_lines)]
fn replay_snapshot(
    connection: &Connection,
    header: &SnapshotHeader,
    attempts: &HashMap<String, AttemptEvidence>,
    attempt_heads: &HashMap<String, AttemptHead>,
) -> Result<ReconciliationSnapshot> {
    if header.schema_version != 1
        || header.state != "COMPLETE"
        || header.policy_identity != RECONCILIATION_POLICY_IDENTITY
        || header.policy_version != "1"
        || header.engine_version != RECONCILIATION_ENGINE_VERSION
        || header.canonicalization_version != CANONICAL_JSON_VERSION
    {
        return integrity(format!(
            "snapshot {} has invalid metadata",
            header.snapshot_id
        ));
    }
    require_timestamp(&header.completed_at, "snapshot completion timestamp")?;
    let attempt = attempts
        .get(&header.attempt_id)
        .ok_or_else(|| integrity_error("snapshot references an absent reconciliation attempt"))?;
    let head = attempt_heads
        .get(&header.attempt_id)
        .ok_or_else(|| integrity_error("snapshot attempt lacks a lifecycle head"))?;
    if attempt.job_id != header.job_id
        || attempt.validation_run_id != header.validation_run_id
        || attempt.policy_identity != header.policy_identity
        || attempt.policy_version != header.policy_version
        || attempt.policy_hash != header.policy_hash
        || attempt.policy_activation_id != header.policy_activation_id
        || attempt.policy_activation_hash != header.policy_activation_hash
        || attempt.governing_input_hash != header.governing_input_hash
        || head.state != "COMPLETE"
        || head.resulting_snapshot_hash.as_deref() != Some(header.snapshot_hash.as_str())
        || head.rows_processed != header.accepted_row_count
    {
        return integrity(format!(
            "snapshot {} does not close its exact attempt",
            header.snapshot_id
        ));
    }
    verify_snapshot_validation_binding(connection, header)?;
    let warning_codes =
        parse_canonical_string_array(&header.warning_codes_json, "snapshot warning codes")?;
    let blocker_codes =
        parse_canonical_string_array(&header.blocker_codes_json, "snapshot blocker codes")?;
    require_sorted_unique(&warning_codes, "snapshot warning codes")?;
    require_sorted_unique(&blocker_codes, "snapshot blocker codes")?;
    if canonical_reconciliation_code_set_hash("WARNING_CODES", &warning_codes)?
        != header.warning_codes_hash
        || canonical_reconciliation_code_set_hash("BLOCKER_CODES", &blocker_codes)?
            != header.blocker_codes_hash
    {
        return integrity(format!(
            "snapshot {} code-set hash differs",
            header.snapshot_id
        ));
    }
    let category_counts = load_category_counts(connection, &header.snapshot_id)?;
    if category_counts.total().map_err(map_domain_integrity)? != header.accepted_row_count
        || canonical_reconciliation_category_counts_hash(&category_counts)?
            != header.category_counts_hash
    {
        return integrity(format!(
            "snapshot {} category counts do not close",
            header.snapshot_id
        ));
    }
    let (classifications, evidence_sets) = load_snapshot_rows(connection, header)?;
    if u64::try_from(classifications.len()).ok() != Some(header.accepted_row_count)
        || canonical_reconciliation_row_set_hash(&classifications)? != header.row_set_hash
    {
        return integrity(format!(
            "snapshot {} row set does not close",
            header.snapshot_id
        ));
    }
    if canonical_reconciliation_evidence_set_hash(
        "ACTIVE_DECISION_HEADS",
        &evidence_sets.decisions,
    )? != header.active_decision_set_hash
        || canonical_reconciliation_evidence_set_hash(
            "ACTIVE_CORRECTION_OVERLAYS",
            &evidence_sets.overlays,
        )? != header.active_overlay_set_hash
        || canonical_reconciliation_evidence_set_hash(
            "EFFECTIVE_CANONICAL_VALUES",
            &evidence_sets.effective_values,
        )? != header.effective_value_set_hash
        || canonical_reconciliation_evidence_set_hash(
            "QUALIFYING_REEVALUATIONS",
            &evidence_sets.reevaluations,
        )? != header.qualifying_reevaluation_set_hash
    {
        return integrity(format!(
            "snapshot {} governing evidence-set hash differs",
            header.snapshot_id
        ));
    }
    let governing = GoverningIdentities {
        schema_version: 1,
        job_id: header.job_id.clone(),
        accepted_source_package: EvidenceIdentity {
            identity: header.job_id.clone(),
            hash: header.accepted_source_hash.clone(),
        },
        accepted_row_set_hash: header.accepted_row_set_hash.clone(),
        accepted_row_count: header.accepted_row_count,
        approved_profile: VersionedEvidenceIdentity {
            identity: header.profile_id.clone(),
            version: header.profile_version.clone(),
            hash: header.profile_hash.clone(),
        },
        source_mapping: VersionedEvidenceIdentity {
            identity: header.mapping_id.clone(),
            version: header.mapping_version.clone(),
            hash: header.mapping_hash.clone(),
        },
        source_validation_run: EvidenceIdentity {
            identity: header.validation_run_id.clone(),
            hash: header.validation_result_hash.clone(),
        },
        active_decision_heads: evidence_sets.decisions,
        active_correction_overlays: evidence_sets.overlays,
        effective_canonical_values: evidence_sets.effective_values,
        qualifying_reevaluations: evidence_sets.reevaluations,
        policy_identity: header.policy_identity.clone(),
        policy_hash: header.policy_hash.clone(),
        engine_version: header.engine_version.clone(),
        canonicalization_version: header.canonicalization_version.clone(),
    };
    let replayed = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
        snapshot_id: header.snapshot_id.clone(),
        attempt_id: header.attempt_id.clone(),
        governing,
        rows: classifications,
        warning_codes,
        blocker_codes,
        completed_at: header.completed_at.clone(),
    })
    .map_err(map_domain_integrity)?;
    if replayed.governing_input_hash != header.governing_input_hash
        || replayed.category_counts != category_counts
        || replayed.snapshot_hash != header.snapshot_hash
    {
        return integrity(format!(
            "snapshot {} canonical hash does not replay",
            header.snapshot_id
        ));
    }
    Ok(replayed)
}

struct GoverningEvidenceSets {
    decisions: Vec<EvidenceIdentity>,
    overlays: Vec<EvidenceIdentity>,
    effective_values: Vec<EvidenceIdentity>,
    reevaluations: Vec<EvidenceIdentity>,
}

#[allow(clippy::too_many_lines)]
fn load_snapshot_rows(
    connection: &Connection,
    header: &SnapshotHeader,
) -> Result<(Vec<RowClassification>, GoverningEvidenceSets)> {
    let mut lineage = load_normalized_lineage(connection, &header.snapshot_id)?;
    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, line_start, line_end,
                row_identity_hash, entity_id, category,
                explanation_codes_json, explanation_codes_hash,
                lineage_references_json, lineage_references_hash,
                classification_hash
         FROM reconciliation_row_classifications
         WHERE snapshot_id = ?1 AND job_id = ?2
         ORDER BY source_file, source_ordinal",
    )?;
    let mut rows = statement.query(params![header.snapshot_id, header.job_id])?;
    let mut classifications = Vec::new();
    let mut decisions = BTreeSet::new();
    let mut overlays = BTreeSet::new();
    let mut effective_values = BTreeSet::new();
    let mut reevaluations = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let source_file: String = row.get(0)?;
        let source_ordinal = positive(row.get(1)?, "classified source ordinal")?;
        let line_start = positive(row.get(2)?, "classified line start")?;
        let line_end = positive(row.get(3)?, "classified line end")?;
        let row_identity_hash: String = row.get(4)?;
        let entity_id: String = row.get(5)?;
        let category = parse_category(&row.get::<_, String>(6)?)?;
        let explanation_json: String = row.get(7)?;
        let explanation_hash: String = row.get(8)?;
        let lineage_json: String = row.get(9)?;
        let lineage_hash: String = row.get(10)?;
        let classification_hash: String = row.get(11)?;
        let identity = AcceptedRowIdentity {
            source_file: source_file.clone(),
            source_ordinal,
            line_start,
            line_end,
            entity_id,
        };
        identity.validate().map_err(map_domain_integrity)?;
        if canonical_reconciliation_row_identity_hash(&identity)? != row_identity_hash {
            return integrity(format!(
                "snapshot {} row identity hash differs at {source_file}:{source_ordinal}",
                header.snapshot_id
            ));
        }
        let explanation_codes =
            parse_canonical_string_array(&explanation_json, "row explanation codes")?;
        require_sorted_unique(&explanation_codes, "row explanation codes")?;
        if canonical_reconciliation_explanation_codes_hash(&explanation_codes)? != explanation_hash
        {
            return integrity("row explanation-code hash does not replay");
        }
        let persisted_references: Vec<ReconciliationLineageReference> =
            parse_canonical_typed_json(&lineage_json, "row lineage references")?;
        require_sorted_unique(&persisted_references, "row lineage references")?;
        let normalized_references = lineage
            .remove(&(source_file.clone(), source_ordinal))
            .unwrap_or_default();
        if persisted_references != normalized_references
            || canonical_reconciliation_lineage_references_hash(&persisted_references)?
                != lineage_hash
        {
            return integrity("row compact lineage differs from normalized lineage evidence");
        }
        let mut lineage_hashes = persisted_references
            .iter()
            .map(|reference| reference.hash.clone())
            .collect::<Vec<_>>();
        lineage_hashes.sort();
        lineage_hashes.dedup();
        if lineage_hashes.len() != persisted_references.len() {
            return integrity("row lineage contains duplicate evidence hashes");
        }
        for reference in &persisted_references {
            verify_lineage_reference(connection, header, &identity, reference)?;
            let evidence = EvidenceIdentity {
                identity: reference.identity.clone(),
                hash: reference.hash.clone(),
            };
            match reference.kind.as_str() {
                "ISSUE_DECISION" => {
                    decisions.insert(evidence);
                }
                "CORRECTION_OVERLAY" => {
                    overlays.insert(evidence);
                }
                "CORRECTION_EFFECTIVATION" => {
                    effective_values.insert(evidence);
                }
                "REEVALUATION_RUN" => {
                    reevaluations.insert(evidence);
                }
                "VALIDATION_RUN" | "VALIDATION_ISSUE" => {}
                _ => return integrity("row lineage contains an unsupported reference kind"),
            }
        }
        let classification = RowClassification {
            row: identity,
            category,
            explanation_codes,
            lineage_hashes,
        };
        classification.validate().map_err(map_domain_integrity)?;
        if canonical_reconciliation_classification_hash(&classification)? != classification_hash {
            return integrity("row classification hash does not replay");
        }
        classifications.push(classification);
    }
    if !lineage.is_empty() {
        return integrity("normalized lineage exists without a classified snapshot row");
    }
    Ok((
        classifications,
        GoverningEvidenceSets {
            decisions: decisions.into_iter().collect(),
            overlays: overlays.into_iter().collect(),
            effective_values: effective_values.into_iter().collect(),
            reevaluations: reevaluations.into_iter().collect(),
        },
    ))
}

fn load_normalized_lineage(
    connection: &Connection,
    snapshot_id: &str,
) -> Result<BTreeMap<(String, u64), Vec<ReconciliationLineageReference>>> {
    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, reference_kind, reference_id, reference_hash
         FROM reconciliation_row_lineage_references
         WHERE snapshot_id = ?1
         ORDER BY source_file, source_ordinal, reference_kind, reference_id",
    )?;
    let mut rows = statement.query([snapshot_id])?;
    let mut grouped: BTreeMap<(String, u64), Vec<ReconciliationLineageReference>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let source_file: String = row.get(0)?;
        let source_ordinal = positive(row.get(1)?, "lineage source ordinal")?;
        let reference = ReconciliationLineageReference {
            kind: row.get(2)?,
            identity: row.get(3)?,
            hash: row.get(4)?,
        };
        validate_lineage_reference(&reference)?;
        grouped
            .entry((source_file, source_ordinal))
            .or_default()
            .push(reference);
    }
    for references in grouped.values() {
        require_sorted_unique(references, "normalized row lineage")?;
    }
    Ok(grouped)
}

#[allow(clippy::too_many_lines)]
fn verify_lineage_reference(
    connection: &Connection,
    header: &SnapshotHeader,
    row: &AcceptedRowIdentity,
    reference: &ReconciliationLineageReference,
) -> Result<()> {
    let matches: bool = match reference.kind.as_str() {
        "VALIDATION_RUN" => {
            reference.identity == header.validation_run_id
                && reference.hash == header.validation_result_hash
        }
        "VALIDATION_ISSUE" => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM validation_issues
                 WHERE run_id = ?1 AND issue_id = ?2 AND issue_hash = ?3
                   AND source_file = ?4 AND source_ordinal = ?5
             )",
            params![
                header.validation_run_id,
                reference.identity,
                reference.hash,
                row.source_file,
                sqlite_u64(row.source_ordinal, "source ordinal")?,
            ],
            |record| record.get(0),
        )?,
        "ISSUE_DECISION" => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM issue_decisions d
                 JOIN validation_issues i
                   ON i.run_id = d.validation_run_id AND i.issue_id = d.issue_id
                 WHERE d.validation_run_id = ?1
                   AND d.decision_id = ?2 AND d.decision_hash = ?3
                   AND i.source_file = ?4 AND i.source_ordinal = ?5
             )",
            params![
                header.validation_run_id,
                reference.identity,
                reference.hash,
                row.source_file,
                sqlite_u64(row.source_ordinal, "source ordinal")?,
            ],
            |record| record.get(0),
        )?,
        "CORRECTION_OVERLAY" => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM correction_overlays o
                 JOIN issue_decisions d ON d.correction_overlay_id = o.overlay_id
                 JOIN validation_issues i
                   ON i.run_id = d.validation_run_id AND i.issue_id = d.issue_id
                 WHERE d.validation_run_id = ?1
                   AND o.overlay_id = ?2 AND o.overlay_hash = ?3
                   AND i.source_file = ?4 AND i.source_ordinal = ?5
             )",
            params![
                header.validation_run_id,
                reference.identity,
                reference.hash,
                row.source_file,
                sqlite_u64(row.source_ordinal, "source ordinal")?,
            ],
            |record| record.get(0),
        )?,
        "CORRECTION_EFFECTIVATION" => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM correction_effectivations e
                 JOIN issue_decisions d ON d.decision_id = e.decision_id
                 JOIN validation_issues i
                   ON i.run_id = d.validation_run_id AND i.issue_id = d.issue_id
                 WHERE d.validation_run_id = ?1
                   AND e.effectivation_id = ?2 AND e.effective_value_hash = ?3
                   AND i.source_file = ?4 AND i.source_ordinal = ?5
             )",
            params![
                header.validation_run_id,
                reference.identity,
                reference.hash,
                row.source_file,
                sqlite_u64(row.source_ordinal, "source ordinal")?,
            ],
            |record| record.get(0),
        )?,
        "REEVALUATION_RUN" => connection.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM reevaluation_runs r
                 WHERE r.source_validation_run_id = ?1
                   AND r.reevaluation_id = ?2 AND r.terminal_hash = ?3
                   AND r.state = 'COMPLETE'
                   AND (
                       EXISTS(
                           SELECT 1 FROM validation_issues i
                           WHERE i.run_id = r.source_validation_run_id
                             AND i.issue_id = r.source_issue_id
                             AND i.source_file = ?4 AND i.source_ordinal = ?5
                       )
                       OR EXISTS(
                           SELECT 1 FROM reevaluation_issues ri
                           WHERE ri.reevaluation_id = r.reevaluation_id
                             AND ri.source_file = ?4 AND ri.source_ordinal = ?5
                       )
                   )
             )",
            params![
                header.validation_run_id,
                reference.identity,
                reference.hash,
                row.source_file,
                sqlite_u64(row.source_ordinal, "source ordinal")?,
            ],
            |record| record.get(0),
        )?,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        integrity(format!(
            "{} lineage {} does not bind exact immutable row evidence",
            reference.kind, reference.identity
        ))
    }
}

fn verify_snapshot_validation_binding(
    connection: &Connection,
    header: &SnapshotHeader,
) -> Result<()> {
    let matches: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM validation_runs
             WHERE id = ?1 AND job_id = ?2
               AND profile_id = ?3 AND profile_version = ?4 AND profile_hash = ?5
               AND mapping_id = ?6 AND mapping_version = ?7 AND mapping_hash = ?8
               AND source_hash = ?9 AND result_hash = ?10 AND state = 'COMPLETE'
         )",
        params![
            header.validation_run_id,
            header.job_id,
            header.profile_id,
            header.profile_version,
            header.profile_hash,
            header.mapping_id,
            header.mapping_version,
            header.mapping_hash,
            header.accepted_source_hash,
            header.validation_result_hash,
        ],
        |row| row.get(0),
    )?;
    if matches {
        Ok(())
    } else {
        integrity(format!(
            "snapshot {} validation authority does not replay",
            header.snapshot_id
        ))
    }
}

fn load_category_counts(connection: &Connection, snapshot_id: &str) -> Result<CategoryCounts> {
    let mut statement = connection.prepare(
        "SELECT category, row_count FROM reconciliation_category_counts
         WHERE snapshot_id = ?1 ORDER BY category",
    )?;
    let mut rows = statement.query([snapshot_id])?;
    let mut seen = HashSet::new();
    let mut counts = CategoryCounts::default();
    while let Some(row) = rows.next()? {
        let category = parse_category(&row.get::<_, String>(0)?)?;
        let count = nonnegative(row.get(1)?, "reconciliation category count")?;
        if !seen.insert(category) {
            return integrity("duplicate reconciliation category count");
        }
        match category {
            ReconciliationCategory::IncludedValid => counts.included_valid = count,
            ReconciliationCategory::IncludedCorrected => counts.included_corrected = count,
            ReconciliationCategory::IncludedWaived => counts.included_waived = count,
            ReconciliationCategory::IncludedAcceptedAsIs => {
                counts.included_accepted_as_is = count;
            }
            ReconciliationCategory::Excluded => counts.excluded = count,
            ReconciliationCategory::BlockedUnresolved => counts.blocked_unresolved = count,
            ReconciliationCategory::BlockedIncomplete => counts.blocked_incomplete = count,
            ReconciliationCategory::BlockedConflict => counts.blocked_conflict = count,
        }
    }
    if seen.len() != ReconciliationCategory::all().len() {
        return integrity("reconciliation category counts do not contain all eight categories");
    }
    Ok(counts)
}

fn verify_attempt_snapshot_closure(
    attempts: &HashMap<String, AttemptEvidence>,
    heads: &HashMap<String, AttemptHead>,
    snapshots: &HashMap<String, ReconciliationSnapshot>,
) -> Result<()> {
    let mut snapshot_attempts = HashSet::new();
    for snapshot in snapshots.values() {
        if !snapshot_attempts.insert(snapshot.attempt_id.clone()) {
            return integrity("one reconciliation attempt owns multiple snapshots");
        }
    }
    for attempt_id in attempts.keys() {
        let head = heads
            .get(attempt_id)
            .ok_or_else(|| integrity_error("reconciliation attempt has no event head"))?;
        if (head.state == "COMPLETE") != snapshot_attempts.contains(attempt_id) {
            return integrity(format!(
                "reconciliation attempt {attempt_id} terminal state and snapshot presence differ"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_candidates(
    connection: &Connection,
    snapshots: &HashMap<String, ReconciliationSnapshot>,
) -> Result<HashMap<String, MigrationResultCandidate>> {
    let mut statement = connection.prepare(
        "SELECT candidate_id, schema_version, job_id, snapshot_id, snapshot_hash,
                governing_input_hash, policy_identity, policy_version, policy_hash,
                engine_version, canonicalization_version,
                accepted_row_count, included_count, excluded_count, blocked_count,
                warning_codes_json, warning_codes_hash,
                required_acknowledgments_json, required_acknowledgments_hash,
                creator, preparation_reason, creation_generation,
                idempotency_key, request_hash,
                canonical_candidate_json, candidate_hash, created_at
         FROM migration_result_candidates
         ORDER BY job_id, creation_generation, candidate_sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut candidates = HashMap::new();
    let mut generations: HashMap<String, u64> = HashMap::new();
    while let Some(row) = rows.next()? {
        let candidate_id: String = row.get(0)?;
        let schema_version: i64 = row.get(1)?;
        let job_id: String = row.get(2)?;
        let snapshot_id: String = row.get(3)?;
        let snapshot_hash: String = row.get(4)?;
        let governing_input_hash: String = row.get(5)?;
        let policy_identity: String = row.get(6)?;
        let policy_version: String = row.get(7)?;
        let policy_hash: String = row.get(8)?;
        let engine_version: String = row.get(9)?;
        let canonicalization_version: String = row.get(10)?;
        let accepted_count = nonnegative(row.get(11)?, "candidate accepted count")?;
        let included_count = nonnegative(row.get(12)?, "candidate included count")?;
        let excluded_count = nonnegative(row.get(13)?, "candidate excluded count")?;
        let blocked_count = nonnegative(row.get(14)?, "candidate blocked count")?;
        let warning_json: String = row.get(15)?;
        let warning_hash: String = row.get(16)?;
        let required_json: String = row.get(17)?;
        let required_hash: String = row.get(18)?;
        let creator: String = row.get(19)?;
        let preparation_reason: String = row.get(20)?;
        let generation = positive(row.get(21)?, "candidate generation")?;
        let idempotency_key: String = row.get(22)?;
        let request_hash: String = row.get(23)?;
        let candidate_json: String = row.get(24)?;
        let stored_hash: String = row.get(25)?;
        let created_at: String = row.get(26)?;
        validate_token(&idempotency_key, "candidate idempotency key")?;
        validate_sha256(&request_hash, "candidate request hash")?;
        let expected_generation = generations.entry(job_id.clone()).or_default();
        *expected_generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| integrity_error("candidate generation overflow"))?;
        if generation != *expected_generation || schema_version != 1 || policy_version != "1" {
            return integrity(format!(
                "candidate {candidate_id} generation/metadata is invalid"
            ));
        }
        let value = parse_canonical_json(&candidate_json, "migration-result candidate")?;
        let candidate: MigrationResultCandidate = deserialize_record(value, "candidate")?;
        candidate
            .validate()
            .map_err(|error| integrity_error(format!("candidate integrity failed: {error}")))?;
        let expected_json = canonical_json_string(&serde_json::to_value(&candidate)?)?;
        if expected_json != candidate_json
            || candidate.candidate_id != candidate_id
            || candidate.candidate_hash != stored_hash
            || candidate.job_id != job_id
            || candidate.snapshot_id != snapshot_id
            || candidate.snapshot_hash != snapshot_hash
            || candidate.governing_input_hash != governing_input_hash
            || candidate.policy_identity != policy_identity
            || candidate.policy_hash != policy_hash
            || candidate.engine_version != engine_version
            || candidate.canonicalization_version != canonicalization_version
            || candidate.counts.included != included_count
            || candidate.counts.excluded != excluded_count
            || candidate.counts.blocked != blocked_count
            || candidate.governing.accepted_row_count != accepted_count
            || candidate.practitioner_id != creator
            || candidate.preparation_reason != preparation_reason
            || candidate.creation_generation != generation
            || candidate.created_at != created_at
        {
            return integrity(format!(
                "candidate {candidate_id} differs from durable columns"
            ));
        }
        let warnings = parse_canonical_string_array(&warning_json, "candidate warning codes")?;
        let required =
            parse_canonical_string_array(&required_json, "candidate required acknowledgements")?;
        if warnings != candidate.warning_codes
            || required != candidate.required_acknowledgement_codes
            || canonical_reconciliation_code_set_hash("WARNING_CODES", &warnings)? != warning_hash
            || canonical_reconciliation_code_set_hash("REQUIRED_ACKNOWLEDGEMENT_CODES", &required)?
                != required_hash
        {
            return integrity(format!("candidate {candidate_id} code evidence differs"));
        }
        let snapshot = snapshots
            .get(&snapshot_id)
            .ok_or_else(|| integrity_error("candidate references an absent snapshot"))?;
        candidate
            .validate_against_snapshot(snapshot)
            .map_err(|error| {
                integrity_error(format!("candidate snapshot binding failed: {error}"))
            })?;
        if candidates.insert(candidate_id, candidate).is_some() {
            return integrity("duplicate migration-result candidate identity");
        }
    }
    Ok(candidates)
}

#[allow(clippy::too_many_lines)]
fn verify_actions(
    connection: &Connection,
    snapshots: &HashMap<String, ReconciliationSnapshot>,
    candidates: &HashMap<String, MigrationResultCandidate>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT action_id, schema_version, job_id, candidate_id, candidate_hash,
                snapshot_id, snapshot_hash, action_kind,
                predecessor_action_id, predecessor_generation,
                observed_generation, generation,
                actor, reason,
                warning_acknowledgments_json, warning_acknowledgments_hash,
                authority_boundary_acknowledged, idempotency_key, request_hash,
                canonical_action_json, action_hash, occurred_at
         FROM migration_result_actions
         ORDER BY job_id, generation, action_sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut actions: HashMap<String, (String, ResultAction)> = HashMap::new();
    let mut generations: HashMap<String, u64> = HashMap::new();
    let mut latest_by_job: HashMap<String, String> = HashMap::new();
    while let Some(row) = rows.next()? {
        let action_id: String = row.get(0)?;
        let schema_version: i64 = row.get(1)?;
        let job_id: String = row.get(2)?;
        let candidate_id: String = row.get(3)?;
        let candidate_hash: String = row.get(4)?;
        let snapshot_id: String = row.get(5)?;
        let snapshot_hash: String = row.get(6)?;
        let action_kind: String = row.get(7)?;
        let predecessor_id: Option<String> = row.get(8)?;
        let predecessor_generation = nonnegative(row.get(9)?, "action predecessor generation")?;
        let observed_generation = nonnegative(row.get(10)?, "action observed generation")?;
        let generation = positive(row.get(11)?, "action generation")?;
        let actor: String = row.get(12)?;
        let reason: String = row.get(13)?;
        let acknowledgements_json: String = row.get(14)?;
        let acknowledgements_hash: String = row.get(15)?;
        let boundary_acknowledged: i64 = row.get(16)?;
        let idempotency_key: String = row.get(17)?;
        let request_hash: String = row.get(18)?;
        let action_json: String = row.get(19)?;
        let stored_hash: String = row.get(20)?;
        let occurred_at: String = row.get(21)?;
        let expected_generation = generations.entry(job_id.clone()).or_default();
        *expected_generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| integrity_error("result-action generation overflow"))?;
        if schema_version != 1
            || generation != *expected_generation
            || generation != observed_generation.saturating_add(1)
            || predecessor_generation != observed_generation
            || boundary_acknowledged != 1
        {
            return integrity(format!(
                "result action {action_id} has invalid generation/metadata"
            ));
        }
        validate_token(&idempotency_key, "result-action idempotency key")?;
        let value = parse_canonical_json(&action_json, "migration-result action")?;
        let action: ResultAction = deserialize_record(value, "action")?;
        action
            .validate()
            .map_err(|error| integrity_error(format!("result-action integrity failed: {error}")))?;
        let expected_json = canonical_json_string(&serde_json::to_value(&action)?)?;
        if expected_json != action_json
            || action.action_id != action_id
            || action.action_hash != stored_hash
            || action.payload.kind.as_str() != action_kind
            || action.payload.candidate_id != candidate_id
            || action.payload.candidate_hash != candidate_hash
            || action.payload.snapshot_id != snapshot_id
            || action.payload.snapshot_hash != snapshot_hash
            || action.payload.predecessor_action_id != predecessor_id
            || action.payload.observed_generation != observed_generation
            || action.generation != generation
            || action.payload.actor != actor
            || action.payload.reason != reason
            || action.payload.authority_boundary_acknowledged != (boundary_acknowledged == 1)
            || action.payload.idempotency_key != idempotency_key
            || action.payload_hash != request_hash
            || action.action_timestamp != occurred_at
        {
            return integrity(format!(
                "result action {action_id} differs from durable columns"
            ));
        }
        let acknowledgements = parse_canonical_string_array(
            &acknowledgements_json,
            "result-action warning acknowledgements",
        )?;
        if acknowledgements != action.payload.warning_acknowledgements
            || canonical_reconciliation_code_set_hash(
                "WARNING_ACKNOWLEDGEMENTS",
                &acknowledgements,
            )? != acknowledgements_hash
        {
            return integrity(format!(
                "result action {action_id} warning evidence differs"
            ));
        }
        let candidate = candidates
            .get(&candidate_id)
            .ok_or_else(|| integrity_error("result action references an absent candidate"))?;
        let snapshot = snapshots
            .get(&snapshot_id)
            .ok_or_else(|| integrity_error("result action references an absent snapshot"))?;
        action
            .validate_binding(candidate, snapshot)
            .map_err(|error| integrity_error(format!("result-action binding failed: {error}")))?;
        match predecessor_id.as_deref() {
            None => {
                if predecessor_generation != 0
                    || action.payload.predecessor_action_hash.is_some()
                    || !matches!(action_kind.as_str(), "APPROVED" | "REJECTED")
                {
                    return integrity(format!(
                        "result action {action_id} has invalid chain origin"
                    ));
                }
            }
            Some(predecessor_id) => {
                let (predecessor_job, predecessor) = actions
                    .get(predecessor_id)
                    .ok_or_else(|| integrity_error("result-action predecessor is absent"))?;
                let predecessor_is_active = matches!(
                    predecessor.payload.kind.as_str(),
                    "APPROVED" | "APPROVAL_SUPERSEDED"
                );
                let successor_kind_is_valid = if predecessor_is_active {
                    matches!(
                        action_kind.as_str(),
                        "APPROVAL_REVOKED" | "APPROVAL_SUPERSEDED"
                    )
                } else {
                    matches!(action_kind.as_str(), "APPROVED" | "REJECTED")
                };
                if predecessor_job != &job_id
                    || latest_by_job.get(&job_id).map(String::as_str) != Some(predecessor_id)
                    || predecessor.generation != predecessor_generation
                    || action.payload.predecessor_action_hash.as_deref()
                        != Some(predecessor.action_hash.as_str())
                    || !successor_kind_is_valid
                    || (action_kind == "APPROVAL_REVOKED"
                        && action.payload.candidate_id != predecessor.payload.candidate_id)
                    || (action_kind == "APPROVAL_SUPERSEDED"
                        && action.payload.candidate_id == predecessor.payload.candidate_id)
                {
                    return integrity(format!(
                        "result action {action_id} predecessor semantics fail"
                    ));
                }
            }
        }
        latest_by_job.insert(job_id.clone(), action_id.clone());
        if actions.insert(action_id, (job_id, action)).is_some() {
            return integrity("duplicate migration-result action identity");
        }
    }
    Ok(())
}

fn valid_attempt_transition(previous: Option<&str>, current: &str) -> bool {
    match (previous, current) {
        (None, "PLANNED") | (Some("PLANNED"), "RUNNING") => true,
        (Some("RUNNING"), state) => matches!(
            state,
            "COMPLETE"
                | "INTERRUPTED"
                | "FAILED"
                | "BOUND_EXCEEDED"
                | "STALE_INPUT"
                | "INTEGRITY_BLOCKED"
                | "CONFLICTED"
        ),
        _ => false,
    }
}

fn verify_attempt_event_shape(
    attempt_id: &str,
    state: &str,
    error_code: Option<&str>,
    snapshot_hash: Option<&str>,
) -> Result<()> {
    let valid = match state {
        "PLANNED" | "RUNNING" => error_code.is_none() && snapshot_hash.is_none(),
        "COMPLETE" => error_code.is_none() && snapshot_hash.is_some(),
        "INTERRUPTED" | "FAILED" | "BOUND_EXCEEDED" | "STALE_INPUT" | "INTEGRITY_BLOCKED"
        | "CONFLICTED" => error_code.is_some() && snapshot_hash.is_none(),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        integrity(format!(
            "reconciliation attempt {attempt_id} event has an invalid state/result shape"
        ))
    }
}

fn parse_category(value: &str) -> Result<ReconciliationCategory> {
    ReconciliationCategory::all()
        .into_iter()
        .find(|category| category.as_str() == value)
        .ok_or_else(|| integrity_error(format!("unsupported reconciliation category {value}")))
}

fn parse_canonical_json(raw: &str, label: &str) -> Result<Value> {
    let value = parse_unique_json(raw.as_bytes())
        .map_err(|error| integrity_error(format!("{label} is not strict JSON: {error}")))?;
    if canonical_json_string(&value)? != raw {
        return integrity(format!("{label} is not canonical JSON v1"));
    }
    Ok(value)
}

fn parse_canonical_typed_json<T: DeserializeOwned>(raw: &str, label: &str) -> Result<T> {
    let value = parse_canonical_json(raw, label)?;
    serde_json::from_value(value)
        .map_err(|error| integrity_error(format!("{label} shape is invalid: {error}")))
}

fn parse_canonical_string_array(raw: &str, label: &str) -> Result<Vec<String>> {
    parse_canonical_typed_json(raw, label)
}

fn deserialize_record<T: DeserializeOwned>(mut value: Value, nested_key: &str) -> Result<T> {
    if let Some(nested) = value
        .as_object_mut()
        .and_then(|object| object.remove(nested_key))
    {
        value = nested;
    }
    serde_json::from_value(value).map_err(|error| {
        integrity_error(format!("canonical {nested_key} shape is invalid: {error}"))
    })
}

fn require_json_string(value: &Value, path: &[&str], expected: &str) -> Result<()> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| integrity_error(format!("canonical JSON lacks {}", path.join("."))))?;
    }
    if current.as_str() == Some(expected) {
        Ok(())
    } else {
        integrity(format!(
            "canonical JSON field {} differs from durable evidence",
            path.join(".")
        ))
    }
}

fn validate_lineage_reference(reference: &ReconciliationLineageReference) -> Result<()> {
    if !matches!(
        reference.kind.as_str(),
        "VALIDATION_RUN"
            | "VALIDATION_ISSUE"
            | "ISSUE_DECISION"
            | "CORRECTION_OVERLAY"
            | "CORRECTION_EFFECTIVATION"
            | "REEVALUATION_RUN"
    ) {
        return integrity("unsupported compact lineage reference kind");
    }
    validate_token(&reference.identity, "lineage reference identity")?;
    validate_sha256(&reference.hash, "lineage reference hash")
}

fn require_sorted_unique<T: Ord>(values: &[T], label: &str) -> Result<()> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return integrity(format!("{label} is not strictly sorted and unique"));
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return integrity(format!("{label} is not a bounded safe internal token"));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return integrity(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

fn require_timestamp(value: &str, label: &str) -> Result<()> {
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|error| integrity_error(format!("{label} is invalid: {error}")))
}

fn policy_key(identity: &str, version: &str) -> String {
    format!("{identity}\u{1f}{version}")
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

fn positive(value: i64, label: &str) -> Result<u64> {
    let value = nonnegative(value, label)?;
    if value == 0 {
        return integrity(format!("{label} must be positive"));
    }
    Ok(value)
}

fn sqlite_u64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| integrity_error(format!("{label} exceeds SQLite INTEGER")))
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` owns the domain error.
fn map_domain_integrity(error: KernelError) -> KernelError {
    integrity_error(error.to_string())
}

fn integrity<T>(detail: impl Into<String>) -> Result<T> {
    Err(integrity_error(detail))
}

fn integrity_error(detail: impl Into<String>) -> KernelError {
    KernelError::ResultApprovalIntegrity(detail.into())
}

#[cfg(test)]
mod tests {
    use super::{
        ReconciliationLineageReference, canonical_reconciliation_lineage_references_hash,
        reconciliation_attempt_event_hash, verify_reconciliation_approval_integrity_connection,
    };
    use crate::{KernelError, state::WorkspaceWriter};

    #[test]
    fn event_hash_is_deterministic_and_state_sensitive() {
        let first = reconciliation_attempt_event_hash(
            "attempt-1",
            "RUNNING",
            0,
            None,
            None,
            None,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "2026-07-22T00:00:00Z",
        )
        .expect("event hash");
        let second = reconciliation_attempt_event_hash(
            "attempt-1",
            "RUNNING",
            0,
            None,
            None,
            None,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "2026-07-22T00:00:00Z",
        )
        .expect("event hash");
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn compact_lineage_hash_contains_no_values() {
        let references = vec![ReconciliationLineageReference {
            kind: "VALIDATION_RUN".to_owned(),
            identity: "validation-1".to_owned(),
            hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        }];
        let hash =
            canonical_reconciliation_lineage_references_hash(&references).expect("lineage hash");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn empty_v7_workspace_and_append_only_guards_replay() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("v7 workspace");
        verify_reconciliation_approval_integrity_connection(&writer.connection)
            .expect("empty WP-006 replay");

        writer
            .connection
            .execute_batch("DROP TRIGGER migration_result_actions_no_update;")
            .expect("remove append-only guard");
        let error = verify_reconciliation_approval_integrity_connection(&writer.connection)
            .expect_err("altered guard must fail closed");
        assert!(matches!(error, KernelError::ResultApprovalIntegrity(_)));
    }
}

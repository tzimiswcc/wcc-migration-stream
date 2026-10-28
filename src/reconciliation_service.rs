//! WP-006 decision-aware reconciliation and local result-approval service.
//!
//! This is the only application write boundary for reconciliation snapshots,
//! migration-result candidates, and append-only result actions. It consumes
//! accepted source and WP-003/WP-005 authority without changing that history.
//! No operation in this module publishes data or grants external target-system authority.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{
        CANONICAL_JSON_VERSION, canonical_hash, canonical_json_string, parse_unique_json,
        sha256_hex,
    },
    mapping::load_stored_mapping,
    reconciliation_approval::{
        AcceptedRowIdentity, ApprovalEligibility, CandidateCurrentness, CategoryCounts,
        EvidenceIdentity, GoverningIdentities, MigrationResultCandidate,
        MigrationResultCandidateDraft, RECONCILIATION_ENGINE_VERSION,
        RECONCILIATION_POLICY_IDENTITY, RECONCILIATION_SCHEMA_VERSION, ReconciliationAttemptState,
        ReconciliationCategory, ReconciliationPolicyV1, ReconciliationSnapshot,
        ReconciliationSnapshotDraft, ResultAction, ResultActionKind, ResultActionPayload,
        RowClassification, VersionedEvidenceIdentity, canonical_accepted_row_set_hash,
        canonical_action_payload_hash, canonical_governing_input_hash, canonical_policy_hash,
        evaluate_approval_eligibility,
    },
    reconciliation_integrity::{
        LineageReference, canonical_reconciliation_explanation_codes_hash, category_counts_hash,
        classification_row_set_hash, code_list_hash, evidence_set_hash, lineage_references_hash,
        reconciliation_event_hash, row_classification_hash, row_identity_hash,
    },
    state::{WorkspaceWriter, ensure_package_cleanup_not_conflicted, now_rfc3339, sqlite_integer},
    validation::verify_validation_run_for_read,
};

const CALCULATE_REQUEST_CONTRACT: &str = "migration-stream/write/calculate-reconciliation/v1";
const CREATE_CANDIDATE_REQUEST_CONTRACT: &str = "migration-stream/write/create-result-candidate/v1";
const RECONCILIATION_RESULT_SCHEMA: &str =
    "migration-stream/write/reconciliation-calculation-result/v1";
const CANDIDATE_RESULT_SCHEMA: &str = "migration-stream/write/result-candidate-command-result/v1";
const ACTION_RESULT_SCHEMA: &str = "migration-stream/write/result-action-command-result/v1";
const POLICY_PROVENANCE_CONTRACT: &str = "migration-stream/reconciliation-policy-provenance/v2";
const POLICY_ACTIVATION_CONTRACT: &str = "migration-stream/reconciliation-policy-activation/v1";
const POLICY_DOCUMENT_PATH: &str = "docs/RECONCILIATION-POLICY.md";
const POLICY_DOCUMENT_BYTES: &[u8] = include_bytes!("../docs/RECONCILIATION-POLICY.md");
const LEGACY_POLICY_PROVENANCE_TUPLE_SHA256: &str =
    "9ef3e54dd3bb24ab3e7eb6f212c7df35e714fd3063fae96714aa94135b66f48a";
const POLICY_REGISTRAR: &str = "migration-stream/reconciliation-policy-registry/v2";
const POLICY_REGISTRATION_REASON: &str =
    "Register the embedded reconciliation policy for local readiness";
const POLICY_ACTIVATION_REASON: &str =
    "Activate the embedded reconciliation policy for local readiness";
const POLICY_ACTIVATION_IDEMPOTENCY_KEY: &str = "reconciliation-policy-v1-neutral-activation";
const BOUNDARY_ACKNOWLEDGEMENT_VERSION: &str = "migration-stream-result-boundary/v1";
const MAXIMUM_RECONCILIATION_ROWS: usize = 1_000_000;
const MAXIMUM_RECONCILIATION_ISSUES: usize = 1_000_000;
const MAXIMUM_IDENTITY_BYTES: usize = 256;
const MAXIMUM_REASON_BYTES: usize = 4_096;
const MAXIMUM_IDEMPOTENCY_BYTES: usize = 128;
const POLICY_VERSION_TEXT: &str = "1";

/// Strict calculation request; all authority is re-derived by the service.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CalculateReconciliationRequest {
    pub schema_version: String,
    pub job_id: String,
    pub validation_run_id: String,
    pub practitioner_id: String,
    pub reason: String,
    pub idempotency_key: String,
    pub local_readiness_only_acknowledged: bool,
}

impl CalculateReconciliationRequest {
    fn validate(&self) -> Result<()> {
        if self.schema_version != CALCULATE_REQUEST_CONTRACT {
            return invalid("unsupported reconciliation calculation request schema");
        }
        validate_identity(&self.job_id, "job ID")?;
        validate_identity(&self.validation_run_id, "validation run ID")?;
        validate_attribution(&self.practitioner_id, &self.reason)?;
        validate_idempotency(&self.idempotency_key)?;
        if !self.local_readiness_only_acknowledged {
            return invalid(
                "reconciliation must acknowledge that it grants no publication or external target-system authority",
            );
        }
        Ok(())
    }
}

/// Durable calculation result returned only after a committed snapshot replays.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationCommandResult {
    pub schema_version: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub governing_input_hash: String,
    pub accepted_row_count: u64,
    pub included_count: u64,
    pub excluded_count: u64,
    pub blocked_count: u64,
    pub idempotent_replay: bool,
    pub integrity_status: String,
    pub publication_authority: bool,
}

/// Strict immutable-candidate creation request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCandidateRequest {
    pub schema_version: String,
    pub job_id: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub practitioner_id: String,
    pub reason: String,
    pub idempotency_key: String,
    pub local_readiness_only_acknowledged: bool,
}

impl CreateCandidateRequest {
    fn validate(&self) -> Result<()> {
        if self.schema_version != CREATE_CANDIDATE_REQUEST_CONTRACT {
            return invalid("unsupported result-candidate request schema");
        }
        validate_identity(&self.job_id, "job ID")?;
        validate_identity(&self.snapshot_id, "snapshot ID")?;
        validate_sha256(&self.snapshot_hash, "snapshot hash")?;
        validate_attribution(&self.practitioner_id, &self.reason)?;
        validate_idempotency(&self.idempotency_key)?;
        if !self.local_readiness_only_acknowledged {
            return invalid(
                "candidate creation must acknowledge that it grants no approval or publication authority",
            );
        }
        Ok(())
    }
}

/// Durable candidate result returned after canonical replay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct CandidateCommandResult {
    pub schema_version: String,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub generation: u64,
    pub current: bool,
    pub eligible: bool,
    pub reason_codes: Vec<String>,
    pub idempotent_replay: bool,
    pub integrity_status: String,
    pub approval_authority: bool,
    pub publication_authority: bool,
}

/// Durable append-only action result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct ResultActionCommandResult {
    pub schema_version: String,
    pub action_id: String,
    pub action_kind: String,
    pub action_hash: String,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub generation: u64,
    pub active: bool,
    pub idempotent_replay: bool,
    pub integrity_status: String,
    pub publication_authority: bool,
    pub authenticated_signature: bool,
}

/// Transport-bound fields used only to replay an already committed action.
///
/// This probe does not authorize a new action. It permits the HTTP boundary to
/// recognize an exact retry before live concurrency state has advanced, while
/// preserving the service's canonical action and integrity authority.
#[derive(Clone, Debug)]
pub(crate) struct ResultActionReplayProbe {
    pub kind: ResultActionKind,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub actor: String,
    pub reason: String,
    pub warning_acknowledgements: Vec<String>,
    pub authority_boundary_acknowledged: bool,
    pub expected_predecessor_action_id: Option<String>,
    pub expected_generation: u64,
    pub idempotency_key: String,
    pub superseded_candidate_id: Option<String>,
    pub superseded_candidate_hash: Option<String>,
}

/// One controlled-workspace WP-006 service.
#[derive(Clone, Debug)]
pub struct ReconciliationApprovalService {
    workspace: PathBuf,
}

impl ReconciliationApprovalService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    /// Replay one integrity-valid committed action only when every
    /// transport-visible semantic field matches exactly.
    ///
    /// # Errors
    ///
    /// Fails closed on malformed probe data, tampered durable evidence, or
    /// reuse of an idempotency key with different action input.
    pub(crate) fn replay_action_if_exact(
        &self,
        job_id: &str,
        probe: &ResultActionReplayProbe,
    ) -> Result<Option<ResultActionCommandResult>> {
        validate_identity(job_id, "job ID")?;
        validate_action_replay_probe(probe)?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let transaction = immediate_transaction(&writer.connection)?;
        crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &transaction,
        )?;
        crate::decision_integrity::verify_decision_integrity_connection(&transaction)
            .map_err(map_predecessor_integrity)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT action_id, canonical_action_json
                 FROM migration_result_actions
                 WHERE job_id = ?1 AND idempotency_key = ?2",
                params![job_id, probe.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((action_id, canonical_action_json)) = existing else {
            transaction.commit()?;
            return Ok(None);
        };
        let value =
            parse_unique_json(canonical_action_json.as_bytes()).map_err(map_domain_integrity)?;
        if canonical_json_string(&value)? != canonical_action_json {
            return integrity("result-action JSON is not canonical");
        }
        let action: ResultAction = serde_json::from_value(value)?;
        action.validate().map_err(map_domain_integrity)?;
        if action.action_id != action_id
            || action.payload.kind != probe.kind
            || action.payload.candidate_id != probe.candidate_id
            || action.payload.candidate_hash != probe.candidate_hash
            || action.payload.actor != probe.actor
            || action.payload.reason != probe.reason
            || action.payload.warning_acknowledgements != probe.warning_acknowledgements
            || action.payload.authority_boundary_acknowledged
                != probe.authority_boundary_acknowledged
            || action.payload.authority_boundary_acknowledgement_version
                != BOUNDARY_ACKNOWLEDGEMENT_VERSION
            || action.payload.predecessor_action_id != probe.expected_predecessor_action_id
            || action.payload.observed_generation != probe.expected_generation
            || action.payload.idempotency_key != probe.idempotency_key
        {
            return conflict("result-action idempotency key was reused with different input");
        }
        verify_supersession_replay_binding(&transaction, &action, probe)?;
        let result = load_action_result(&transaction, &action_id, true)?;
        transaction.commit()?;
        Ok(Some(result))
    }

    /// Calculate and atomically persist one immutable reconciliation snapshot.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid request, predecessor integrity/authority,
    /// explicit bounds, stale input, idempotency conflict, or persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn calculate(
        &self,
        request: &CalculateReconciliationRequest,
    ) -> Result<ReconciliationCommandResult> {
        request.validate()?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        ensure_package_cleanup_not_conflicted(&writer.connection, &request.job_id)?;
        crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &writer.connection,
        )?;
        crate::decision_integrity::verify_decision_integrity_connection(&writer.connection)
            .map_err(map_predecessor_integrity)?;
        let policy_binding = ensure_v1_policy(&writer.connection)?;
        let request_value = json!({
            "contract": CALCULATE_REQUEST_CONTRACT,
            "request": request,
            "policyIdentity": RECONCILIATION_POLICY_IDENTITY,
            "policyHash": policy_binding.policy_hash,
            "policyActivationId": policy_binding.activation_id,
            "policyActivationHash": policy_binding.activation_hash,
        });
        let request_json = canonical_json_string(&request_value)?;
        let request_hash = canonical_hash(&request_value)?;
        if let Some(result) = existing_reconciliation_result(
            &writer.connection,
            &request.job_id,
            &request.idempotency_key,
            &request_hash,
        )? {
            return Ok(result);
        }

        let calculated = calculate_snapshot_inputs(
            &writer.connection,
            &request.job_id,
            &request.validation_run_id,
            &policy_binding.policy,
            &policy_binding.policy_hash,
        )?;
        let attempt_id = internal_id("reconciliation-attempt");
        let snapshot_id = internal_id("reconciliation-snapshot");
        let created_at = now_rfc3339();
        let governing_input_hash =
            canonical_governing_input_hash(&calculated.governing).map_err(map_domain_integrity)?;

        {
            let transaction = immediate_transaction(&writer.connection)?;
            if let Some(result) = existing_reconciliation_result(
                &transaction,
                &request.job_id,
                &request.idempotency_key,
                &request_hash,
            )? {
                transaction.commit()?;
                return Ok(result);
            }
            let current_governing = load_current_governing(
                &transaction,
                &request.job_id,
                &request.validation_run_id,
                &policy_binding.policy,
            )?;
            if current_governing != calculated.governing {
                return conflict(
                    "governing input changed before reconciliation attempt persistence",
                );
            }
            transaction.execute(
                "INSERT INTO reconciliation_attempts (
                     attempt_id, schema_version, job_id, validation_run_id,
                     policy_identity, policy_version, policy_hash,
                     policy_activation_id, policy_activation_hash,
                     engine_version, canonicalization_version,
                     governing_input_hash, idempotency_key, request_hash,
                     canonical_request_json, created_at
                 ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                           ?11, ?12, ?13, ?14, ?15)",
                params![
                    attempt_id,
                    request.job_id,
                    request.validation_run_id,
                    RECONCILIATION_POLICY_IDENTITY,
                    POLICY_VERSION_TEXT,
                    policy_binding.policy_hash,
                    policy_binding.activation_id,
                    policy_binding.activation_hash,
                    RECONCILIATION_ENGINE_VERSION,
                    CANONICAL_JSON_VERSION,
                    governing_input_hash,
                    request.idempotency_key,
                    request_hash,
                    request_json,
                    created_at,
                ],
            )?;
            append_attempt_event(
                &transaction,
                &attempt_id,
                ReconciliationAttemptState::Planned,
                0,
                None,
                None,
                None,
                &created_at,
            )?;
            append_attempt_event(
                &transaction,
                &attempt_id,
                ReconciliationAttemptState::Running,
                0,
                None,
                None,
                None,
                &created_at,
            )?;
            transaction.commit()?;
        }
        inject_hard_crash_after_running();

        let completed_at = now_rfc3339();
        let rows_processed = calculated.governing.accepted_row_count;
        let outcome = (|| {
            let snapshot = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
                snapshot_id,
                attempt_id: attempt_id.clone(),
                governing: calculated.governing.clone(),
                rows: calculated
                    .rows
                    .iter()
                    .map(|row| row.classification.clone())
                    .collect(),
                warning_codes: calculated.warning_codes.clone(),
                blocker_codes: calculated.blocker_codes.clone(),
                completed_at: completed_at.clone(),
            })
            .map_err(map_domain_integrity)?;
            persist_snapshot(
                &writer.connection,
                &snapshot,
                &calculated.rows,
                &policy_binding,
                &calculated.validation_result_hash,
                &completed_at,
            )?;
            load_reconciliation_result(&writer.connection, &snapshot.snapshot_id, false)
        })();
        match outcome {
            Ok(result) => Ok(result),
            Err(error) => {
                persist_attempt_failure(&writer.connection, &attempt_id, rows_processed, &error)?;
                Err(error)
            }
        }
    }

    /// Create one immutable candidate bound to an exact complete snapshot.
    ///
    /// # Errors
    ///
    /// Fails closed on stale/tampered snapshot evidence, idempotency conflict,
    /// attribution bounds, or persistence failure.
    pub fn create_candidate(
        &self,
        request: &CreateCandidateRequest,
    ) -> Result<CandidateCommandResult> {
        request.validate()?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let transaction = immediate_transaction(&writer.connection)?;
        crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &transaction,
        )?;
        crate::decision_integrity::verify_decision_integrity_connection(&transaction)
            .map_err(map_predecessor_integrity)?;
        let snapshot = load_snapshot(&transaction, &request.snapshot_id)?;
        if snapshot.governing.job_id != request.job_id
            || snapshot.snapshot_hash != request.snapshot_hash
        {
            return conflict("candidate request does not bind the exact job snapshot");
        }
        let request_value = json!({
            "contract": CREATE_CANDIDATE_REQUEST_CONTRACT,
            "request": request,
        });
        let request_hash = canonical_hash(&request_value)?;
        if let Some(result) = existing_candidate_result(
            &transaction,
            &request.job_id,
            &request.idempotency_key,
            &request_hash,
        )? {
            transaction.commit()?;
            return Ok(result);
        }
        let policy = load_active_policy(&transaction)?.policy;
        let current_governing = load_current_governing(
            &transaction,
            &request.job_id,
            &snapshot.governing.source_validation_run.identity,
            &policy,
        )?;
        if derive_candidate_currentness_placeholder(&snapshot, &current_governing)?
            == CandidateCurrentness::Stale
        {
            return conflict("cannot create a candidate from a stale reconciliation snapshot");
        }
        let generation = next_candidate_generation(&transaction, &request.job_id)?;
        let required_acknowledgement_codes = snapshot
            .warning_codes
            .iter()
            .filter(|code| policy.warnings_requiring_acknowledgement.contains(code))
            .cloned()
            .collect();
        let candidate = MigrationResultCandidate::build(
            MigrationResultCandidateDraft {
                candidate_id: internal_id("migration-result-candidate"),
                practitioner_id: request.practitioner_id.clone(),
                preparation_reason: request.reason.clone(),
                created_at: now_rfc3339(),
                creation_generation: generation,
                required_acknowledgement_codes,
            },
            &snapshot,
        )
        .map_err(map_domain_integrity)?;
        persist_candidate(
            &transaction,
            &candidate,
            &request.idempotency_key,
            &request_hash,
        )?;
        transaction.commit()?;
        load_candidate_result(&writer.connection, &candidate.candidate_id, false)
    }

    /// Append one approval/rejection/revocation/supersession action.
    ///
    /// # Errors
    ///
    /// Fails closed unless candidate, currentness, eligibility, predecessor,
    /// generation, warning acknowledgements, and idempotency all match.
    pub fn apply_action(
        &self,
        job_id: &str,
        payload: &ResultActionPayload,
    ) -> Result<ResultActionCommandResult> {
        validate_identity(job_id, "job ID")?;
        payload.validate().map_err(map_domain_invalid)?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let transaction = immediate_transaction(&writer.connection)?;
        crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &transaction,
        )?;
        crate::decision_integrity::verify_decision_integrity_connection(&transaction)
            .map_err(map_predecessor_integrity)?;
        let request_hash = canonical_action_payload_hash(payload).map_err(map_domain_invalid)?;
        if let Some(result) = existing_action_result(
            &transaction,
            job_id,
            &payload.idempotency_key,
            &request_hash,
        )? {
            transaction.commit()?;
            return Ok(result);
        }
        let candidate = load_candidate(&transaction, &payload.candidate_id)?;
        if candidate.job_id != job_id
            || candidate.candidate_hash != payload.candidate_hash
            || candidate.snapshot_id != payload.snapshot_id
            || candidate.snapshot_hash != payload.snapshot_hash
        {
            return conflict("result action does not bind the exact job candidate snapshot");
        }
        let snapshot = load_snapshot(&transaction, &candidate.snapshot_id)?;
        let policy = load_active_policy(&transaction)?.policy;
        let current_governing = load_current_governing(
            &transaction,
            job_id,
            &candidate.governing.source_validation_run.identity,
            &policy,
        )?;
        let eligibility = evaluate_approval_eligibility(
            &candidate,
            &snapshot,
            &current_governing,
            &payload.warning_acknowledgements,
            payload.authority_boundary_acknowledged,
        )
        .map_err(map_domain_integrity)?;
        if payload.authority_boundary_acknowledgement_version
            != policy.approval_boundary_acknowledgement_version
        {
            return invalid("action uses an unsupported authority-boundary acknowledgement");
        }
        require_action_semantics(&transaction, job_id, payload, &eligibility)?;
        let action = ResultAction::build(
            internal_id("migration-result-action"),
            now_rfc3339(),
            payload.clone(),
        )
        .map_err(map_domain_invalid)?;
        persist_action(&transaction, job_id, &action, &request_hash)?;
        transaction.commit()?;
        load_action_result(&writer.connection, &action.action_id, false)
    }
}

#[derive(Clone)]
struct PolicyBinding {
    policy: ReconciliationPolicyV1,
    policy_hash: String,
    activation_id: String,
    activation_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyDocumentRegistration {
    policy_json: String,
    policy_hash: String,
    provenance_json: String,
    provenance_hash: String,
    registered_by: String,
    registration_reason: String,
}

struct ClassifiedRow {
    classification: RowClassification,
    references: Vec<LineageReference>,
}

struct CalculatedSnapshotInputs {
    governing: GoverningIdentities,
    rows: Vec<ClassifiedRow>,
    warning_codes: Vec<String>,
    blocker_codes: Vec<String>,
    validation_result_hash: String,
}

#[allow(clippy::too_many_lines)]
fn ensure_v1_policy(connection: &Connection) -> Result<PolicyBinding> {
    ensure_v1_policy_with_legacy_fingerprint(connection, LEGACY_POLICY_PROVENANCE_TUPLE_SHA256)
}

#[allow(clippy::too_many_lines)]
fn ensure_v1_policy_with_legacy_fingerprint(
    connection: &Connection,
    accepted_legacy_fingerprint: &str,
) -> Result<PolicyBinding> {
    let policy = v1_policy();
    policy.validate().map_err(map_domain_integrity)?;
    let policy_hash = canonical_policy_hash(&policy).map_err(map_domain_integrity)?;
    let policy_json = canonical_json_string(&serde_json::to_value(&policy)?)?;
    let registration = current_policy_document_registration(policy_json, policy_hash.clone())?;

    let existing: Option<PolicyDocumentRegistration> = connection
        .query_row(
            "SELECT canonical_policy_json, policy_hash, provenance_json,
                    provenance_hash, registered_by, registration_reason
             FROM reconciliation_policy_documents
             WHERE policy_identity = ?1 AND policy_version = ?2",
            params![RECONCILIATION_POLICY_IDENTITY, POLICY_VERSION_TEXT],
            |row| {
                Ok(PolicyDocumentRegistration {
                    policy_json: row.get(0)?,
                    policy_hash: row.get(1)?,
                    provenance_json: row.get(2)?,
                    provenance_hash: row.get(3)?,
                    registered_by: row.get(4)?,
                    registration_reason: row.get(5)?,
                })
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if !policy_document_registration_is_compatible(
            &existing,
            &registration,
            accepted_legacy_fingerprint,
        )? {
            return integrity("persisted v1 reconciliation policy conflicts with canonical policy");
        }
    } else {
        connection.execute(
            "INSERT INTO reconciliation_policy_documents (
                 policy_identity, policy_version, schema_version,
                 canonicalization_version, engine_version,
                 canonical_policy_json, policy_hash,
                 provenance_json, provenance_hash,
                 registered_by, registration_reason, registered_at
             ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                RECONCILIATION_POLICY_IDENTITY,
                POLICY_VERSION_TEXT,
                CANONICAL_JSON_VERSION,
                RECONCILIATION_ENGINE_VERSION,
                &registration.policy_json,
                &registration.policy_hash,
                &registration.provenance_json,
                &registration.provenance_hash,
                &registration.registered_by,
                &registration.registration_reason,
                now_rfc3339(),
            ],
        )?;
    }

    if let Some(binding) = active_policy_binding(connection)? {
        if binding.policy_hash != policy_hash || binding.policy != policy {
            return conflict("a different reconciliation policy is already active");
        }
        return Ok(binding);
    }

    let activation_id = internal_id("reconciliation-policy-activation");
    let actor = POLICY_REGISTRAR;
    let reason = POLICY_ACTIVATION_REASON;
    let idempotency_key = POLICY_ACTIVATION_IDEMPOTENCY_KEY;
    let activated_at = now_rfc3339();
    let request_value = json!({
        "contract": POLICY_ACTIVATION_CONTRACT,
        "policyIdentity": RECONCILIATION_POLICY_IDENTITY,
        "policyVersion": POLICY_VERSION_TEXT,
        "policyHash": policy_hash,
        "actor": actor,
        "reason": reason,
        "idempotencyKey": idempotency_key,
        "observedGeneration": 0,
    });
    let request_hash = canonical_hash(&request_value)?;
    let activation_value = json!({
        "contract": POLICY_ACTIVATION_CONTRACT,
        "activationId": activation_id,
        "requestHash": request_hash,
        "policyIdentity": RECONCILIATION_POLICY_IDENTITY,
        "policyVersion": POLICY_VERSION_TEXT,
        "policyHash": policy_hash,
        "predecessorActivationId": Value::Null,
        "predecessorGeneration": 0,
        "predecessorActivationHash": Value::Null,
        "generation": 1,
        "actor": actor,
        "reason": reason,
        "activatedAt": activated_at,
    });
    let activation_json = canonical_json_string(&activation_value)?;
    let activation_hash = canonical_hash(&activation_value)?;
    connection.execute(
        "INSERT INTO reconciliation_policy_activations (
             activation_id, schema_version, policy_identity, policy_version,
             policy_hash, predecessor_activation_id, predecessor_generation,
             predecessor_activation_hash, observed_generation, generation,
             actor, reason, idempotency_key, request_hash,
             canonical_activation_json, activation_hash, activated_at
         ) VALUES (?1, 1, ?2, ?3, ?4, NULL, 0, NULL, 0, 1,
                   ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            activation_id,
            RECONCILIATION_POLICY_IDENTITY,
            POLICY_VERSION_TEXT,
            policy_hash,
            actor,
            reason,
            idempotency_key,
            request_hash,
            activation_json,
            activation_hash,
            activated_at,
        ],
    )?;
    Ok(PolicyBinding {
        policy,
        policy_hash,
        activation_id,
        activation_hash,
    })
}

fn current_policy_document_registration(
    policy_json: String,
    policy_hash: String,
) -> Result<PolicyDocumentRegistration> {
    let provenance = json!({
        "contract": POLICY_PROVENANCE_CONTRACT,
        "policyDocument": POLICY_DOCUMENT_PATH,
        "policyDocumentSha256": sha256_hex(POLICY_DOCUMENT_BYTES),
        "authorityBoundary": "LOCAL_MIGRATION_RESULT_READINESS_ONLY",
    });
    Ok(PolicyDocumentRegistration {
        policy_json,
        policy_hash,
        provenance_json: canonical_json_string(&provenance)?,
        provenance_hash: canonical_hash(&provenance)?,
        registered_by: POLICY_REGISTRAR.to_owned(),
        registration_reason: POLICY_REGISTRATION_REASON.to_owned(),
    })
}

fn policy_document_registration_is_compatible(
    existing: &PolicyDocumentRegistration,
    current: &PolicyDocumentRegistration,
    accepted_legacy_fingerprint: &str,
) -> Result<bool> {
    if existing == current {
        return Ok(true);
    }
    if existing.policy_json != current.policy_json || existing.policy_hash != current.policy_hash {
        return Ok(false);
    }
    let fingerprint = policy_provenance_tuple_fingerprint(existing)?;
    Ok(fingerprint == accepted_legacy_fingerprint)
}

fn policy_provenance_tuple_fingerprint(
    registration: &PolicyDocumentRegistration,
) -> Result<String> {
    canonical_hash(&json!([
        &registration.provenance_json,
        &registration.provenance_hash,
        &registration.registered_by,
        &registration.registration_reason,
    ]))
}

fn load_active_policy(connection: &Connection) -> Result<PolicyBinding> {
    active_policy_binding(connection)?.ok_or_else(|| {
        KernelError::ResultApprovalIntegrity("no active reconciliation policy".to_owned())
    })
}

fn active_policy_binding(connection: &Connection) -> Result<Option<PolicyBinding>> {
    type PolicyRow = (String, String, String, String, String);
    let row: Option<PolicyRow> = connection
        .query_row(
            "SELECT a.activation_id, a.activation_hash, a.policy_identity,
                    a.policy_hash, d.canonical_policy_json
             FROM reconciliation_policy_activations a
             JOIN reconciliation_policy_documents d
               ON d.policy_identity = a.policy_identity
              AND d.policy_version = a.policy_version
              AND d.policy_hash = a.policy_hash
             ORDER BY a.generation DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|row| {
        if row.2 != RECONCILIATION_POLICY_IDENTITY {
            return conflict("active reconciliation policy identity is unsupported");
        }
        let value = parse_unique_json(row.4.as_bytes()).map_err(map_domain_integrity)?;
        let policy: ReconciliationPolicyV1 =
            serde_json::from_value(value).map_err(KernelError::from)?;
        policy.validate().map_err(map_domain_integrity)?;
        if canonical_policy_hash(&policy).map_err(map_domain_integrity)? != row.3 {
            return integrity("active reconciliation policy hash does not replay");
        }
        Ok(PolicyBinding {
            policy,
            policy_hash: row.3,
            activation_id: row.0,
            activation_hash: row.1,
        })
    })
    .transpose()
}

struct ValidationAuthority {
    job_id: String,
    source_hash: String,
    accepted_row_count: u64,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    validation_run_id: String,
    validation_result_hash: String,
    root_entity_id: String,
    rule_kinds: HashMap<String, String>,
}

#[derive(Clone)]
struct ActiveDecisionFact {
    decision_id: String,
    decision_hash: String,
    disposition: String,
    overlay_id: Option<String>,
    overlay_hash: Option<String>,
    effectivation_id: Option<String>,
    effectivation_hash: Option<String>,
    effective_value_hash: Option<String>,
    reevaluation_id: Option<String>,
    reevaluation_hash: Option<String>,
}

#[derive(Clone)]
struct IssueFact {
    issue_id: String,
    issue_hash: String,
    severity: String,
    rule_kind: Option<String>,
    active: Option<ActiveDecisionFact>,
    incomplete_correction_attempt: bool,
}

#[derive(Clone)]
struct ReevaluationIssueFact {
    issue_id: String,
    issue_hash: String,
    severity: String,
    source_issue_id: Option<String>,
    reevaluation_id: String,
    reevaluation_hash: String,
}

struct ActiveEvidence {
    decisions: Vec<EvidenceIdentity>,
    overlays: Vec<EvidenceIdentity>,
    effective_values: Vec<EvidenceIdentity>,
    reevaluations: Vec<EvidenceIdentity>,
}

fn calculate_snapshot_inputs(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    policy: &ReconciliationPolicyV1,
    policy_hash: &str,
) -> Result<CalculatedSnapshotInputs> {
    let authority = load_validation_authority(connection, job_id, validation_run_id)?;
    let accepted_rows = load_accepted_rows(connection, &authority)?;
    let accepted_row_set_hash =
        canonical_accepted_row_set_hash(&accepted_rows).map_err(map_domain_integrity)?;
    let active_evidence = load_active_evidence(connection, validation_run_id)?;
    let governing = GoverningIdentities {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        job_id: job_id.to_owned(),
        accepted_source_package: EvidenceIdentity {
            identity: job_id.to_owned(),
            hash: authority.source_hash.clone(),
        },
        accepted_row_set_hash,
        accepted_row_count: authority.accepted_row_count,
        approved_profile: VersionedEvidenceIdentity {
            identity: authority.profile_id.clone(),
            version: authority.profile_version.clone(),
            hash: authority.profile_hash.clone(),
        },
        source_mapping: VersionedEvidenceIdentity {
            identity: authority.mapping_id.clone(),
            version: authority.mapping_version.clone(),
            hash: authority.mapping_hash.clone(),
        },
        source_validation_run: EvidenceIdentity {
            identity: validation_run_id.to_owned(),
            hash: authority.validation_result_hash.clone(),
        },
        active_decision_heads: active_evidence.decisions,
        active_correction_overlays: active_evidence.overlays,
        effective_canonical_values: active_evidence.effective_values,
        qualifying_reevaluations: active_evidence.reevaluations,
        policy_identity: RECONCILIATION_POLICY_IDENTITY.to_owned(),
        policy_hash: policy_hash.to_owned(),
        engine_version: RECONCILIATION_ENGINE_VERSION.to_owned(),
        canonicalization_version: CANONICAL_JSON_VERSION.to_owned(),
    };
    governing.validate().map_err(map_domain_integrity)?;

    let issues = load_issue_facts(connection, &authority)?;
    let reevaluation_issues = load_current_reevaluation_issues(connection, &authority)?;
    let (rows, warning_codes) = classify_rows(
        &accepted_rows,
        &authority,
        &issues,
        &reevaluation_issues,
        policy,
    )?;
    let mut category_counts = BTreeMap::new();
    for row in &rows {
        let count = category_counts
            .entry(row.classification.category)
            .or_insert(0_u64);
        *count = count.checked_add(1).ok_or_else(|| {
            KernelError::ResultApprovalBound("category count overflow".to_owned())
        })?;
    }
    let blocker_codes = [
        ReconciliationCategory::BlockedUnresolved,
        ReconciliationCategory::BlockedIncomplete,
        ReconciliationCategory::BlockedConflict,
    ]
    .into_iter()
    .filter(|category| category_counts.get(category).copied().unwrap_or(0) > 0)
    .map(|category| category.as_str().to_owned())
    .collect();
    Ok(CalculatedSnapshotInputs {
        governing,
        rows,
        warning_codes,
        blocker_codes,
        validation_result_hash: authority.validation_result_hash,
    })
}

fn load_current_governing(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    policy: &ReconciliationPolicyV1,
) -> Result<GoverningIdentities> {
    let policy_hash = canonical_policy_hash(policy).map_err(map_domain_integrity)?;
    let authority = load_validation_authority(connection, job_id, validation_run_id)?;
    let accepted_rows = load_accepted_rows(connection, &authority)?;
    let active = load_active_evidence(connection, validation_run_id)?;
    let governing = GoverningIdentities {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        job_id: job_id.to_owned(),
        accepted_source_package: EvidenceIdentity {
            identity: job_id.to_owned(),
            hash: authority.source_hash,
        },
        accepted_row_set_hash: canonical_accepted_row_set_hash(&accepted_rows)
            .map_err(map_domain_integrity)?,
        accepted_row_count: authority.accepted_row_count,
        approved_profile: VersionedEvidenceIdentity {
            identity: authority.profile_id,
            version: authority.profile_version,
            hash: authority.profile_hash,
        },
        source_mapping: VersionedEvidenceIdentity {
            identity: authority.mapping_id,
            version: authority.mapping_version,
            hash: authority.mapping_hash,
        },
        source_validation_run: EvidenceIdentity {
            identity: validation_run_id.to_owned(),
            hash: authority.validation_result_hash,
        },
        active_decision_heads: active.decisions,
        active_correction_overlays: active.overlays,
        effective_canonical_values: active.effective_values,
        qualifying_reevaluations: active.reevaluations,
        policy_identity: RECONCILIATION_POLICY_IDENTITY.to_owned(),
        policy_hash,
        engine_version: RECONCILIATION_ENGINE_VERSION.to_owned(),
        canonicalization_version: CANONICAL_JSON_VERSION.to_owned(),
    };
    governing.validate().map_err(map_domain_integrity)?;
    Ok(governing)
}

/// Derive the exact current governing-input hash for bounded read models.
///
/// This is read-only and grants no approval or publication authority.
pub(crate) fn current_governing_input_hash_for_read(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
) -> Result<String> {
    let policy = load_active_policy(connection)?.policy;
    let governing = load_current_governing(connection, job_id, validation_run_id, &policy)?;
    canonical_governing_input_hash(&governing).map_err(map_domain_integrity)
}

#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn load_validation_authority(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
) -> Result<ValidationAuthority> {
    validate_identity(job_id, "job ID")?;
    validate_identity(validation_run_id, "validation run ID")?;
    verify_validation_run_for_read(connection, validation_run_id)
        .map_err(map_predecessor_integrity)?;
    type AuthorityRow = (
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
        Option<String>,
        i64,
        i64,
        i64,
    );
    let row: AuthorityRow = connection
        .query_row(
            "SELECT vr.job_id, vr.source_hash,
                    vr.profile_id, vr.profile_version, vr.profile_hash,
                    vr.mapping_id, vr.mapping_version, vr.mapping_hash,
                    vr.state, j.state, j.source_hash, vr.result_hash,
                    vr.rows_processed, j.stored_records, j.pending_records
             FROM validation_runs vr
             JOIN jobs j ON j.id = vr.job_id
             WHERE vr.id = ?1",
            [validation_run_id],
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
                ))
            },
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("validation run {validation_run_id}")))?;
    if row.0 != job_id
        || row.8 != "COMPLETE"
        || row.9 != "COMPLETE"
        || row.10 != row.1
        || row.14 != 0
        || row.12 != row.13
    {
        return integrity("validation run is not an exactly closed accepted-job authority");
    }
    let validation_result_hash = row.11.ok_or_else(|| {
        KernelError::ResultApprovalIntegrity("complete validation lacks result hash".to_owned())
    })?;
    validate_sha256(&validation_result_hash, "validation result hash")?;
    let accepted_row_count = u64::try_from(row.13).map_err(|_| {
        KernelError::ResultApprovalIntegrity("accepted row count is negative".to_owned())
    })?;
    if usize::try_from(accepted_row_count).unwrap_or(usize::MAX) > MAXIMUM_RECONCILIATION_ROWS {
        return Err(KernelError::ResultApprovalBound(format!(
            "accepted row count {accepted_row_count} exceeds {MAXIMUM_RECONCILIATION_ROWS}"
        )));
    }
    let mapping =
        load_stored_mapping(connection, &row.5, &row.6).map_err(map_predecessor_integrity)?;
    if mapping.mapping_hash != row.7
        || mapping.document.profile.profile_id != row.2
        || mapping.document.profile.version != row.3
        || mapping.document.profile.sha256 != row.4
        || mapping.document.source.package_sha256 != row.1
    {
        return integrity("validation mapping authority differs from immutable mapping evidence");
    }
    let profile_json: String = connection.query_row(
        "SELECT document_json FROM profile_versions
         WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3",
        params![row.2, row.3, row.4],
        |record| record.get(0),
    )?;
    let profile = parse_unique_json(profile_json.as_bytes()).map_err(map_predecessor_integrity)?;
    let rule_kinds = profile
        .get("rules")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::ResultApprovalIntegrity("approved profile has no rule array".to_owned())
        })?
        .iter()
        .map(|rule| {
            let id = rule.get("ruleId").and_then(Value::as_str).ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("profile rule lacks ruleId".to_owned())
            })?;
            let kind = rule.get("kind").and_then(Value::as_str).ok_or_else(|| {
                KernelError::ResultApprovalIntegrity("profile rule lacks kind".to_owned())
            })?;
            Ok((id.to_owned(), kind.to_owned()))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(ValidationAuthority {
        job_id: row.0,
        source_hash: row.1,
        accepted_row_count,
        profile_id: row.2,
        profile_version: row.3,
        profile_hash: row.4,
        mapping_id: row.5,
        mapping_version: row.6,
        mapping_hash: row.7,
        validation_run_id: validation_run_id.to_owned(),
        validation_result_hash,
        root_entity_id: mapping.document.root_entity_id,
        rule_kinds,
    })
}

fn load_accepted_rows(
    connection: &Connection,
    authority: &ValidationAuthority,
) -> Result<Vec<AcceptedRowIdentity>> {
    let mut statement = connection.prepare(
        "SELECT file_path, source_ordinal, line_start, line_end
         FROM source_records WHERE job_id = ?1
         ORDER BY file_path, source_ordinal",
    )?;
    let rows = statement
        .query_map([&authority.job_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .map(|row| {
            let row = row?;
            Ok(AcceptedRowIdentity {
                source_file: row.0,
                source_ordinal: positive_u64(row.1, "source ordinal")?,
                line_start: positive_u64(row.2, "line start")?,
                line_end: positive_u64(row.3, "line end")?,
                entity_id: authority.root_entity_id.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if u64::try_from(rows.len()).ok() != Some(authority.accepted_row_count) {
        return integrity("accepted source-row query does not close to authoritative job count");
    }
    for row in &rows {
        row.validate().map_err(map_domain_integrity)?;
    }
    Ok(rows)
}

#[allow(clippy::too_many_lines)]
fn load_active_evidence(
    connection: &Connection,
    validation_run_id: &str,
) -> Result<ActiveEvidence> {
    type ActiveRow = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let mut statement = connection.prepare(
        "SELECT d.decision_id, d.decision_hash, d.disposition,
                d.correction_overlay_id, o.overlay_hash,
                ce.effectivation_id, ce.effectivation_hash, ce.effective_value_hash,
                d.reevaluation_id, r.terminal_hash, r.state
         FROM issue_decisions d
         LEFT JOIN correction_overlays o ON o.overlay_id = d.correction_overlay_id
         LEFT JOIN correction_effectivations ce ON ce.decision_id = d.decision_id
         LEFT JOIN reevaluation_runs r ON r.reevaluation_id = d.reevaluation_id
         WHERE d.validation_run_id = ?1
           AND d.generation = (
               SELECT MAX(head.generation) FROM issue_decisions head
               WHERE head.validation_run_id = d.validation_run_id
                 AND head.issue_id = d.issue_id
           )
         ORDER BY d.decision_id",
    )?;
    let rows = statement
        .query_map([validation_run_id], |row| {
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
            ))
        })?
        .collect::<std::result::Result<Vec<ActiveRow>, _>>()?;
    let mut decisions = Vec::with_capacity(rows.len());
    let mut overlays = Vec::new();
    let mut effective_values = Vec::new();
    let mut reevaluations = Vec::new();
    for row in rows {
        decisions.push(EvidenceIdentity {
            identity: row.0,
            hash: row.1,
        });
        match row.2.as_str() {
            "CORRECTED" => {
                let (
                    Some(overlay_id),
                    Some(overlay_hash),
                    Some(effectivation_id),
                    Some(_effectivation_hash),
                    Some(effective_value_hash),
                    Some(reevaluation_id),
                    Some(reevaluation_hash),
                    Some(state),
                ) = (row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10)
                else {
                    return integrity(
                        "active corrected decision lacks qualifying overlay/effectivation/re-evaluation evidence",
                    );
                };
                if state != "COMPLETE" {
                    return integrity(
                        "active corrected decision references non-COMPLETE re-evaluation",
                    );
                }
                overlays.push(EvidenceIdentity {
                    identity: overlay_id,
                    hash: overlay_hash,
                });
                effective_values.push(EvidenceIdentity {
                    identity: effectivation_id,
                    hash: effective_value_hash,
                });
                reevaluations.push(EvidenceIdentity {
                    identity: reevaluation_id,
                    hash: reevaluation_hash,
                });
            }
            "WAIVED" | "ACCEPTED_AS_IS" | "EXCLUDED" => {
                if row.3.is_some()
                    || row.4.is_some()
                    || row.5.is_some()
                    || row.6.is_some()
                    || row.7.is_some()
                    || row.8.is_some()
                    || row.9.is_some()
                    || row.10.is_some()
                {
                    return integrity("non-correction active decision owns correction evidence");
                }
            }
            _ => return integrity("active decision has unsupported disposition"),
        }
    }
    decisions.sort();
    overlays.sort();
    effective_values.sort();
    reevaluations.sort();
    Ok(ActiveEvidence {
        decisions,
        overlays,
        effective_values,
        reevaluations,
    })
}

#[allow(clippy::too_many_lines)]
fn load_issue_facts(
    connection: &Connection,
    authority: &ValidationAuthority,
) -> Result<BTreeMap<(String, u64), Vec<IssueFact>>> {
    type IssueRow = (
        String,
        i64,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let incomplete_attempts = load_incomplete_attempts(connection, &authority.validation_run_id)?;
    let mut statement = connection.prepare(
        "SELECT vi.source_file, vi.source_ordinal, vi.issue_id, vi.issue_hash,
                vi.severity, vi.rule_id,
                d.decision_id, d.decision_hash, d.disposition,
                d.correction_overlay_id, o.overlay_hash,
                ce.effectivation_id, ce.effectivation_hash, ce.effective_value_hash,
                d.reevaluation_id, r.terminal_hash, r.state
         FROM validation_issues vi
         LEFT JOIN issue_decisions d
           ON d.validation_run_id = vi.run_id AND d.issue_id = vi.issue_id
          AND d.generation = (
              SELECT MAX(head.generation) FROM issue_decisions head
              WHERE head.validation_run_id = vi.run_id
                AND head.issue_id = vi.issue_id
          )
         LEFT JOIN correction_overlays o ON o.overlay_id = d.correction_overlay_id
         LEFT JOIN correction_effectivations ce ON ce.decision_id = d.decision_id
         LEFT JOIN reevaluation_runs r ON r.reevaluation_id = d.reevaluation_id
         WHERE vi.run_id = ?1
         ORDER BY vi.source_file, vi.source_ordinal, vi.issue_id",
    )?;
    let rows = statement
        .query_map([&authority.validation_run_id], |row| {
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
        })?
        .collect::<std::result::Result<Vec<IssueRow>, _>>()?;
    if rows.len() > MAXIMUM_RECONCILIATION_ISSUES {
        return Err(KernelError::ResultApprovalBound(format!(
            "validation issue count exceeds {MAXIMUM_RECONCILIATION_ISSUES}"
        )));
    }
    let mut grouped: BTreeMap<(String, u64), Vec<IssueFact>> = BTreeMap::new();
    for row in rows {
        let ordinal = positive_u64(row.1, "validation issue source ordinal")?;
        let active = match (row.6, row.7, row.8) {
            (None, None, None) => None,
            (Some(decision_id), Some(decision_hash), Some(disposition)) => {
                if disposition == "CORRECTED"
                    && (row.9.is_none()
                        || row.10.is_none()
                        || row.11.is_none()
                        || row.12.is_none()
                        || row.13.is_none()
                        || row.14.is_none()
                        || row.15.is_none()
                        || row.16.as_deref() != Some("COMPLETE"))
                {
                    return integrity("corrected decision issue binding is incomplete");
                }
                Some(ActiveDecisionFact {
                    decision_id,
                    decision_hash,
                    disposition,
                    overlay_id: row.9,
                    overlay_hash: row.10,
                    effectivation_id: row.11,
                    effectivation_hash: row.12,
                    effective_value_hash: row.13,
                    reevaluation_id: row.14,
                    reevaluation_hash: row.15,
                })
            }
            _ => return integrity("partial active decision columns were observed"),
        };
        grouped
            .entry((row.0, ordinal))
            .or_default()
            .push(IssueFact {
                issue_id: row.2.clone(),
                issue_hash: row.3,
                severity: row.4,
                rule_kind: authority.rule_kinds.get(&row.5).cloned(),
                active,
                incomplete_correction_attempt: incomplete_attempts.contains(&row.2),
            });
    }
    Ok(grouped)
}

fn load_incomplete_attempts(
    connection: &Connection,
    validation_run_id: &str,
) -> Result<BTreeSet<String>> {
    let mut statement = connection.prepare(
        "SELECT a.issue_id,
                (SELECT e.state FROM decision_attempt_events e
                 WHERE e.attempt_id = a.attempt_id
                 ORDER BY e.sequence DESC LIMIT 1) AS latest_state
         FROM decision_attempts a
         WHERE a.validation_run_id = ?1 AND a.disposition = 'CORRECTED'
           AND a.created_at = (
               SELECT MAX(latest.created_at) FROM decision_attempts latest
               WHERE latest.validation_run_id = a.validation_run_id
                 AND latest.issue_id = a.issue_id
           )",
    )?;
    let rows = statement.query_map([validation_run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut incomplete = BTreeSet::new();
    for row in rows {
        let (issue_id, state) = row?;
        match state.as_deref() {
            Some("EFFECTIVE") => {}
            Some(
                "RECEIVED"
                | "VALIDATED"
                | "REJECTED"
                | "CONFLICT"
                | "OVERLAY_PERSISTED"
                | "REEVALUATION_PLANNED"
                | "REEVALUATION_RUNNING"
                | "REEVALUATION_COMPLETE"
                | "REEVALUATION_INTERRUPTED"
                | "REEVALUATION_FAILED"
                | "REEVALUATION_BOUND_EXCEEDED"
                | "REEVALUATION_INTEGRITY_BLOCKED"
                | "REEVALUATION_STALE_INPUT",
            ) => {
                incomplete.insert(issue_id);
            }
            Some(other) => {
                return integrity(format!(
                    "corrected decision attempt has unsupported lifecycle head {other}"
                ));
            }
            None => return integrity("corrected decision attempt has no lifecycle head"),
        }
    }
    Ok(incomplete)
}

fn load_current_reevaluation_issues(
    connection: &Connection,
    authority: &ValidationAuthority,
) -> Result<BTreeMap<(String, u64), Vec<ReevaluationIssueFact>>> {
    let mut statement = connection.prepare(
        "SELECT ri.source_file, ri.source_ordinal, ri.issue_id, ri.issue_hash,
                ri.severity, ri.source_issue_id,
                r.reevaluation_id, r.terminal_hash
         FROM issue_decisions d
         JOIN reevaluation_runs r ON r.reevaluation_id = d.reevaluation_id
         JOIN reevaluation_issues ri ON ri.reevaluation_id = r.reevaluation_id
         WHERE d.validation_run_id = ?1
           AND d.disposition = 'CORRECTED'
           AND r.state = 'COMPLETE'
           AND ri.outcome IN ('RESIDUAL', 'NEW')
           AND d.generation = (
               SELECT MAX(head.generation) FROM issue_decisions head
               WHERE head.validation_run_id = d.validation_run_id
                 AND head.issue_id = d.issue_id
           )
         ORDER BY ri.source_file, ri.source_ordinal,
                  r.reevaluation_id, ri.issue_id",
    )?;
    let rows = statement.query_map([&authority.validation_run_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut grouped = BTreeMap::new();
    for row in rows {
        let row = row?;
        grouped
            .entry((row.0, positive_u64(row.1, "re-evaluation source ordinal")?))
            .or_insert_with(Vec::new)
            .push(ReevaluationIssueFact {
                issue_id: row.2,
                issue_hash: row.3,
                severity: row.4,
                source_issue_id: row.5,
                reevaluation_id: row.6,
                reevaluation_hash: row.7,
            });
    }
    Ok(grouped)
}

#[allow(clippy::too_many_lines)]
fn classify_rows(
    accepted_rows: &[AcceptedRowIdentity],
    authority: &ValidationAuthority,
    issues: &BTreeMap<(String, u64), Vec<IssueFact>>,
    reevaluation_issues: &BTreeMap<(String, u64), Vec<ReevaluationIssueFact>>,
    policy: &ReconciliationPolicyV1,
) -> Result<(Vec<ClassifiedRow>, Vec<String>)> {
    let accepted_keys = accepted_rows
        .iter()
        .map(|row| (row.source_file.clone(), row.source_ordinal))
        .collect::<BTreeSet<_>>();
    if issues.keys().any(|key| !accepted_keys.contains(key))
        || reevaluation_issues
            .keys()
            .any(|key| !accepted_keys.contains(key))
    {
        return integrity("validation or re-evaluation evidence references a non-accepted row");
    }
    let mut classified = Vec::with_capacity(accepted_rows.len());
    let mut snapshot_warnings = BTreeSet::new();

    for accepted in accepted_rows {
        let key = (accepted.source_file.clone(), accepted.source_ordinal);
        let row_issues = issues.get(&key).map(Vec::as_slice).unwrap_or_default();
        let row_reevaluation_issues = reevaluation_issues
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let source_issue_ids = row_issues
            .iter()
            .map(|issue| issue.issue_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut references = vec![LineageReference {
            kind: "VALIDATION_RUN".to_owned(),
            identity: authority.validation_run_id.clone(),
            hash: authority.validation_result_hash.clone(),
        }];
        let mut explanation_codes = BTreeSet::new();
        let mut has_conflict = false;
        let mut has_incomplete = false;
        let mut has_exclusion = false;
        let mut has_unresolved = false;
        let mut has_correction = false;
        let mut has_waiver = false;
        let mut has_accepted_as_is = false;

        for issue in row_issues {
            validate_identity(&issue.issue_id, "validation issue ID")?;
            validate_sha256(&issue.issue_hash, "validation issue hash")?;
            references.push(LineageReference {
                kind: "VALIDATION_ISSUE".to_owned(),
                identity: issue.issue_id.clone(),
                hash: issue.issue_hash.clone(),
            });
            let Some(rule_kind) = issue.rule_kind.as_deref() else {
                has_conflict = true;
                explanation_codes.insert("UNSUPPORTED_RULE_AUTHORITY".to_owned());
                continue;
            };
            let policy_blocking = policy
                .blocking_issue_severities
                .iter()
                .any(|severity| severity == &issue.severity);
            if !policy_blocking {
                snapshot_warnings.insert("NON_BLOCKING_VALIDATION_WARNING".to_owned());
            }

            let Some(active) = issue.active.as_ref() else {
                if issue.incomplete_correction_attempt {
                    has_incomplete = true;
                    explanation_codes.insert("CORRECTION_EVIDENCE_INCOMPLETE".to_owned());
                } else if policy_blocking {
                    has_unresolved = true;
                    explanation_codes.insert("VALIDATION_ERROR_UNRESOLVED".to_owned());
                }
                continue;
            };
            validate_identity(&active.decision_id, "active decision ID")?;
            validate_sha256(&active.decision_hash, "active decision hash")?;
            references.push(LineageReference {
                kind: "ISSUE_DECISION".to_owned(),
                identity: active.decision_id.clone(),
                hash: active.decision_hash.clone(),
            });

            match active.disposition.as_str() {
                "CORRECTED" => {
                    if !policy
                        .blocking_rule_kinds
                        .iter()
                        .any(|kind| kind == rule_kind)
                    {
                        has_conflict = true;
                        explanation_codes.insert("CORRECTION_RULE_NOT_GOVERNED".to_owned());
                        continue;
                    }
                    let (
                        Some(overlay_id),
                        Some(overlay_hash),
                        Some(effectivation_id),
                        Some(effectivation_hash),
                        Some(effective_value_hash),
                        Some(reevaluation_id),
                        Some(reevaluation_hash),
                    ) = (
                        active.overlay_id.as_ref(),
                        active.overlay_hash.as_ref(),
                        active.effectivation_id.as_ref(),
                        active.effectivation_hash.as_ref(),
                        active.effective_value_hash.as_ref(),
                        active.reevaluation_id.as_ref(),
                        active.reevaluation_hash.as_ref(),
                    )
                    else {
                        has_incomplete = true;
                        explanation_codes.insert("CORRECTION_EVIDENCE_INCOMPLETE".to_owned());
                        continue;
                    };
                    validate_sha256(effectivation_hash, "correction effectivation hash")?;
                    references.extend([
                        LineageReference {
                            kind: "CORRECTION_OVERLAY".to_owned(),
                            identity: overlay_id.clone(),
                            hash: overlay_hash.clone(),
                        },
                        LineageReference {
                            kind: "CORRECTION_EFFECTIVATION".to_owned(),
                            identity: effectivation_id.clone(),
                            hash: effective_value_hash.clone(),
                        },
                        LineageReference {
                            kind: "REEVALUATION_RUN".to_owned(),
                            identity: reevaluation_id.clone(),
                            hash: reevaluation_hash.clone(),
                        },
                    ]);
                    has_correction = true;
                    explanation_codes.insert("DECISION_CORRECTED".to_owned());
                }
                "WAIVED" => {
                    if policy
                        .waiver_eligible_rule_kinds
                        .iter()
                        .any(|kind| kind == rule_kind)
                    {
                        has_waiver = true;
                        explanation_codes.insert("DECISION_WAIVED".to_owned());
                    } else {
                        has_conflict = true;
                        explanation_codes.insert("WAIVER_RULE_NOT_GOVERNED".to_owned());
                    }
                }
                "ACCEPTED_AS_IS" => {
                    if policy
                        .accepted_as_is_eligible_rule_kinds
                        .iter()
                        .any(|kind| kind == rule_kind)
                    {
                        has_accepted_as_is = true;
                        explanation_codes.insert("DECISION_ACCEPTED_AS_IS".to_owned());
                    } else {
                        has_conflict = true;
                        explanation_codes.insert("ACCEPTED_AS_IS_RULE_NOT_GOVERNED".to_owned());
                    }
                }
                "EXCLUDED" if policy.exclusion_closes_row => {
                    has_exclusion = true;
                    explanation_codes.insert("DECISION_EXCLUDED".to_owned());
                }
                "EXCLUDED" => {
                    has_conflict = true;
                    explanation_codes.insert("EXCLUSION_POLICY_CONFLICT".to_owned());
                }
                _ => {
                    has_conflict = true;
                    explanation_codes.insert("UNSUPPORTED_ACTIVE_DISPOSITION".to_owned());
                }
            }
        }

        for issue in row_reevaluation_issues {
            validate_identity(&issue.issue_id, "re-evaluation issue ID")?;
            validate_sha256(&issue.issue_hash, "re-evaluation issue hash")?;
            if let Some(source_issue_id) = issue.source_issue_id.as_deref() {
                validate_identity(source_issue_id, "re-evaluation source issue ID")?;
                if !source_issue_ids.contains(source_issue_id) {
                    has_conflict = true;
                    explanation_codes.insert("REEVALUATION_SOURCE_CONFLICT".to_owned());
                }
            }
            references.push(LineageReference {
                kind: "REEVALUATION_RUN".to_owned(),
                identity: issue.reevaluation_id.clone(),
                hash: issue.reevaluation_hash.clone(),
            });
            let closed_by_active_noncorrection =
                issue
                    .source_issue_id
                    .as_deref()
                    .is_some_and(|source_issue_id| {
                        row_issues.iter().any(|source| {
                            source.issue_id == source_issue_id
                                && source.active.as_ref().is_some_and(|decision| {
                                    matches!(
                                        decision.disposition.as_str(),
                                        "WAIVED" | "ACCEPTED_AS_IS" | "EXCLUDED"
                                    )
                                })
                        })
                    });
            if closed_by_active_noncorrection {
                explanation_codes.insert("REEVALUATION_ISSUE_GOVERNED".to_owned());
                continue;
            }
            if policy
                .blocking_issue_severities
                .iter()
                .any(|severity| severity == &issue.severity)
            {
                has_unresolved = true;
                explanation_codes.insert("REEVALUATION_BLOCKING_ISSUE".to_owned());
            } else {
                snapshot_warnings.insert("NON_BLOCKING_VALIDATION_WARNING".to_owned());
            }
        }

        references.sort_by(|left, right| {
            (&left.kind, &left.identity, &left.hash).cmp(&(
                &right.kind,
                &right.identity,
                &right.hash,
            ))
        });
        references.dedup_by(|left, right| {
            left.kind == right.kind && left.identity == right.identity && left.hash == right.hash
        });
        if references.windows(2).any(|pair| {
            pair[0].kind == pair[1].kind
                && pair[0].identity == pair[1].identity
                && pair[0].hash != pair[1].hash
        }) {
            has_conflict = true;
            explanation_codes.insert("LINEAGE_IDENTITY_HASH_CONFLICT".to_owned());
        }

        let category = if has_conflict {
            ReconciliationCategory::BlockedConflict
        } else if has_incomplete {
            ReconciliationCategory::BlockedIncomplete
        } else if has_exclusion {
            ReconciliationCategory::Excluded
        } else if has_unresolved {
            ReconciliationCategory::BlockedUnresolved
        } else if has_correction {
            ReconciliationCategory::IncludedCorrected
        } else if has_waiver {
            ReconciliationCategory::IncludedWaived
        } else if has_accepted_as_is {
            ReconciliationCategory::IncludedAcceptedAsIs
        } else {
            ReconciliationCategory::IncludedValid
        };
        if category == ReconciliationCategory::IncludedValid {
            explanation_codes.clear();
        }
        let mut lineage_hashes = references
            .iter()
            .map(|reference| reference.hash.clone())
            .collect::<Vec<_>>();
        lineage_hashes.sort();
        lineage_hashes.dedup();
        let classification = RowClassification {
            row: accepted.clone(),
            category,
            explanation_codes: explanation_codes.into_iter().collect(),
            lineage_hashes,
        };
        classification.validate().map_err(map_domain_integrity)?;
        classified.push(ClassifiedRow {
            classification,
            references,
        });
    }

    Ok((classified, snapshot_warnings.into_iter().collect()))
}

fn positive_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| KernelError::ResultApprovalIntegrity(format!("{label} is not positive")))
}

fn derive_candidate_currentness_placeholder(
    snapshot: &ReconciliationSnapshot,
    current_governing: &GoverningIdentities,
) -> Result<CandidateCurrentness> {
    snapshot.validate().map_err(map_domain_integrity)?;
    current_governing.validate().map_err(map_domain_integrity)?;
    let current_hash =
        canonical_governing_input_hash(current_governing).map_err(map_domain_integrity)?;
    if snapshot.governing == *current_governing && snapshot.governing_input_hash == current_hash {
        Ok(CandidateCurrentness::Current)
    } else {
        Ok(CandidateCurrentness::Stale)
    }
}

fn immediate_transaction(connection: &Connection) -> Result<Transaction<'_>> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn append_attempt_event(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    state: ReconciliationAttemptState,
    rows_processed: u64,
    error_code: Option<&str>,
    detail: Option<&str>,
    resulting_snapshot_hash: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    let previous_event_hash: Option<String> = transaction
        .query_row(
            "SELECT event_hash FROM reconciliation_attempt_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let event_hash = reconciliation_event_hash(
        attempt_id,
        state.as_str(),
        rows_processed,
        error_code,
        detail,
        resulting_snapshot_hash,
        previous_event_hash.as_deref(),
        occurred_at,
    )?;
    transaction.execute(
        "INSERT INTO reconciliation_attempt_events (
             attempt_id, state, rows_processed, error_code, detail,
             resulting_snapshot_hash, previous_event_hash, event_hash, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            attempt_id,
            state.as_str(),
            sqlite_integer(rows_processed, "reconciliation event row count")?,
            error_code,
            detail,
            resulting_snapshot_hash,
            previous_event_hash,
            event_hash,
            occurred_at,
        ],
    )?;
    Ok(())
}

fn failure_state(error: &KernelError) -> ReconciliationAttemptState {
    match error {
        KernelError::Cancelled(_) => ReconciliationAttemptState::Interrupted,
        KernelError::ResultApprovalBound(_)
        | KernelError::DecisionBound(_)
        | KernelError::ValidationBound(_) => ReconciliationAttemptState::BoundExceeded,
        KernelError::ResultApprovalIntegrity(_)
        | KernelError::DecisionIntegrity(_)
        | KernelError::IntegrityFailed(_) => ReconciliationAttemptState::IntegrityBlocked,
        KernelError::ResultApprovalConflict(detail)
            if detail.starts_with("governing input changed") =>
        {
            ReconciliationAttemptState::StaleInput
        }
        KernelError::ResultApprovalConflict(_)
        | KernelError::DecisionConflict(_)
        | KernelError::Conflict(_) => ReconciliationAttemptState::Conflicted,
        _ => ReconciliationAttemptState::Failed,
    }
}

fn persist_attempt_failure(
    connection: &Connection,
    attempt_id: &str,
    rows_processed: u64,
    error: &KernelError,
) -> Result<()> {
    let transaction = immediate_transaction(connection)?;
    let head: Option<String> = transaction
        .query_row(
            "SELECT state FROM reconciliation_attempt_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    match head.as_deref() {
        Some("RUNNING") => append_attempt_event(
            &transaction,
            attempt_id,
            failure_state(error),
            rows_processed,
            Some(error.code()),
            Some("reconciliation did not produce a qualifying complete snapshot"),
            None,
            &now_rfc3339(),
        )?,
        Some(
            "COMPLETE" | "INTERRUPTED" | "FAILED" | "BOUND_EXCEEDED" | "STALE_INPUT"
            | "INTEGRITY_BLOCKED" | "CONFLICTED",
        ) => {}
        Some(state) => {
            return integrity(format!(
                "reconciliation attempt {attempt_id} has unexpected failure head {state}"
            ));
        }
        None => {
            return integrity(format!(
                "reconciliation attempt {attempt_id} has no lifecycle head"
            ));
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Mark every previously-running reconciliation attempt interrupted.
///
/// The append-only event makes recovery idempotent: a repeated recovery sees
/// no `RUNNING` head and appends nothing.
pub(crate) fn recover_interrupted_reconciliations(
    transaction: &Transaction<'_>,
    occurred_at: &str,
) -> Result<usize> {
    let attempt_ids = {
        let mut statement = transaction.prepare(
            "SELECT a.attempt_id
             FROM reconciliation_attempts a
             WHERE (
                 SELECT e.state FROM reconciliation_attempt_events e
                 WHERE e.attempt_id = a.attempt_id
                 ORDER BY e.sequence DESC LIMIT 1
             ) = 'RUNNING'
             ORDER BY a.attempt_id",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for attempt_id in &attempt_ids {
        append_attempt_event(
            transaction,
            attempt_id,
            ReconciliationAttemptState::Interrupted,
            0,
            Some("INTERRUPTED_RECOVERED"),
            Some("prior nonterminal reconciliation recovered after interruption"),
            None,
            occurred_at,
        )?;
    }
    Ok(attempt_ids.len())
}

#[allow(clippy::too_many_lines)]
fn persist_snapshot(
    connection: &Connection,
    snapshot: &ReconciliationSnapshot,
    rows: &[ClassifiedRow],
    policy: &PolicyBinding,
    validation_result_hash: &str,
    completed_at: &str,
) -> Result<()> {
    snapshot.validate().map_err(map_domain_integrity)?;
    if snapshot.governing.policy_hash != policy.policy_hash
        || snapshot.governing.source_validation_run.hash != validation_result_hash
        || snapshot.completed_at != completed_at
        || rows.len() != snapshot.rows.len()
        || rows
            .iter()
            .map(|row| &row.classification)
            .ne(snapshot.rows.iter())
    {
        return integrity("snapshot persistence inputs do not match canonical calculation");
    }
    let warning_codes_json =
        canonical_json_string(&serde_json::to_value(&snapshot.warning_codes)?)?;
    let blocker_codes_json =
        canonical_json_string(&serde_json::to_value(&snapshot.blocker_codes)?)?;
    let transaction = immediate_transaction(connection)?;
    let current_policy = load_active_policy(&transaction)?;
    let current_governing = load_current_governing(
        &transaction,
        &snapshot.governing.job_id,
        &snapshot.governing.source_validation_run.identity,
        &current_policy.policy,
    )?;
    if current_policy.policy_hash != policy.policy_hash
        || current_policy.activation_id != policy.activation_id
        || current_policy.activation_hash != policy.activation_hash
        || current_governing != snapshot.governing
    {
        return conflict("governing input changed before reconciliation snapshot commit");
    }
    transaction.execute(
        "INSERT INTO reconciliation_snapshots (
             snapshot_id, schema_version, attempt_id, state, job_id,
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
             blocker_codes_json, blocker_codes_hash, row_set_hash,
             snapshot_hash, completed_at
         ) VALUES (
             ?1, 1, ?2, 'COMPLETE', ?3, ?4, ?5, ?6,
             ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
             ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34
         )",
        params![
            snapshot.snapshot_id,
            snapshot.attempt_id,
            snapshot.governing.job_id,
            snapshot.governing.accepted_source_package.hash,
            snapshot.governing.accepted_row_set_hash,
            sqlite_integer(snapshot.governing.accepted_row_count, "accepted row count")?,
            snapshot.governing.approved_profile.identity,
            snapshot.governing.approved_profile.version,
            snapshot.governing.approved_profile.hash,
            snapshot.governing.source_mapping.identity,
            snapshot.governing.source_mapping.version,
            snapshot.governing.source_mapping.hash,
            snapshot.governing.source_validation_run.identity,
            validation_result_hash,
            evidence_set_hash(
                "ACTIVE_DECISION_HEADS",
                &snapshot.governing.active_decision_heads
            )?,
            evidence_set_hash(
                "ACTIVE_CORRECTION_OVERLAYS",
                &snapshot.governing.active_correction_overlays
            )?,
            evidence_set_hash(
                "EFFECTIVE_CANONICAL_VALUES",
                &snapshot.governing.effective_canonical_values
            )?,
            evidence_set_hash(
                "QUALIFYING_REEVALUATIONS",
                &snapshot.governing.qualifying_reevaluations
            )?,
            snapshot.governing.policy_identity,
            POLICY_VERSION_TEXT,
            snapshot.governing.policy_hash,
            policy.activation_id,
            policy.activation_hash,
            snapshot.governing.engine_version,
            snapshot.governing.canonicalization_version,
            snapshot.governing_input_hash,
            category_counts_hash(&snapshot.category_counts)?,
            warning_codes_json,
            code_list_hash("WARNING_CODES", &snapshot.warning_codes)?,
            blocker_codes_json,
            code_list_hash("BLOCKER_CODES", &snapshot.blocker_codes)?,
            classification_row_set_hash(&snapshot.rows)?,
            snapshot.snapshot_hash,
            completed_at,
        ],
    )?;
    for category in ReconciliationCategory::all() {
        transaction.execute(
            "INSERT INTO reconciliation_category_counts
                 (snapshot_id, category, row_count) VALUES (?1, ?2, ?3)",
            params![
                snapshot.snapshot_id,
                category.as_str(),
                sqlite_integer(snapshot.category_counts.get(category), "category count")?,
            ],
        )?;
    }
    for row in rows {
        let explanation_json = canonical_json_string(&serde_json::to_value(
            &row.classification.explanation_codes,
        )?)?;
        let references_json = canonical_json_string(&serde_json::to_value(&row.references)?)?;
        transaction.execute(
            "INSERT INTO reconciliation_row_classifications (
                 snapshot_id, job_id, source_file, source_ordinal,
                 line_start, line_end, row_identity_hash, entity_id, category,
                 explanation_codes_json, explanation_codes_hash,
                 lineage_references_json, lineage_references_hash,
                 classification_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14)",
            params![
                snapshot.snapshot_id,
                snapshot.governing.job_id,
                row.classification.row.source_file,
                sqlite_integer(row.classification.row.source_ordinal, "row source ordinal")?,
                sqlite_integer(row.classification.row.line_start, "row line start")?,
                sqlite_integer(row.classification.row.line_end, "row line end")?,
                row_identity_hash(&row.classification.row)?,
                row.classification.row.entity_id,
                row.classification.category.as_str(),
                explanation_json,
                canonical_reconciliation_explanation_codes_hash(
                    &row.classification.explanation_codes
                )?,
                references_json,
                lineage_references_hash(&row.references)?,
                row_classification_hash(&row.classification)?,
            ],
        )?;
        for reference in &row.references {
            transaction.execute(
                "INSERT INTO reconciliation_row_lineage_references (
                     snapshot_id, source_file, source_ordinal,
                     reference_kind, reference_id, reference_hash
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    snapshot.snapshot_id,
                    row.classification.row.source_file,
                    sqlite_integer(
                        row.classification.row.source_ordinal,
                        "lineage source ordinal"
                    )?,
                    reference.kind,
                    reference.identity,
                    reference.hash,
                ],
            )?;
        }
    }
    append_attempt_event(
        &transaction,
        &snapshot.attempt_id,
        ReconciliationAttemptState::Complete,
        snapshot.governing.accepted_row_count,
        None,
        None,
        Some(&snapshot.snapshot_hash),
        completed_at,
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::struct_field_names)]
struct SnapshotHeader {
    snapshot_id: String,
    attempt_id: String,
    state: String,
    job_id: String,
    accepted_source_hash: String,
    accepted_row_set_hash: String,
    accepted_row_count: i64,
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

#[allow(clippy::too_many_lines)]
fn load_snapshot(connection: &Connection, snapshot_id: &str) -> Result<ReconciliationSnapshot> {
    validate_identity(snapshot_id, "snapshot ID")?;
    let header = connection
        .query_row(
            "SELECT snapshot_id, attempt_id, state, job_id,
                    accepted_source_hash, accepted_row_set_hash, accepted_row_count,
                    profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    validation_run_id, validation_result_hash,
                    active_decision_set_hash, active_overlay_set_hash,
                    effective_value_set_hash, qualifying_reevaluation_set_hash,
                    policy_identity, policy_version, policy_hash,
                    engine_version, canonicalization_version, governing_input_hash,
                    category_counts_hash, warning_codes_json, warning_codes_hash,
                    blocker_codes_json, blocker_codes_hash, row_set_hash,
                    snapshot_hash, completed_at
             FROM reconciliation_snapshots WHERE snapshot_id = ?1",
            [snapshot_id],
            |row| {
                Ok(SnapshotHeader {
                    snapshot_id: row.get(0)?,
                    attempt_id: row.get(1)?,
                    state: row.get(2)?,
                    job_id: row.get(3)?,
                    accepted_source_hash: row.get(4)?,
                    accepted_row_set_hash: row.get(5)?,
                    accepted_row_count: row.get(6)?,
                    profile_id: row.get(7)?,
                    profile_version: row.get(8)?,
                    profile_hash: row.get(9)?,
                    mapping_id: row.get(10)?,
                    mapping_version: row.get(11)?,
                    mapping_hash: row.get(12)?,
                    validation_run_id: row.get(13)?,
                    validation_result_hash: row.get(14)?,
                    active_decision_set_hash: row.get(15)?,
                    active_overlay_set_hash: row.get(16)?,
                    effective_value_set_hash: row.get(17)?,
                    qualifying_reevaluation_set_hash: row.get(18)?,
                    policy_identity: row.get(19)?,
                    policy_version: row.get(20)?,
                    policy_hash: row.get(21)?,
                    engine_version: row.get(22)?,
                    canonicalization_version: row.get(23)?,
                    governing_input_hash: row.get(24)?,
                    category_counts_hash: row.get(25)?,
                    warning_codes_json: row.get(26)?,
                    warning_codes_hash: row.get(27)?,
                    blocker_codes_json: row.get(28)?,
                    blocker_codes_hash: row.get(29)?,
                    row_set_hash: row.get(30)?,
                    snapshot_hash: row.get(31)?,
                    completed_at: row.get(32)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("reconciliation snapshot {snapshot_id}")))?;
    if header.state != "COMPLETE" || header.policy_version != POLICY_VERSION_TEXT {
        return integrity("snapshot has unsupported state or policy version");
    }
    let accepted_row_count = u64::try_from(header.accepted_row_count).map_err(|_| {
        KernelError::ResultApprovalIntegrity("negative accepted row count".to_owned())
    })?;
    let warning_codes = parse_json_vec::<String>(&header.warning_codes_json, "warning codes")?;
    let blocker_codes = parse_json_vec::<String>(&header.blocker_codes_json, "blocker codes")?;
    if code_list_hash("WARNING_CODES", &warning_codes)? != header.warning_codes_hash
        || code_list_hash("BLOCKER_CODES", &blocker_codes)? != header.blocker_codes_hash
    {
        return integrity("snapshot warning or blocker hash does not replay");
    }

    let normalized_references = load_normalized_lineage(connection, snapshot_id)?;
    let mut rows = Vec::new();
    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, line_start, line_end,
                row_identity_hash, entity_id, category,
                explanation_codes_json, explanation_codes_hash,
                lineage_references_json, lineage_references_hash,
                classification_hash
         FROM reconciliation_row_classifications
         WHERE snapshot_id = ?1 ORDER BY source_file, source_ordinal",
    )?;
    let records = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
        ))
    })?;
    for record in records {
        let record = record?;
        let row = AcceptedRowIdentity {
            source_file: record.0,
            source_ordinal: positive_u64(record.1, "snapshot source ordinal")?,
            line_start: positive_u64(record.2, "snapshot line start")?,
            line_end: positive_u64(record.3, "snapshot line end")?,
            entity_id: record.5,
        };
        if row_identity_hash(&row)? != record.4 {
            return integrity("snapshot row identity hash does not replay");
        }
        let explanation_codes = parse_json_vec::<String>(&record.7, "explanation codes")?;
        if canonical_reconciliation_explanation_codes_hash(&explanation_codes)? != record.8 {
            return integrity("snapshot row explanation hash does not replay");
        }
        let references = parse_json_vec::<LineageReference>(&record.9, "lineage references")?;
        let key = (row.source_file.clone(), row.source_ordinal);
        if normalized_references.get(&key).map(Vec::as_slice) != Some(references.as_slice())
            || lineage_references_hash(&references)? != record.10
        {
            return integrity("snapshot compact and normalized lineage differ");
        }
        let mut lineage_hashes = references
            .iter()
            .map(|reference| reference.hash.clone())
            .collect::<Vec<_>>();
        lineage_hashes.sort();
        lineage_hashes.dedup();
        let classification = RowClassification {
            row,
            category: category_from_str(&record.6)?,
            explanation_codes,
            lineage_hashes,
        };
        if row_classification_hash(&classification)? != record.11 {
            return integrity("snapshot row classification hash does not replay");
        }
        rows.push(classification);
    }
    drop(statement);

    let active_decision_heads = evidence_from_references(&normalized_references, "ISSUE_DECISION")?;
    let active_correction_overlays =
        evidence_from_references(&normalized_references, "CORRECTION_OVERLAY")?;
    let effective_canonical_values =
        evidence_from_references(&normalized_references, "CORRECTION_EFFECTIVATION")?;
    let qualifying_reevaluations =
        evidence_from_references(&normalized_references, "REEVALUATION_RUN")?;
    if evidence_set_hash("ACTIVE_DECISION_HEADS", &active_decision_heads)?
        != header.active_decision_set_hash
        || evidence_set_hash("ACTIVE_CORRECTION_OVERLAYS", &active_correction_overlays)?
            != header.active_overlay_set_hash
        || evidence_set_hash("EFFECTIVE_CANONICAL_VALUES", &effective_canonical_values)?
            != header.effective_value_set_hash
        || evidence_set_hash("QUALIFYING_REEVALUATIONS", &qualifying_reevaluations)?
            != header.qualifying_reevaluation_set_hash
    {
        return integrity("snapshot governing evidence-set hash does not replay");
    }
    let governing = GoverningIdentities {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        job_id: header.job_id.clone(),
        accepted_source_package: EvidenceIdentity {
            identity: header.job_id,
            hash: header.accepted_source_hash,
        },
        accepted_row_set_hash: header.accepted_row_set_hash,
        accepted_row_count,
        approved_profile: VersionedEvidenceIdentity {
            identity: header.profile_id,
            version: header.profile_version,
            hash: header.profile_hash,
        },
        source_mapping: VersionedEvidenceIdentity {
            identity: header.mapping_id,
            version: header.mapping_version,
            hash: header.mapping_hash,
        },
        source_validation_run: EvidenceIdentity {
            identity: header.validation_run_id,
            hash: header.validation_result_hash,
        },
        active_decision_heads,
        active_correction_overlays,
        effective_canonical_values,
        qualifying_reevaluations,
        policy_identity: header.policy_identity,
        policy_hash: header.policy_hash,
        engine_version: header.engine_version,
        canonicalization_version: header.canonicalization_version,
    };
    let rebuilt = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
        snapshot_id: header.snapshot_id,
        attempt_id: header.attempt_id,
        governing,
        rows,
        warning_codes,
        blocker_codes,
        completed_at: header.completed_at,
    })
    .map_err(map_domain_integrity)?;
    let stored_counts = load_category_counts(connection, snapshot_id)?;
    if rebuilt.governing_input_hash != header.governing_input_hash
        || rebuilt.snapshot_hash != header.snapshot_hash
        || rebuilt.category_counts != stored_counts
        || category_counts_hash(&stored_counts)? != header.category_counts_hash
        || classification_row_set_hash(&rebuilt.rows)? != header.row_set_hash
    {
        return integrity("snapshot aggregate or canonical hash does not replay");
    }
    Ok(rebuilt)
}

fn load_normalized_lineage(
    connection: &Connection,
    snapshot_id: &str,
) -> Result<BTreeMap<(String, u64), Vec<LineageReference>>> {
    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, reference_kind,
                reference_id, reference_hash
         FROM reconciliation_row_lineage_references
         WHERE snapshot_id = ?1
         ORDER BY source_file, source_ordinal,
                  reference_kind, reference_id, reference_hash",
    )?;
    let records = statement.query_map([snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut grouped = BTreeMap::new();
    for record in records {
        let record = record?;
        grouped
            .entry((record.0, positive_u64(record.1, "lineage source ordinal")?))
            .or_insert_with(Vec::new)
            .push(LineageReference {
                kind: record.2,
                identity: record.3,
                hash: record.4,
            });
    }
    Ok(grouped)
}

fn evidence_from_references(
    references: &BTreeMap<(String, u64), Vec<LineageReference>>,
    kind: &str,
) -> Result<Vec<EvidenceIdentity>> {
    let mut by_identity = BTreeMap::new();
    for reference in references
        .values()
        .flatten()
        .filter(|reference| reference.kind == kind)
    {
        if by_identity
            .insert(reference.identity.clone(), reference.hash.clone())
            .is_some_and(|existing| existing != reference.hash)
        {
            return integrity(format!("{kind} lineage identity has conflicting hashes"));
        }
    }
    Ok(by_identity
        .into_iter()
        .map(|(identity, hash)| EvidenceIdentity { identity, hash })
        .collect())
}

fn load_category_counts(connection: &Connection, snapshot_id: &str) -> Result<CategoryCounts> {
    let mut values = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT category, row_count FROM reconciliation_category_counts
         WHERE snapshot_id = ?1 ORDER BY category",
    )?;
    let records = statement.query_map([snapshot_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for record in records {
        let (category, count) = record?;
        let count = u64::try_from(count).map_err(|_| {
            KernelError::ResultApprovalIntegrity("negative category count".to_owned())
        })?;
        if values.insert(category, count).is_some() {
            return integrity("duplicate reconciliation category count");
        }
    }
    if values.len() != ReconciliationCategory::all().len() {
        return integrity("snapshot does not persist all eight category counts");
    }
    Ok(CategoryCounts {
        included_valid: take_category_count(&mut values, ReconciliationCategory::IncludedValid)?,
        included_corrected: take_category_count(
            &mut values,
            ReconciliationCategory::IncludedCorrected,
        )?,
        included_waived: take_category_count(&mut values, ReconciliationCategory::IncludedWaived)?,
        included_accepted_as_is: take_category_count(
            &mut values,
            ReconciliationCategory::IncludedAcceptedAsIs,
        )?,
        excluded: take_category_count(&mut values, ReconciliationCategory::Excluded)?,
        blocked_unresolved: take_category_count(
            &mut values,
            ReconciliationCategory::BlockedUnresolved,
        )?,
        blocked_incomplete: take_category_count(
            &mut values,
            ReconciliationCategory::BlockedIncomplete,
        )?,
        blocked_conflict: take_category_count(
            &mut values,
            ReconciliationCategory::BlockedConflict,
        )?,
    })
}

fn take_category_count(
    values: &mut BTreeMap<String, u64>,
    category: ReconciliationCategory,
) -> Result<u64> {
    values.remove(category.as_str()).ok_or_else(|| {
        KernelError::ResultApprovalIntegrity(format!("missing {} count", category.as_str()))
    })
}

fn category_from_str(value: &str) -> Result<ReconciliationCategory> {
    ReconciliationCategory::all()
        .into_iter()
        .find(|category| category.as_str() == value)
        .ok_or_else(|| {
            KernelError::ResultApprovalIntegrity(format!(
                "unsupported reconciliation category {value}"
            ))
        })
}

fn parse_json_vec<T>(bytes: &str, label: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let value = parse_unique_json(bytes.as_bytes()).map_err(map_domain_integrity)?;
    serde_json::from_value(value)
        .map_err(|error| KernelError::ResultApprovalIntegrity(format!("invalid {label}: {error}")))
}

fn next_candidate_generation(connection: &Connection, job_id: &str) -> Result<u64> {
    let current: i64 = connection.query_row(
        "SELECT COALESCE(MAX(creation_generation), 0)
         FROM migration_result_candidates WHERE job_id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    u64::try_from(current)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| KernelError::ResultApprovalBound("candidate generation overflow".to_owned()))
}

fn persist_candidate(
    transaction: &Transaction<'_>,
    candidate: &MigrationResultCandidate,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<()> {
    candidate.validate().map_err(map_domain_integrity)?;
    let candidate_json = canonical_json_string(&serde_json::to_value(candidate)?)?;
    let warning_codes_json =
        canonical_json_string(&serde_json::to_value(&candidate.warning_codes)?)?;
    let required_json = canonical_json_string(&serde_json::to_value(
        &candidate.required_acknowledgement_codes,
    )?)?;
    transaction.execute(
        "INSERT INTO migration_result_candidates (
             candidate_id, schema_version, job_id, snapshot_id, snapshot_hash,
             governing_input_hash, policy_identity, policy_version, policy_hash,
             engine_version, canonicalization_version, accepted_row_count,
             included_count, excluded_count, blocked_count,
             warning_codes_json, warning_codes_hash,
             required_acknowledgments_json, required_acknowledgments_hash,
             creator, preparation_reason, creation_generation,
             idempotency_key, request_hash, canonical_candidate_json,
             candidate_hash, created_at
         ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                   ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            candidate.candidate_id,
            candidate.job_id,
            candidate.snapshot_id,
            candidate.snapshot_hash,
            candidate.governing_input_hash,
            candidate.policy_identity,
            POLICY_VERSION_TEXT,
            candidate.policy_hash,
            candidate.engine_version,
            candidate.canonicalization_version,
            sqlite_integer(
                candidate.governing.accepted_row_count,
                "candidate accepted count"
            )?,
            sqlite_integer(candidate.counts.included, "candidate included count")?,
            sqlite_integer(candidate.counts.excluded, "candidate excluded count")?,
            sqlite_integer(candidate.counts.blocked, "candidate blocked count")?,
            warning_codes_json,
            code_list_hash("WARNING_CODES", &candidate.warning_codes)?,
            required_json,
            code_list_hash(
                "REQUIRED_ACKNOWLEDGEMENT_CODES",
                &candidate.required_acknowledgement_codes,
            )?,
            candidate.practitioner_id,
            candidate.preparation_reason,
            sqlite_integer(candidate.creation_generation, "candidate generation")?,
            idempotency_key,
            request_hash,
            candidate_json,
            candidate.candidate_hash,
            candidate.created_at,
        ],
    )?;
    Ok(())
}

pub(crate) fn load_candidate(
    connection: &Connection,
    candidate_id: &str,
) -> Result<MigrationResultCandidate> {
    validate_identity(candidate_id, "candidate ID")?;
    let (candidate_json, stored_hash): (String, String) = connection
        .query_row(
            "SELECT canonical_candidate_json, candidate_hash
             FROM migration_result_candidates WHERE candidate_id = ?1",
            [candidate_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::NotFound(format!("migration-result candidate {candidate_id}"))
        })?;
    let value = parse_unique_json(candidate_json.as_bytes()).map_err(map_domain_integrity)?;
    if canonical_json_string(&value)? != candidate_json {
        return integrity("candidate JSON is not canonical");
    }
    let candidate: MigrationResultCandidate = serde_json::from_value(value)?;
    candidate.validate().map_err(map_domain_integrity)?;
    if candidate.candidate_id != candidate_id || candidate.candidate_hash != stored_hash {
        return integrity("candidate identity/hash differs from durable columns");
    }
    let snapshot = load_snapshot(connection, &candidate.snapshot_id)?;
    candidate
        .validate_against_snapshot(&snapshot)
        .map_err(map_domain_integrity)?;
    Ok(candidate)
}

fn existing_candidate_result(
    connection: &Connection,
    job_id: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<CandidateCommandResult>> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT candidate_id, request_hash
             FROM migration_result_candidates
             WHERE job_id = ?1 AND idempotency_key = ?2",
            params![job_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    existing
        .map(|(candidate_id, existing_hash)| {
            if existing_hash != request_hash {
                return conflict("candidate idempotency key was reused with different input");
            }
            load_candidate_result(connection, &candidate_id, true)
        })
        .transpose()
}

fn load_candidate_result(
    connection: &Connection,
    candidate_id: &str,
    idempotent_replay: bool,
) -> Result<CandidateCommandResult> {
    let candidate = load_candidate(connection, candidate_id)?;
    let snapshot = load_snapshot(connection, &candidate.snapshot_id)?;
    let policy = load_active_policy(connection)?.policy;
    let current_governing = load_current_governing(
        connection,
        &candidate.job_id,
        &candidate.governing.source_validation_run.identity,
        &policy,
    )?;
    let eligibility = evaluate_approval_eligibility(
        &candidate,
        &snapshot,
        &current_governing,
        &candidate.required_acknowledgement_codes,
        true,
    )
    .map_err(map_domain_integrity)?;
    Ok(CandidateCommandResult {
        schema_version: CANDIDATE_RESULT_SCHEMA.to_owned(),
        candidate_id: candidate.candidate_id,
        candidate_hash: candidate.candidate_hash,
        snapshot_id: candidate.snapshot_id,
        snapshot_hash: candidate.snapshot_hash,
        generation: candidate.creation_generation,
        current: eligibility.currentness == CandidateCurrentness::Current,
        eligible: eligibility.eligible,
        reason_codes: eligibility.reason_codes,
        idempotent_replay,
        integrity_status: "VERIFIED".to_owned(),
        approval_authority: false,
        publication_authority: false,
    })
}

fn existing_reconciliation_result(
    connection: &Connection,
    job_id: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<ReconciliationCommandResult>> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT attempt_id, request_hash FROM reconciliation_attempts
             WHERE job_id = ?1 AND idempotency_key = ?2",
            params![job_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((attempt_id, existing_hash)) = existing else {
        return Ok(None);
    };
    if existing_hash != request_hash {
        return conflict("reconciliation idempotency key was reused with different input");
    }
    let head: String = connection.query_row(
        "SELECT state FROM reconciliation_attempt_events
         WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
        [&attempt_id],
        |row| row.get(0),
    )?;
    if head != "COMPLETE" {
        return conflict(format!(
            "prior reconciliation attempt {attempt_id} is terminal/noncomplete or requires recovery ({head})"
        ));
    }
    let snapshot_id: String = connection.query_row(
        "SELECT snapshot_id FROM reconciliation_snapshots WHERE attempt_id = ?1",
        [&attempt_id],
        |row| row.get(0),
    )?;
    load_reconciliation_result(connection, &snapshot_id, true).map(Some)
}

fn load_reconciliation_result(
    connection: &Connection,
    snapshot_id: &str,
    idempotent_replay: bool,
) -> Result<ReconciliationCommandResult> {
    let snapshot = load_snapshot(connection, snapshot_id)?;
    Ok(ReconciliationCommandResult {
        schema_version: RECONCILIATION_RESULT_SCHEMA.to_owned(),
        snapshot_id: snapshot.snapshot_id,
        snapshot_hash: snapshot.snapshot_hash,
        governing_input_hash: snapshot.governing_input_hash,
        accepted_row_count: snapshot.governing.accepted_row_count,
        included_count: snapshot
            .category_counts
            .included()
            .map_err(map_domain_integrity)?,
        excluded_count: snapshot.category_counts.excluded,
        blocked_count: snapshot
            .category_counts
            .blocked()
            .map_err(map_domain_integrity)?,
        idempotent_replay,
        integrity_status: "VERIFIED".to_owned(),
        publication_authority: false,
    })
}

fn require_action_semantics(
    connection: &Connection,
    job_id: &str,
    payload: &ResultActionPayload,
    eligibility: &ApprovalEligibility,
) -> Result<()> {
    let head: Option<(String, String, String, String, i64)> = connection
        .query_row(
            "SELECT action_id, action_hash, action_kind, candidate_id, generation
             FROM migration_result_actions
             WHERE job_id = ?1 ORDER BY generation DESC LIMIT 1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    match head.as_ref() {
        None => {
            if payload.observed_generation != 0
                || payload.predecessor_action_id.is_some()
                || payload.predecessor_action_hash.is_some()
            {
                return conflict("result action expected an absent generation-zero head");
            }
        }
        Some((head_id, head_hash, _, _, generation)) => {
            let generation = u64::try_from(*generation).map_err(|_| {
                KernelError::ResultApprovalIntegrity("negative action generation".to_owned())
            })?;
            if payload.observed_generation != generation
                || payload.predecessor_action_id.as_deref() != Some(head_id)
                || payload.predecessor_action_hash.as_deref() != Some(head_hash)
            {
                return conflict("result action does not bind the latest immutable action head");
            }
        }
    }
    let active_head = head.as_ref().is_some_and(|(_, _, kind, _, _)| {
        matches!(kind.as_str(), "APPROVED" | "APPROVAL_SUPERSEDED")
    });
    match payload.kind {
        ResultActionKind::Approved | ResultActionKind::Rejected if active_head => {
            return conflict("an active approval must be revoked or superseded explicitly");
        }
        ResultActionKind::ApprovalRevoked | ResultActionKind::ApprovalSuperseded
            if !active_head =>
        {
            return conflict("revocation or supersession requires the active approval head");
        }
        _ => {}
    }
    if payload.kind == ResultActionKind::ApprovalRevoked
        && head.as_ref().map(|head| head.3.as_str()) != Some(payload.candidate_id.as_str())
    {
        return conflict("revocation must bind the active approved candidate");
    }
    if payload.kind == ResultActionKind::ApprovalSuperseded
        && head.as_ref().map(|head| head.3.as_str()) == Some(payload.candidate_id.as_str())
    {
        return conflict("supersession requires a different replacement candidate");
    }
    if payload.kind.requires_eligible_candidate() && !eligibility.eligible {
        return conflict(format!(
            "candidate is not eligible for {}: {}",
            payload.kind.as_str(),
            eligibility.reason_codes.join(",")
        ));
    }
    Ok(())
}

fn persist_action(
    transaction: &Transaction<'_>,
    job_id: &str,
    action: &ResultAction,
    request_hash: &str,
) -> Result<()> {
    action.validate().map_err(map_domain_integrity)?;
    if action.payload_hash != request_hash {
        return integrity("action payload hash differs from idempotency request hash");
    }
    let action_json = canonical_json_string(&serde_json::to_value(action)?)?;
    let acknowledgements_json = canonical_json_string(&serde_json::to_value(
        &action.payload.warning_acknowledgements,
    )?)?;
    transaction.execute(
        "INSERT INTO migration_result_actions (
             action_id, schema_version, job_id, candidate_id, candidate_hash,
             snapshot_id, snapshot_hash, action_kind,
             predecessor_action_id, predecessor_generation,
             observed_generation, generation, actor, reason,
             warning_acknowledgments_json, warning_acknowledgments_hash,
             authority_boundary_acknowledged, idempotency_key, request_hash,
             canonical_action_json, action_hash, occurred_at
         ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                   ?10, ?11, ?12, ?13, ?14, ?15, 1, ?16, ?17, ?18, ?19, ?20)",
        params![
            action.action_id,
            job_id,
            action.payload.candidate_id,
            action.payload.candidate_hash,
            action.payload.snapshot_id,
            action.payload.snapshot_hash,
            action.payload.kind.as_str(),
            action.payload.predecessor_action_id,
            sqlite_integer(action.payload.observed_generation, "predecessor generation")?,
            sqlite_integer(action.payload.observed_generation, "observed generation")?,
            sqlite_integer(action.generation, "action generation")?,
            action.payload.actor,
            action.payload.reason,
            acknowledgements_json,
            code_list_hash(
                "WARNING_ACKNOWLEDGEMENTS",
                &action.payload.warning_acknowledgements,
            )?,
            action.payload.idempotency_key,
            request_hash,
            action_json,
            action.action_hash,
            action.action_timestamp,
        ],
    )?;
    Ok(())
}

fn validate_action_replay_probe(probe: &ResultActionReplayProbe) -> Result<()> {
    validate_identity(&probe.candidate_id, "action candidate ID")?;
    validate_sha256(&probe.candidate_hash, "action candidate hash")?;
    validate_attribution(&probe.actor, &probe.reason)?;
    validate_idempotency(&probe.idempotency_key)?;
    if !probe.authority_boundary_acknowledged {
        return invalid("result action replay requires the explicit local-authority boundary");
    }
    if probe.warning_acknowledgements.len() > 200
        || probe
            .warning_acknowledgements
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || probe.warning_acknowledgements.iter().any(|code| {
            code.is_empty()
                || code.len() > 256
                || !code.as_bytes()[0].is_ascii_uppercase()
                || !code.bytes().all(|byte| {
                    byte.is_ascii_uppercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b':' | b'-')
                })
        })
    {
        return invalid("result action replay warning acknowledgements are not canonical");
    }
    match (
        probe.expected_generation,
        probe.expected_predecessor_action_id.as_deref(),
    ) {
        (0, None) => {}
        (0, Some(_)) | (_, None) => {
            return invalid("result action replay predecessor/generation shape is invalid");
        }
        (_, Some(action_id)) => validate_identity(action_id, "expected predecessor action ID")?,
    }
    match (
        probe.kind,
        probe.superseded_candidate_id.as_deref(),
        probe.superseded_candidate_hash.as_deref(),
    ) {
        (ResultActionKind::ApprovalSuperseded, Some(candidate_id), Some(candidate_hash)) => {
            validate_identity(candidate_id, "superseded candidate ID")?;
            validate_sha256(candidate_hash, "superseded candidate hash")?;
        }
        (ResultActionKind::ApprovalSuperseded, _, _) => {
            return invalid("supersession replay lacks the superseded candidate binding");
        }
        (_, None, None) => {}
        _ => return invalid("non-supersession replay supplied a superseded candidate binding"),
    }
    Ok(())
}

fn verify_supersession_replay_binding(
    connection: &Connection,
    action: &ResultAction,
    probe: &ResultActionReplayProbe,
) -> Result<()> {
    let Some(expected_candidate_id) = probe.superseded_candidate_id.as_deref() else {
        return Ok(());
    };
    let expected_candidate_hash = probe.superseded_candidate_hash.as_deref().ok_or_else(|| {
        KernelError::ResultApprovalInvalid(
            "supersession replay lacks the superseded candidate hash".to_owned(),
        )
    })?;
    let predecessor_id = action
        .payload
        .predecessor_action_id
        .as_deref()
        .ok_or_else(|| {
            KernelError::ResultApprovalIntegrity(
                "stored supersession action lacks its predecessor".to_owned(),
            )
        })?;
    let predecessor: Option<(String, String)> = connection
        .query_row(
            "SELECT candidate_id, candidate_hash
             FROM migration_result_actions WHERE action_id = ?1",
            [predecessor_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if predecessor
        .as_ref()
        .map(|(id, hash)| (id.as_str(), hash.as_str()))
        != Some((expected_candidate_id, expected_candidate_hash))
    {
        return conflict("result-action idempotency key was reused with different input");
    }
    Ok(())
}

fn existing_action_result(
    connection: &Connection,
    job_id: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<ResultActionCommandResult>> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT action_id, request_hash FROM migration_result_actions
             WHERE job_id = ?1 AND idempotency_key = ?2",
            params![job_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    existing
        .map(|(action_id, existing_hash)| {
            if existing_hash != request_hash {
                return conflict("result-action idempotency key was reused with different input");
            }
            load_action_result(connection, &action_id, true)
        })
        .transpose()
}

fn load_action_result(
    connection: &Connection,
    action_id: &str,
    idempotent_replay: bool,
) -> Result<ResultActionCommandResult> {
    let (action_json, stored_hash, job_id): (String, String, String) = connection
        .query_row(
            "SELECT canonical_action_json, action_hash, job_id
             FROM migration_result_actions WHERE action_id = ?1",
            [action_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("migration-result action {action_id}")))?;
    let value = parse_unique_json(action_json.as_bytes()).map_err(map_domain_integrity)?;
    if canonical_json_string(&value)? != action_json {
        return integrity("result-action JSON is not canonical");
    }
    let action: ResultAction = serde_json::from_value(value)?;
    action.validate().map_err(map_domain_integrity)?;
    if action.action_id != action_id || action.action_hash != stored_hash {
        return integrity("result-action identity/hash differs from durable columns");
    }
    let candidate = load_candidate(connection, &action.payload.candidate_id)?;
    let snapshot = load_snapshot(connection, &action.payload.snapshot_id)?;
    action
        .validate_binding(&candidate, &snapshot)
        .map_err(map_domain_integrity)?;
    let active: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM migration_result_actions
             WHERE job_id = ?1 AND action_id = ?2
               AND action_kind IN ('APPROVED', 'APPROVAL_SUPERSEDED')
               AND generation = (
                   SELECT MAX(generation) FROM migration_result_actions WHERE job_id = ?1
               )
         )",
        params![job_id, action_id],
        |row| row.get(0),
    )?;
    Ok(ResultActionCommandResult {
        schema_version: ACTION_RESULT_SCHEMA.to_owned(),
        action_id: action.action_id,
        action_kind: action.payload.kind.as_str().to_owned(),
        action_hash: action.action_hash,
        candidate_id: action.payload.candidate_id,
        candidate_hash: action.payload.candidate_hash,
        generation: action.generation,
        active,
        idempotent_replay,
        integrity_status: "VERIFIED".to_owned(),
        publication_authority: false,
        authenticated_signature: false,
    })
}

fn v1_policy() -> ReconciliationPolicyV1 {
    let supported_rule_kinds = vec![
        "allowed_values".to_owned(),
        "conditional_required".to_owned(),
        "max_length".to_owned(),
        "pattern".to_owned(),
        "reference_lookup".to_owned(),
        "required".to_owned(),
        "unique".to_owned(),
    ];
    ReconciliationPolicyV1 {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        identity: RECONCILIATION_POLICY_IDENTITY.to_owned(),
        policy_version: 1,
        blocking_issue_severities: vec!["ERROR".to_owned()],
        blocking_rule_kinds: supported_rule_kinds.clone(),
        waiver_eligible_rule_kinds: supported_rule_kinds.clone(),
        accepted_as_is_eligible_rule_kinds: supported_rule_kinds,
        exclusion_closes_row: true,
        required_reevaluation_terminal_states: vec!["COMPLETE".to_owned()],
        required_integrity_states: vec!["VERIFIED".to_owned()],
        operational_warning_codes: vec!["NON_BLOCKING_VALIDATION_WARNING".to_owned()],
        warnings_requiring_acknowledgement: vec![
            "NON_BLOCKING_VALIDATION_WARNING".to_owned(),
        ],
        approval_boundary_acknowledgement_text: "I approve only this exact local Migration Stream result candidate. This is not publication approval, external target-system acceptance or loading authority, an authenticated electronic signature, or production certification.".to_owned(),
        approval_boundary_acknowledgement_version: BOUNDARY_ACKNOWLEDGEMENT_VERSION.to_owned(),
        canonicalization_version: CANONICAL_JSON_VERSION.to_owned(),
        engine_version: RECONCILIATION_ENGINE_VERSION.to_owned(),
    }
}

fn internal_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn inject_hard_crash_after_running() {
    if std::env::var("MIGRATION_STREAM_TEST_CRASH_RECONCILIATION_AFTER_RUNNING")
        .is_ok_and(|value| value == "1")
    {
        std::process::abort();
    }
}

fn invalid<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ResultApprovalInvalid(detail.into()))
}

fn conflict<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ResultApprovalConflict(detail.into()))
}

fn integrity<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ResultApprovalIntegrity(detail.into()))
}

#[allow(clippy::needless_pass_by_value)]
fn map_domain_invalid(error: KernelError) -> KernelError {
    KernelError::ResultApprovalInvalid(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn map_domain_integrity(error: KernelError) -> KernelError {
    KernelError::ResultApprovalIntegrity(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn map_predecessor_integrity(error: KernelError) -> KernelError {
    KernelError::ResultApprovalIntegrity(format!(
        "predecessor decision evidence did not replay: {error}"
    ))
}

fn validate_identity(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return invalid(format!(
            "{label} must be a bounded safe internal identifier"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid(format!("{label} must be lowercase SHA-256"));
    }
    Ok(())
}

fn validate_attribution(practitioner: &str, reason: &str) -> Result<()> {
    validate_text(
        practitioner,
        "practitioner identifier",
        MAXIMUM_IDENTITY_BYTES,
    )?;
    validate_text(reason, "reason", MAXIMUM_REASON_BYTES)
}

fn validate_idempotency(value: &str) -> Result<()> {
    if value.len() < 16 || value.len() > MAXIMUM_IDEMPOTENCY_BYTES {
        return invalid("idempotency key must contain 16..=128 bytes");
    }
    validate_identity(value, "idempotency key")
}

fn validate_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return invalid(format!(
            "{label} must be nonempty bounded text without controls"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("open policy database");
        connection
            .execute_batch(
                "CREATE TABLE reconciliation_policy_documents (
                     policy_identity TEXT NOT NULL,
                     policy_version TEXT NOT NULL,
                     schema_version INTEGER NOT NULL,
                     canonicalization_version TEXT NOT NULL,
                     engine_version TEXT NOT NULL,
                     canonical_policy_json TEXT NOT NULL,
                     policy_hash TEXT NOT NULL,
                     provenance_json TEXT NOT NULL,
                     provenance_hash TEXT NOT NULL,
                     registered_by TEXT NOT NULL,
                     registration_reason TEXT NOT NULL,
                     registered_at TEXT NOT NULL,
                     PRIMARY KEY (policy_identity, policy_version)
                 );
                 CREATE TABLE reconciliation_policy_activations (
                     activation_id TEXT NOT NULL PRIMARY KEY,
                     schema_version INTEGER NOT NULL,
                     policy_identity TEXT NOT NULL,
                     policy_version TEXT NOT NULL,
                     policy_hash TEXT NOT NULL,
                     predecessor_activation_id TEXT,
                     predecessor_generation INTEGER NOT NULL,
                     predecessor_activation_hash TEXT,
                     observed_generation INTEGER NOT NULL,
                     generation INTEGER NOT NULL,
                     actor TEXT NOT NULL,
                     reason TEXT NOT NULL,
                     idempotency_key TEXT NOT NULL,
                     request_hash TEXT NOT NULL,
                     canonical_activation_json TEXT NOT NULL,
                     activation_hash TEXT NOT NULL,
                     activated_at TEXT NOT NULL
                 );",
            )
            .expect("create focused policy schema");
        connection
    }

    fn synthetic_legacy_registration(label: &str) -> PolicyDocumentRegistration {
        let policy = v1_policy();
        let provenance = json!({
            "contract": format!("migration-stream/test-policy-provenance/{label}/v1"),
            "policyDocument": POLICY_DOCUMENT_PATH,
            "policyDocumentSha256": sha256_hex(format!("test-policy-document-{label}")),
            "authorityBoundary": "LOCAL_MIGRATION_RESULT_READINESS_ONLY",
        });
        PolicyDocumentRegistration {
            policy_json: canonical_json_string(&serde_json::to_value(&policy).unwrap()).unwrap(),
            policy_hash: canonical_policy_hash(&policy).unwrap(),
            provenance_json: canonical_json_string(&provenance).unwrap(),
            provenance_hash: canonical_hash(&provenance).unwrap(),
            registered_by: format!("test-policy-registry-{label}"),
            registration_reason: format!("Register synthetic policy tuple {label}"),
        }
    }

    fn insert_policy_state(
        connection: &Connection,
        registration: &PolicyDocumentRegistration,
    ) -> (String, String) {
        connection
            .execute(
                "INSERT INTO reconciliation_policy_documents (
                     policy_identity, policy_version, schema_version,
                     canonicalization_version, engine_version,
                     canonical_policy_json, policy_hash,
                     provenance_json, provenance_hash,
                     registered_by, registration_reason, registered_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    RECONCILIATION_POLICY_IDENTITY,
                    POLICY_VERSION_TEXT,
                    CANONICAL_JSON_VERSION,
                    RECONCILIATION_ENGINE_VERSION,
                    &registration.policy_json,
                    &registration.policy_hash,
                    &registration.provenance_json,
                    &registration.provenance_hash,
                    &registration.registered_by,
                    &registration.registration_reason,
                    "2026-08-10T00:00:00.000Z",
                ],
            )
            .unwrap();

        let activation_id = "test-existing-policy-activation".to_owned();
        let actor = "test-policy-registry";
        let reason = "Preserve the exact existing test activation";
        let idempotency_key = "test-policy-activation-idempotency";
        let activated_at = "2026-08-10T00:00:01.000Z";
        let request_hash = canonical_hash(&json!({
            "contract": POLICY_ACTIVATION_CONTRACT,
            "policyIdentity": RECONCILIATION_POLICY_IDENTITY,
            "policyVersion": POLICY_VERSION_TEXT,
            "policyHash": registration.policy_hash,
            "actor": actor,
            "reason": reason,
            "idempotencyKey": idempotency_key,
            "observedGeneration": 0,
        }))
        .unwrap();
        let activation = json!({
            "contract": POLICY_ACTIVATION_CONTRACT,
            "activationId": activation_id,
            "requestHash": request_hash,
            "policyIdentity": RECONCILIATION_POLICY_IDENTITY,
            "policyVersion": POLICY_VERSION_TEXT,
            "policyHash": registration.policy_hash,
            "predecessorActivationId": Value::Null,
            "predecessorGeneration": 0,
            "predecessorActivationHash": Value::Null,
            "generation": 1,
            "actor": actor,
            "reason": reason,
            "activatedAt": activated_at,
        });
        let activation_json = canonical_json_string(&activation).unwrap();
        let activation_hash = canonical_hash(&activation).unwrap();
        connection
            .execute(
                "INSERT INTO reconciliation_policy_activations (
                     activation_id, schema_version, policy_identity, policy_version,
                     policy_hash, predecessor_activation_id, predecessor_generation,
                     predecessor_activation_hash, observed_generation, generation,
                     actor, reason, idempotency_key, request_hash,
                     canonical_activation_json, activation_hash, activated_at
                 ) VALUES (?1, 1, ?2, ?3, ?4, NULL, 0, NULL, 0, 1,
                           ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    activation_id,
                    RECONCILIATION_POLICY_IDENTITY,
                    POLICY_VERSION_TEXT,
                    &registration.policy_hash,
                    actor,
                    reason,
                    idempotency_key,
                    request_hash,
                    activation_json,
                    activation_hash,
                    activated_at,
                ],
            )
            .unwrap();
        (activation_id, activation_hash)
    }

    fn stored_policy_registration(connection: &Connection) -> PolicyDocumentRegistration {
        connection
            .query_row(
                "SELECT canonical_policy_json, policy_hash, provenance_json,
                        provenance_hash, registered_by, registration_reason
                 FROM reconciliation_policy_documents",
                [],
                |row| {
                    Ok(PolicyDocumentRegistration {
                        policy_json: row.get(0)?,
                        policy_hash: row.get(1)?,
                        provenance_json: row.get(2)?,
                        provenance_hash: row.get(3)?,
                        registered_by: row.get(4)?,
                        registration_reason: row.get(5)?,
                    })
                },
            )
            .unwrap()
    }

    fn stored_activation_state(connection: &Connection) -> (String, String, i64) {
        let (activation_id, activation_hash) = connection
            .query_row(
                "SELECT activation_id, activation_hash
                 FROM reconciliation_policy_activations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM reconciliation_policy_activations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        (activation_id, activation_hash, count)
    }

    #[test]
    fn new_policy_registration_binds_embedded_document_and_reuses_active_binding() {
        let connection = policy_connection();
        let first = ensure_v1_policy(&connection).expect("register neutral policy provenance");
        let provenance_json: String = connection
            .query_row(
                "SELECT provenance_json FROM reconciliation_policy_documents",
                [],
                |row| row.get(0),
            )
            .expect("read policy provenance");
        let provenance = parse_unique_json(provenance_json.as_bytes()).expect("parse provenance");
        assert_eq!(provenance["contract"], POLICY_PROVENANCE_CONTRACT);
        assert_eq!(provenance["policyDocument"], POLICY_DOCUMENT_PATH);
        assert_eq!(
            provenance["policyDocumentSha256"],
            sha256_hex(POLICY_DOCUMENT_BYTES)
        );
        assert_eq!(first.policy.policy_version, 1);
        assert_eq!(
            first.policy_hash,
            canonical_policy_hash(&v1_policy()).unwrap()
        );

        let second = ensure_v1_policy(&connection).expect("reuse active policy binding");
        assert_eq!(second.activation_id, first.activation_id);
        assert_eq!(second.activation_hash, first.activation_hash);
        let activation_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reconciliation_policy_activations",
                [],
                |row| row.get(0),
            )
            .expect("count activations");
        assert_eq!(activation_count, 1);
    }

    #[test]
    fn policy_registration_rejects_every_nonlegacy_drift() {
        let policy = v1_policy();
        let policy_hash = canonical_policy_hash(&policy).expect("policy hash");
        let policy_json = canonical_json_string(&serde_json::to_value(policy).unwrap()).unwrap();
        let current = current_policy_document_registration(policy_json, policy_hash).unwrap();

        let mut variants = Vec::new();
        let mut changed = current.clone();
        changed.policy_json.push(' ');
        variants.push(changed);
        let mut changed = current.clone();
        changed.policy_hash = "0".repeat(64);
        variants.push(changed);
        let mut changed = current.clone();
        changed.provenance_json.push(' ');
        variants.push(changed);
        let mut changed = current.clone();
        changed.provenance_hash = "0".repeat(64);
        variants.push(changed);
        let mut changed = current.clone();
        changed.registered_by.push('x');
        variants.push(changed);
        let mut changed = current.clone();
        changed.registration_reason.push('x');
        variants.push(changed);

        assert!(
            policy_document_registration_is_compatible(
                &current,
                &current,
                LEGACY_POLICY_PROVENANCE_TUPLE_SHA256,
            )
            .unwrap()
        );
        for changed in variants {
            assert!(
                !policy_document_registration_is_compatible(
                    &changed,
                    &current,
                    LEGACY_POLICY_PROVENANCE_TUPLE_SHA256,
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn injected_legacy_fingerprint_preserves_registration_and_active_binding() {
        let connection = policy_connection();
        let legacy = synthetic_legacy_registration("compatibility");
        let fingerprint = policy_provenance_tuple_fingerprint(&legacy).unwrap();
        assert_ne!(fingerprint, LEGACY_POLICY_PROVENANCE_TUPLE_SHA256);
        let (activation_id, activation_hash) = insert_policy_state(&connection, &legacy);
        let registration_before = stored_policy_registration(&connection);
        let activation_before = stored_activation_state(&connection);

        let binding = ensure_v1_policy_with_legacy_fingerprint(&connection, &fingerprint)
            .expect("accept the injected exact legacy tuple");

        assert_eq!(binding.activation_id, activation_id);
        assert_eq!(binding.activation_hash, activation_hash);
        assert_eq!(stored_policy_registration(&connection), registration_before);
        assert_eq!(stored_activation_state(&connection), activation_before);
    }

    #[test]
    fn recomputed_nonlegacy_provenance_is_rejected_without_mutation() {
        let connection = policy_connection();
        let nonlegacy = synthetic_legacy_registration("nonlegacy");
        let parsed = parse_unique_json(nonlegacy.provenance_json.as_bytes()).unwrap();
        assert_eq!(canonical_hash(&parsed).unwrap(), nonlegacy.provenance_hash);
        insert_policy_state(&connection, &nonlegacy);
        let registration_before = stored_policy_registration(&connection);
        let activation_before = stored_activation_state(&connection);

        let Err(error) = ensure_v1_policy(&connection) else {
            panic!("coherent nonlegacy provenance must be rejected");
        };
        assert!(matches!(error, KernelError::ResultApprovalIntegrity(_)));
        assert_eq!(stored_policy_registration(&connection), registration_before);
        assert_eq!(stored_activation_state(&connection), activation_before);
    }

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn row(ordinal: u64) -> AcceptedRowIdentity {
        AcceptedRowIdentity {
            source_file: "synthetic.csv".to_owned(),
            source_ordinal: ordinal,
            line_start: ordinal + 1,
            line_end: ordinal + 1,
            entity_id: "SyntheticEntity".to_owned(),
        }
    }

    fn decision(disposition: &str, suffix: u64) -> ActiveDecisionFact {
        let corrected = disposition == "CORRECTED";
        ActiveDecisionFact {
            decision_id: format!("decision-{suffix}"),
            decision_hash: hash('a'),
            disposition: disposition.to_owned(),
            overlay_id: corrected.then(|| format!("overlay-{suffix}")),
            overlay_hash: corrected.then(|| hash('b')),
            effectivation_id: corrected.then(|| format!("effectivation-{suffix}")),
            effectivation_hash: corrected.then(|| hash('c')),
            effective_value_hash: corrected.then(|| hash('d')),
            reevaluation_id: corrected.then(|| format!("reevaluation-{suffix}")),
            reevaluation_hash: corrected.then(|| hash('e')),
        }
    }

    fn issue(
        suffix: u64,
        rule_kind: Option<&str>,
        active: Option<ActiveDecisionFact>,
        incomplete: bool,
    ) -> IssueFact {
        IssueFact {
            issue_id: format!("issue-{suffix}"),
            issue_hash: hash('f'),
            severity: "ERROR".to_owned(),
            rule_kind: rule_kind.map(str::to_owned),
            active,
            incomplete_correction_attempt: incomplete,
        }
    }

    fn authority() -> ValidationAuthority {
        ValidationAuthority {
            job_id: "job-synthetic".to_owned(),
            source_hash: hash('1'),
            accepted_row_count: 8,
            profile_id: "profile-synthetic".to_owned(),
            profile_version: "1".to_owned(),
            profile_hash: hash('2'),
            mapping_id: "mapping-synthetic".to_owned(),
            mapping_version: "1".to_owned(),
            mapping_hash: hash('3'),
            validation_run_id: "validation-synthetic".to_owned(),
            validation_result_hash: hash('4'),
            root_entity_id: "SyntheticEntity".to_owned(),
            rule_kinds: HashMap::new(),
        }
    }

    #[test]
    fn classifier_is_closed_deterministic_and_covers_all_eight_categories() {
        let accepted_rows = (1..=8).map(row).collect::<Vec<_>>();
        let mut issues = BTreeMap::new();
        issues.insert(
            ("synthetic.csv".to_owned(), 2),
            vec![issue(
                2,
                Some("required"),
                Some(decision("CORRECTED", 2)),
                false,
            )],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 3),
            vec![issue(
                3,
                Some("required"),
                Some(decision("WAIVED", 3)),
                false,
            )],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 4),
            vec![issue(
                4,
                Some("required"),
                Some(decision("ACCEPTED_AS_IS", 4)),
                false,
            )],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 5),
            vec![issue(
                5,
                Some("required"),
                Some(decision("EXCLUDED", 5)),
                false,
            )],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 6),
            vec![issue(6, Some("required"), None, false)],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 7),
            vec![issue(7, Some("required"), None, true)],
        );
        issues.insert(
            ("synthetic.csv".to_owned(), 8),
            vec![issue(8, None, None, false)],
        );
        let expected = ReconciliationCategory::all();
        let (first, warnings) = classify_rows(
            &accepted_rows,
            &authority(),
            &issues,
            &BTreeMap::new(),
            &v1_policy(),
        )
        .expect("classify exact synthetic rows");
        let (second, _) = classify_rows(
            &accepted_rows,
            &authority(),
            &issues,
            &BTreeMap::new(),
            &v1_policy(),
        )
        .expect("repeat deterministic classification");
        assert_eq!(warnings, [] as [String; 0]);
        assert_eq!(first.len(), expected.len());
        assert_eq!(
            first
                .iter()
                .map(|row| row.classification.category)
                .collect::<Vec<_>>(),
            expected,
        );
        assert_eq!(
            first
                .iter()
                .map(|row| &row.classification)
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|row| &row.classification)
                .collect::<Vec<_>>(),
        );
        let counts = CategoryCounts::from_rows(
            &first
                .iter()
                .map(|row| row.classification.clone())
                .collect::<Vec<_>>(),
        )
        .expect("derive exact closed counts");
        assert_eq!(counts.total().expect("total"), 8);
        for category in expected {
            assert_eq!(counts.get(category), 1, "{}", category.as_str());
        }
    }

    #[test]
    fn corrected_row_with_residual_blocker_remains_unresolved() {
        let accepted_rows = vec![row(1)];
        let issues = BTreeMap::from([(
            ("synthetic.csv".to_owned(), 1),
            vec![issue(
                1,
                Some("required"),
                Some(decision("CORRECTED", 1)),
                false,
            )],
        )]);
        let reevaluation = BTreeMap::from([(
            ("synthetic.csv".to_owned(), 1),
            vec![ReevaluationIssueFact {
                issue_id: "residual-1".to_owned(),
                issue_hash: hash('9'),
                severity: "ERROR".to_owned(),
                source_issue_id: Some("issue-1".to_owned()),
                reevaluation_id: "reevaluation-1".to_owned(),
                reevaluation_hash: hash('e'),
            }],
        )]);
        let mut authority = authority();
        authority.accepted_row_count = 1;
        let (rows, _) = classify_rows(
            &accepted_rows,
            &authority,
            &issues,
            &reevaluation,
            &v1_policy(),
        )
        .expect("classify residual blocker");
        assert_eq!(
            rows[0].classification.category,
            ReconciliationCategory::BlockedUnresolved
        );
    }
}

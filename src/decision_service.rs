//! Governed WP-005 issue-decision application service.
//!
//! This is the only business write boundary for decisions and correction
//! overlays. It owns authority replay, optimistic concurrency, idempotency,
//! bounded re-evaluation, and the atomic transition from inert evidence to an
//! effective immutable decision. HTTP is only a typed transport adapter.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json},
    decisions::{
        CorrectionLogicalType, CorrectionOperation, CorrectionOverlay, DecisionLifecycleOutcome,
        IssueDecision, IssueDecisionRequest, IssueDisposition, TextualCorrectionEligibility,
        canonical_decision_hash, canonical_decision_material, canonical_decision_request_hash,
        canonical_decision_request_material, canonical_overlay_hash, canonical_overlay_material,
    },
    effective_values::{
        EffectiveField, effectivation_material, effective_lineage, effective_value_hash,
        load_effective_field, value_kind,
    },
    mapping::{MappingDocument, load_stored_mapping},
    profile_registry::load_approved_profile,
    state::{WorkspaceWriter, ensure_package_cleanup_not_conflicted, integrity_check, now_rfc3339},
    validation::{
        EffectiveOverlayInput, EffectiveValidationEvaluation, IssueEvidence, ValidationControl,
        ValidationPolicy, evaluate_effective_overlay, verify_validation_issue_page_for_read,
        verify_validation_run_for_read,
    },
};

const COMMAND_RESULT_SCHEMA: &str = "migration-stream/write/decision-command-result/v1";

/// Durable result returned only after the service has observed committed
/// authority, including exact idempotent replay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCommandResult {
    pub schema_version: String,
    pub data: DecisionCommandResultData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCommandResultData {
    pub decision_id: String,
    pub disposition: String,
    pub lifecycle_outcome: String,
    pub active: bool,
    pub idempotent_replay: bool,
    pub correction_overlay_id: Option<String>,
    pub reevaluation_id: Option<String>,
    pub reevaluation_state: String,
    pub original_issue_present: bool,
    pub residual_issue_count: u64,
    pub new_issue_count: u64,
    pub integrity_status: String,
}

/// Application service fixed to one controlled workspace and one checked-in
/// canonical profile schema.
#[derive(Clone, Debug)]
pub struct DecisionService {
    workspace: PathBuf,
    profile_schema: PathBuf,
}

impl DecisionService {
    pub fn new(workspace: impl Into<PathBuf>, profile_schema: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            profile_schema: profile_schema.into(),
        }
    }

    /// Apply one governed decision synchronously.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid contracts, authority/integrity mismatch, stale
    /// concurrency, conflicting idempotency, bounded re-evaluation failure, or
    /// persistence error. No incomplete correction becomes effective.
    pub fn apply(&self, request: &IssueDecisionRequest) -> Result<DecisionCommandResult> {
        self.apply_with_control(request, &ValidationControl::default())
    }

    /// Apply one command with bounded cooperative re-evaluation controls.
    ///
    /// This is exposed for recovery/failure proof; the HTTP transport never
    /// accepts filesystem paths or cancellation controls from a request.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, conflict, integrity, bound, cancellation,
    /// or persistence error without granting incomplete authority.
    pub fn apply_with_control(
        &self,
        request: &IssueDecisionRequest,
        control: &ValidationControl,
    ) -> Result<DecisionCommandResult> {
        request.validate()?;
        let request_hash = canonical_decision_request_hash(request)?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let integrity = integrity_check(&self.workspace)?;
        if !integrity.ok {
            return Err(KernelError::DecisionIntegrity(
                "workspace integrity preflight blocks governed decisions".to_owned(),
            ));
        }

        if let Some(existing) = idempotent_result(
            &writer.connection,
            &request.scope.job_id,
            &request.idempotency_key,
            &request_hash,
        )? {
            return existing;
        }

        let authority = load_authority(&writer, request, &self.profile_schema)?;
        let effective =
            load_effective_field(&writer.connection, &request.scope, &authority.mapping)?;
        require_current_head(&writer.connection, request)?;
        require_current_effective_hash(request, &effective)?;

        if request.disposition == IssueDisposition::Corrected {
            let source_issue_count: i64 = writer.connection.query_row(
                "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
                [&request.scope.validation_run_id],
                |row| row.get(0),
            )?;
            let source_issue_count = u64::try_from(source_issue_count).map_err(|_| {
                KernelError::DecisionIntegrity(
                    "source validation issue count is negative".to_owned(),
                )
            })?;
            let maximum_new_issues =
                u64::try_from(authority.policy.maximum_issues).map_err(|_| {
                    KernelError::DecisionBound(
                        "validation maximum issue count does not fit replay capacity".to_owned(),
                    )
                })?;
            let maximum_reevaluation_issues = source_issue_count
                .checked_add(maximum_new_issues)
                .ok_or_else(|| {
                    KernelError::DecisionBound(
                        "re-evaluation evidence reservation overflowed".to_owned(),
                    )
                })?;
            crate::decision_integrity::ensure_correction_write_capacity(
                &writer.connection,
                maximum_reevaluation_issues,
            )?;
        } else {
            crate::decision_integrity::ensure_non_correction_write_capacity(&writer.connection)?;
        }

        match request.disposition {
            IssueDisposition::Corrected => Self::apply_correction(
                &writer,
                request,
                &request_hash,
                &authority,
                &effective,
                control,
            ),
            IssueDisposition::Waived
            | IssueDisposition::AcceptedAsIs
            | IssueDisposition::Excluded => {
                apply_non_correction(&writer, request, &request_hash, &authority.mapping)
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn apply_correction(
        writer: &WorkspaceWriter,
        request: &IssueDecisionRequest,
        request_hash: &str,
        authority: &AuthorityContext,
        effective: &EffectiveField,
        control: &ValidationControl,
    ) -> Result<DecisionCommandResult> {
        let eligibility = correction_eligibility(request, authority)?;
        request.validate_correction_eligibility(&eligibility)?;
        let correction_request = request.correction_overlay.as_ref().ok_or_else(|| {
            KernelError::DecisionInvalid("CORRECTED has no correction overlay".to_owned())
        })?;
        let attempt_id = internal_id("decision-attempt");
        let overlay_id = internal_id("correction-overlay");
        let reevaluation_id = internal_id("reevaluation");
        let created_at = now_rfc3339();
        let mut overlay = CorrectionOverlay {
            schema_version: crate::decisions::DECISION_SCHEMA_VERSION,
            overlay_id: overlay_id.clone(),
            decision_request_hash: request_hash.to_owned(),
            scope: request.scope.clone(),
            prior_effective_value_hash: effective.effective_value_hash.clone(),
            operation: correction_request.operation,
            replacement_text: correction_request.replacement_text.clone(),
            practitioner_id: request.practitioner_id.clone(),
            reason: request.reason.clone(),
            created_at: created_at.clone(),
            overlay_hash: "0".repeat(64),
        };
        overlay.overlay_hash = canonical_overlay_hash(&overlay)?;
        overlay.validate(&eligibility)?;

        let scope_value = reevaluation_scope_material(request, "FULL_INHERITED_VALIDATION_SCOPE");
        let scope_json = canonical_json_string(&scope_value)?;
        let scope_hash = canonical_hash(&scope_value)?;
        let identity_value = reevaluation_identity_material(
            &attempt_id,
            &overlay_id,
            &request.scope.validation_run_id,
            &request.scope.issue_id,
            &authority.rule_set_hash,
            &authority.engine_version,
            &authority.policy_hash,
            &scope_hash,
        );
        let identity_hash = canonical_hash(&identity_value)?;

        {
            let transaction = writer.connection.unchecked_transaction()?;
            insert_attempt(
                &transaction,
                &attempt_id,
                request,
                request_hash,
                &created_at,
            )?;
            append_attempt_event(&transaction, &attempt_id, "RECEIVED", None, None)?;
            append_attempt_event(&transaction, &attempt_id, "VALIDATED", None, None)?;
            insert_overlay(&transaction, &attempt_id, &overlay)?;
            append_attempt_event(&transaction, &attempt_id, "OVERLAY_PERSISTED", None, None)?;
            append_attempt_event(
                &transaction,
                &attempt_id,
                "REEVALUATION_PLANNED",
                None,
                None,
            )?;
            append_attempt_event(
                &transaction,
                &attempt_id,
                "REEVALUATION_RUNNING",
                None,
                None,
            )?;
            transaction.commit()?;
        }
        inject_hard_crash_after_overlay_persisted();

        let evaluation = evaluate_effective_overlay(
            &writer.connection,
            &request.scope.job_id,
            &request.scope.validation_run_id,
            &authority.source_hash,
            &authority.mapping,
            &authority.mapping_hash,
            &authority.rules,
            &authority.policy,
            &EffectiveOverlayInput {
                source_file: request.scope.accepted_source_file.clone(),
                source_ordinal: request.scope.source_ordinal,
                entity_id: request.scope.canonical_entity_id.clone(),
                field_id: request.scope.canonical_field_id.clone(),
                value: correction_request.effective_value().map(str::to_owned),
            },
            control,
        );
        let evaluation = match evaluation {
            Ok(evaluation) => evaluation,
            Err(error) => {
                persist_failed_reevaluation(
                    writer,
                    request,
                    &attempt_id,
                    &overlay_id,
                    &reevaluation_id,
                    &identity_hash,
                    authority,
                    &scope_json,
                    &scope_hash,
                    &error,
                    &created_at,
                )?;
                return Err(error);
            }
        };
        if evaluation.scope_kind != "FULL_INHERITED_VALIDATION_SCOPE" {
            let error = KernelError::DecisionIntegrity(
                "re-evaluation did not execute the sealed inherited validation scope".to_owned(),
            );
            persist_failed_reevaluation(
                writer,
                request,
                &attempt_id,
                &overlay_id,
                &reevaluation_id,
                &identity_hash,
                authority,
                &scope_json,
                &scope_hash,
                &error,
                &created_at,
            )?;
            return Err(error);
        }

        require_current_head(&writer.connection, request)?;
        let current_effective =
            load_effective_field(&writer.connection, &request.scope, &authority.mapping)?;
        require_current_effective_hash(request, &current_effective)?;
        if current_effective.effective_value_hash != effective.effective_value_hash {
            return Err(KernelError::DecisionConflict(
                "effective value changed during bounded re-evaluation".to_owned(),
            ));
        }

        complete_correction(
            writer,
            request,
            request_hash,
            &attempt_id,
            &overlay,
            &reevaluation_id,
            &identity_hash,
            authority,
            effective,
            &evaluation,
            &scope_json,
            &scope_hash,
            &created_at,
        )
    }
}

fn inject_hard_crash_after_overlay_persisted() {
    if std::env::var("MIGRATION_STREAM_TEST_CRASH_DECISION_AFTER_OVERLAY")
        .is_ok_and(|value| value == "1")
    {
        std::process::abort();
    }
}

struct AuthorityContext {
    source_hash: String,
    mapping: MappingDocument,
    mapping_hash: String,
    profile_document: Value,
    rules: Value,
    rule_set_hash: String,
    engine_version: String,
    policy: ValidationPolicy,
    policy_json: String,
    policy_hash: String,
}

#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn load_authority(
    writer: &WorkspaceWriter,
    request: &IssueDecisionRequest,
    profile_schema: &Path,
) -> Result<AuthorityContext> {
    verify_validation_run_for_read(&writer.connection, &request.scope.validation_run_id)
        .map_err(|error| KernelError::DecisionIntegrity(error.to_string()))?;
    verify_validation_issue_page_for_read(
        &writer.connection,
        &request.scope.validation_run_id,
        std::slice::from_ref(&request.scope.issue_id),
    )
    .map_err(|error| KernelError::DecisionIntegrity(error.to_string()))?;

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
        String,
        String,
        i64,
        i64,
    );
    let row: AuthorityRow = writer
        .connection
        .query_row(
            "SELECT vr.state, vr.source_hash,
                    vr.profile_id, vr.profile_version, vr.profile_hash,
                    vr.mapping_id, vr.mapping_version, vr.mapping_hash,
                    vr.rule_set_hash, vr.engine_version, vr.policy_json, vr.policy_hash,
                    vi.issue_hash, vi.line_start, vi.line_end
             FROM validation_runs vr
             JOIN validation_issues vi ON vi.run_id = vr.id
             WHERE vr.id = ?1 AND vr.job_id = ?2 AND vi.issue_id = ?3
               AND vi.entity_id = ?4 AND vi.field_id = ?5
               AND vi.source_file = ?6 AND vi.source_ordinal = ?7",
            params![
                request.scope.validation_run_id,
                request.scope.job_id,
                request.scope.issue_id,
                request.scope.canonical_entity_id,
                request.scope.canonical_field_id,
                request.scope.accepted_source_file,
                sqlite_i64(request.scope.source_ordinal, "source ordinal")?,
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
        .ok_or_else(|| {
            KernelError::DecisionInvalid(
                "decision scope does not identify one accepted validation issue".to_owned(),
            )
        })?;
    if row.0 != "COMPLETE"
        || row.12 != request.scope.issue_hash
        || nonnegative_u64(row.13, "issue line start")? != request.scope.line_start
        || nonnegative_u64(row.14, "issue line end")? != request.scope.line_end
        || row.2 != request.scope.profile_id
        || row.3 != request.scope.profile_version
        || row.4 != request.scope.profile_hash
        || row.5 != request.scope.mapping_id
        || row.6 != request.scope.mapping_version
        || row.7 != request.scope.mapping_hash
    {
        return Err(KernelError::DecisionConflict(
            "decision authority identities are stale or mismatched".to_owned(),
        ));
    }
    let job: (String, Option<String>, Option<String>) = writer.connection.query_row(
        "SELECT state, source_hash, stored_archive FROM jobs WHERE id = ?1",
        [&request.scope.job_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if job.0 != "COMPLETE" || job.1.as_deref() != Some(row.1.as_str()) {
        return Err(KernelError::DecisionIntegrity(
            "decision source is not one complete accepted job".to_owned(),
        ));
    }
    let stored_archive = job.2.ok_or_else(|| {
        KernelError::DecisionIntegrity("accepted job has no immutable archive identity".to_owned())
    })?;
    let stored_extension = Path::new(&stored_archive)
        .extension()
        .and_then(|extension| extension.to_str());
    if stored_extension.is_some_and(|extension| {
        extension.eq_ignore_ascii_case("part") || extension.eq_ignore_ascii_case("tombstone")
    }) || crate::durability::path_uses_tombstone_namespace(Path::new(&stored_archive))
    {
        return Err(KernelError::DecisionIntegrity(
            "live partial or tombstone evidence cannot govern a decision".to_owned(),
        ));
    }
    ensure_package_cleanup_not_conflicted(&writer.connection, &request.scope.job_id)?;

    let mapping = load_stored_mapping(
        &writer.connection,
        &request.scope.mapping_id,
        &request.scope.mapping_version,
    )?;
    if mapping.mapping_hash != request.scope.mapping_hash
        || mapping.document.profile.profile_id != request.scope.profile_id
        || mapping.document.profile.version != request.scope.profile_version
        || mapping.document.profile.sha256 != request.scope.profile_hash
        || mapping.document.source.package_sha256 != row.1
        || mapping.document.root_entity_id != request.scope.canonical_entity_id
    {
        return Err(KernelError::DecisionIntegrity(
            "decision scope differs from immutable mapping authority".to_owned(),
        ));
    }
    let approved = load_approved_profile(
        writer,
        profile_schema,
        &request.scope.profile_id,
        &request.scope.profile_version,
        &request.scope.profile_hash,
    )?;
    if !approved.view.can_start_new_runs || approved.profile_hash != request.scope.profile_hash {
        return Err(KernelError::DecisionInvalid(
            "exact approved profile is not eligible for a new local decision".to_owned(),
        ));
    }
    let profile_document = approved.profile.document().clone();
    let rules = profile_document.get("rules").cloned().ok_or_else(|| {
        KernelError::DecisionIntegrity("approved profile has no rule evidence".to_owned())
    })?;
    if canonical_hash(&rules)? != row.8 {
        return Err(KernelError::DecisionIntegrity(
            "validation rule-set hash differs from the approved profile".to_owned(),
        ));
    }
    let policy_value = parse_unique_json(row.10.as_bytes()).map_err(|error| {
        KernelError::DecisionIntegrity(format!("validation policy JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&policy_value)? != row.10 || canonical_hash(&policy_value)? != row.11 {
        return Err(KernelError::DecisionIntegrity(
            "validation policy bytes/hash are not canonical".to_owned(),
        ));
    }
    let policy: ValidationPolicy = serde_json::from_value(policy_value)?;
    Ok(AuthorityContext {
        source_hash: row.1,
        mapping: mapping.document,
        mapping_hash: mapping.mapping_hash,
        profile_document,
        rules,
        rule_set_hash: row.8,
        engine_version: row.9,
        policy,
        policy_json: row.10,
        policy_hash: row.11,
    })
}

fn correction_eligibility(
    request: &IssueDecisionRequest,
    authority: &AuthorityContext,
) -> Result<TextualCorrectionEligibility> {
    let entities = authority
        .profile_document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::DecisionIntegrity("profile entities are absent".to_owned()))?;
    let matching_entities = entities
        .iter()
        .filter(|entity| {
            entity.get("entityId").and_then(Value::as_str)
                == Some(request.scope.canonical_entity_id.as_str())
        })
        .collect::<Vec<_>>();
    let entity = matching_entities.first().copied().ok_or_else(|| {
        KernelError::DecisionInvalid("correction target entity is absent".to_owned())
    })?;
    let fields = entity
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::DecisionIntegrity("profile fields are absent".to_owned()))?;
    let matching_fields = fields
        .iter()
        .filter(|field| {
            field.get("fieldId").and_then(Value::as_str)
                == Some(request.scope.canonical_field_id.as_str())
        })
        .collect::<Vec<_>>();
    let field = matching_fields.first().copied().ok_or_else(|| {
        KernelError::DecisionInvalid("correction target field is absent".to_owned())
    })?;
    let binding_count = authority
        .mapping
        .bindings
        .iter()
        .filter(|binding| binding.target_field_id == request.scope.canonical_field_id)
        .count();
    let maximum_length = field
        .get("maxLength")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .map_err(|_| {
            KernelError::DecisionBound("profile maxLength does not fit usize".to_owned())
        })?;
    Ok(TextualCorrectionEligibility {
        exact_scope_match: matching_entities.len() == 1
            && matching_fields.len() == 1
            && binding_count == 1,
        root_entity: entity.get("root").and_then(Value::as_bool) == Some(true)
            && authority.mapping.root_entity_id == request.scope.canonical_entity_id,
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

fn require_current_head(connection: &Connection, request: &IssueDecisionRequest) -> Result<()> {
    let current: Option<(String, i64)> = connection
        .query_row(
            "SELECT decision_id, generation FROM issue_decisions
             WHERE validation_run_id = ?1 AND issue_id = ?2
             ORDER BY generation DESC, decision_id DESC LIMIT 1",
            params![request.scope.validation_run_id, request.scope.issue_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let expected_generation = request.concurrency.expected_active_decision_generation;
    match (
        current,
        request.concurrency.expected_active_decision_id.as_deref(),
    ) {
        (None, None) if expected_generation == 0 => Ok(()),
        (Some((id, generation)), Some(expected))
            if id == expected
                && nonnegative_u64(generation, "decision generation")? == expected_generation =>
        {
            Ok(())
        }
        _ => Err(KernelError::DecisionConflict(
            "active decision head or generation is stale".to_owned(),
        )),
    }
}

fn require_current_effective_hash(
    request: &IssueDecisionRequest,
    effective: &EffectiveField,
) -> Result<()> {
    if matches!(
        request.disposition,
        IssueDisposition::Corrected | IssueDisposition::AcceptedAsIs
    ) && request.concurrency.expected_effective_value_hash.as_deref()
        != Some(effective.effective_value_hash.as_str())
    {
        return Err(KernelError::DecisionConflict(
            "expected effective value hash is stale".to_owned(),
        ));
    }
    Ok(())
}

fn idempotent_result(
    connection: &Connection,
    job_id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<Result<DecisionCommandResult>>> {
    let attempt: Option<(String, String)> = connection
        .query_row(
            "SELECT attempt_id, request_hash FROM decision_attempts
             WHERE job_id = ?1 AND idempotency_key = ?2",
            params![job_id, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((attempt_id, stored_hash)) = attempt else {
        return Ok(None);
    };
    if stored_hash != request_hash {
        return Ok(Some(Err(KernelError::DecisionConflict(
            "idempotency key was already used for different canonical bytes".to_owned(),
        ))));
    }
    let decision_id: Option<String> = connection
        .query_row(
            "SELECT decision_id FROM issue_decisions WHERE decision_attempt_id = ?1",
            [&attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(decision_id) = decision_id else {
        return Ok(Some(Err(KernelError::DecisionConflict(
            "identical command has inert incomplete evidence and no effective decision".to_owned(),
        ))));
    };
    Ok(Some(load_command_result(connection, &decision_id, true)))
}

fn apply_non_correction(
    writer: &WorkspaceWriter,
    request: &IssueDecisionRequest,
    request_hash: &str,
    mapping: &MappingDocument,
) -> Result<DecisionCommandResult> {
    let attempt_id = internal_id("decision-attempt");
    let decision_id = internal_id("decision");
    let now = now_rfc3339();
    let generation = request
        .concurrency
        .expected_active_decision_generation
        .checked_add(1)
        .ok_or_else(|| KernelError::DecisionBound("decision generation overflow".to_owned()))?;
    let mut decision = IssueDecision {
        schema_version: crate::decisions::DECISION_SCHEMA_VERSION,
        decision_id: decision_id.clone(),
        request_hash: request_hash.to_owned(),
        idempotency_key: request.idempotency_key.clone(),
        disposition: request.disposition,
        scope: request.scope.clone(),
        concurrency: request.concurrency.clone(),
        practitioner_id: request.practitioner_id.clone(),
        application_timestamp: now.clone(),
        reason: request.reason.clone(),
        evidence_reference: request.evidence_reference.clone(),
        authority_separation_acknowledged: request.authority_separation_acknowledged,
        row_scope_exclusion_acknowledged: request.row_scope_exclusion_acknowledged,
        active_generation: generation,
        correction_overlay_id: None,
        reevaluation_id: None,
        lifecycle_outcome: DecisionLifecycleOutcome::Effective,
        decision_hash: "0".repeat(64),
    };
    decision.decision_hash = canonical_decision_hash(&decision)?;
    decision.validate()?;

    let transaction = writer.connection.unchecked_transaction()?;
    insert_attempt(&transaction, &attempt_id, request, request_hash, &now)?;
    append_attempt_event(&transaction, &attempt_id, "RECEIVED", None, None)?;
    append_attempt_event(&transaction, &attempt_id, "VALIDATED", None, None)?;
    require_current_head(&transaction, request)?;
    if request.disposition == IssueDisposition::AcceptedAsIs {
        let current_effective = load_effective_field(&transaction, &request.scope, mapping)?;
        require_current_effective_hash(request, &current_effective)?;
    }
    insert_decision(&transaction, &attempt_id, &decision, None, None)?;
    append_attempt_event(&transaction, &attempt_id, "EFFECTIVE", None, None)?;
    transaction.commit()?;
    crate::decision_integrity::verify_decision_integrity_connection(&writer.connection)?;
    load_command_result(&writer.connection, &decision_id, false)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn complete_correction(
    writer: &WorkspaceWriter,
    request: &IssueDecisionRequest,
    request_hash: &str,
    attempt_id: &str,
    overlay: &CorrectionOverlay,
    reevaluation_id: &str,
    identity_hash: &str,
    authority: &AuthorityContext,
    effective: &EffectiveField,
    evaluation: &EffectiveValidationEvaluation,
    scope_json: &str,
    scope_hash: &str,
    created_at: &str,
) -> Result<DecisionCommandResult> {
    let completed_at = now_rfc3339();
    require_nondecreasing_timestamp(created_at, &completed_at)?;
    let evidence_issues = build_reevaluation_issues(
        &writer.connection,
        reevaluation_id,
        &request.scope.validation_run_id,
        evaluation,
    )?;
    let current_issue_count = u64::try_from(evaluation.issues.len())
        .map_err(|_| KernelError::DecisionBound("current issue count overflow".to_owned()))?;
    let resolved_count = count_outcome(&evidence_issues, "RESOLVED")?;
    let residual_count = count_outcome(&evidence_issues, "RESIDUAL")?;
    let new_count = count_outcome(&evidence_issues, "NEW")?;
    let result_value = reevaluation_result_material(
        reevaluation_id,
        identity_hash,
        &request.scope.validation_run_id,
        &request.scope.issue_id,
        scope_hash,
        evaluation.rows_processed,
        current_issue_count,
        resolved_count,
        residual_count,
        new_count,
        &evidence_issues,
    );
    let result_json = canonical_json_string(&result_value)?;
    let result_hash = canonical_hash(&result_value)?;
    let terminal_value = reevaluation_terminal_material(
        reevaluation_id,
        identity_hash,
        "COMPLETE",
        Some(&result_hash),
        u64::try_from(evidence_issues.len())
            .map_err(|_| KernelError::DecisionBound("evidence issue count overflow".to_owned()))?,
        evaluation.rows_processed,
        created_at,
        &completed_at,
        None,
        None,
        &evidence_issues,
    );
    let terminal_hash = canonical_hash(&terminal_value)?;

    let decision_id = internal_id("decision");
    let generation = request
        .concurrency
        .expected_active_decision_generation
        .checked_add(1)
        .ok_or_else(|| KernelError::DecisionBound("decision generation overflow".to_owned()))?;
    let mut decision = IssueDecision {
        schema_version: crate::decisions::DECISION_SCHEMA_VERSION,
        decision_id: decision_id.clone(),
        request_hash: request_hash.to_owned(),
        idempotency_key: request.idempotency_key.clone(),
        disposition: IssueDisposition::Corrected,
        scope: request.scope.clone(),
        concurrency: request.concurrency.clone(),
        practitioner_id: request.practitioner_id.clone(),
        application_timestamp: completed_at.clone(),
        reason: request.reason.clone(),
        evidence_reference: request.evidence_reference.clone(),
        authority_separation_acknowledged: request.authority_separation_acknowledged,
        row_scope_exclusion_acknowledged: false,
        active_generation: generation,
        correction_overlay_id: Some(overlay.overlay_id.clone()),
        reevaluation_id: Some(reevaluation_id.to_owned()),
        lifecycle_outcome: DecisionLifecycleOutcome::Effective,
        decision_hash: "0".repeat(64),
    };
    decision.decision_hash = canonical_decision_hash(&decision)?;
    decision.validate()?;

    let effective_value = overlay.effective_value().map(str::to_owned);
    let effective_hash = effective_value_hash(
        &request.scope,
        &effective.original_value_hash,
        Some(&effective.effective_value_hash),
        Some(&overlay.overlay_hash),
        generation,
        &effective_value,
    )?;
    let lineage = effective_lineage(
        &effective.original_lineage,
        &overlay.overlay_id,
        &overlay.overlay_hash,
        &effective.effective_value_hash,
        generation,
    );
    let lineage_json = canonical_json_string(&lineage)?;
    let lineage_hash = canonical_hash(&lineage)?;
    let effectivation_id = internal_id("effectivation");
    let effectivation_value = effectivation_material(
        &effectivation_id,
        &overlay.overlay_id,
        generation,
        &effective.original_value_hash,
        &effective.effective_value_hash,
        value_kind(&effective_value),
        &effective_value,
        &effective_hash,
        &lineage_hash,
    );
    let effectivation_hash = canonical_hash(&effectivation_value)?;

    let transaction = writer.connection.unchecked_transaction()?;
    require_current_head(&transaction, request)?;
    let commit_effective = load_effective_field(&transaction, &request.scope, &authority.mapping)?;
    require_current_effective_hash(request, &commit_effective)?;
    if commit_effective.effective_value_hash != effective.effective_value_hash {
        return Err(KernelError::DecisionConflict(
            "effective value changed before correction authority commit".to_owned(),
        ));
    }
    insert_complete_reevaluation(
        &transaction,
        request,
        attempt_id,
        &overlay.overlay_id,
        reevaluation_id,
        identity_hash,
        authority,
        scope_json,
        scope_hash,
        &result_json,
        &result_hash,
        &terminal_hash,
        evaluation.rows_processed,
        &evidence_issues,
        created_at,
        &completed_at,
    )?;
    append_attempt_event(
        &transaction,
        attempt_id,
        "REEVALUATION_COMPLETE",
        None,
        None,
    )?;
    insert_decision(
        &transaction,
        attempt_id,
        &decision,
        Some(&overlay.overlay_id),
        Some(reevaluation_id),
    )?;
    transaction.execute(
        "INSERT INTO correction_effectivations (
             effectivation_id, decision_id, decision_attempt_id,
             correction_overlay_id, reevaluation_id,
             decision_disposition, reevaluation_state, decision_generation,
             original_value_hash, prior_effective_value_hash,
             effective_value_kind, effective_value_text,
             effective_value_hash, effective_lineage_json, effective_lineage_hash,
             effectivation_hash, effectivated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'CORRECTED', 'COMPLETE', ?6,
                   ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            effectivation_id,
            decision_id,
            attempt_id,
            overlay.overlay_id,
            reevaluation_id,
            sqlite_i64(generation, "decision generation")?,
            effective.original_value_hash,
            effective.effective_value_hash,
            value_kind(&effective_value),
            effective_value,
            effective_hash,
            lineage_json,
            lineage_hash,
            effectivation_hash,
            completed_at,
        ],
    )?;
    append_attempt_event(&transaction, attempt_id, "EFFECTIVE", None, None)?;
    transaction.commit()?;
    crate::decision_integrity::verify_decision_integrity_connection(&writer.connection)?;
    load_command_result(&writer.connection, &decision_id, false)
}

fn insert_attempt(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    request: &IssueDecisionRequest,
    request_hash: &str,
    created_at: &str,
) -> Result<()> {
    let canonical_request_json =
        canonical_json_string(&canonical_decision_request_material(request)?)?;
    transaction.execute(
        "INSERT INTO decision_attempts (
             attempt_id, schema_version, idempotency_key, request_hash,
             canonical_request_json, job_id, validation_run_id, issue_id,
             issue_hash, accepted_source_file, source_ordinal, line_start, line_end,
             canonical_entity_id, canonical_field_id,
             profile_id, profile_version, profile_hash,
             mapping_id, mapping_version, mapping_hash,
             disposition, practitioner_id, reason, evidence_reference,
             authority_separation_acknowledged, row_scope_exclusion_acknowledged,
             expected_active_decision_id, expected_active_generation,
             expected_effective_value_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                   ?21, ?22, ?23, ?24, ?25, 1, ?26, ?27, ?28, ?29, ?30)",
        params![
            attempt_id,
            request.schema_version,
            request.idempotency_key,
            request_hash,
            canonical_request_json,
            request.scope.job_id,
            request.scope.validation_run_id,
            request.scope.issue_id,
            request.scope.issue_hash,
            request.scope.accepted_source_file,
            sqlite_i64(request.scope.source_ordinal, "source ordinal")?,
            sqlite_i64(request.scope.line_start, "line start")?,
            sqlite_i64(request.scope.line_end, "line end")?,
            request.scope.canonical_entity_id,
            request.scope.canonical_field_id,
            request.scope.profile_id,
            request.scope.profile_version,
            request.scope.profile_hash,
            request.scope.mapping_id,
            request.scope.mapping_version,
            request.scope.mapping_hash,
            request.disposition.as_str(),
            request.practitioner_id,
            request.reason,
            request.evidence_reference,
            i64::from(request.row_scope_exclusion_acknowledged),
            request.concurrency.expected_active_decision_id,
            sqlite_i64(
                request.concurrency.expected_active_decision_generation,
                "expected active generation"
            )?,
            request.concurrency.expected_effective_value_hash,
            created_at,
        ],
    )?;
    Ok(())
}

fn insert_overlay(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    overlay: &CorrectionOverlay,
) -> Result<()> {
    let replacement = match overlay.operation {
        CorrectionOperation::ReplaceEffectiveText => overlay.replacement_text.clone(),
        CorrectionOperation::SetEffectiveEmpty => Some(String::new()),
        CorrectionOperation::SetEffectiveNull => None,
    };
    transaction.execute(
        "INSERT INTO correction_overlays (
             overlay_id, schema_version, decision_attempt_id, decision_request_hash,
             disposition, prior_effective_value_hash, operation, replacement_text,
             practitioner_id, reason, canonical_overlay_json, overlay_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'CORRECTED', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            overlay.overlay_id,
            overlay.schema_version,
            attempt_id,
            overlay.decision_request_hash,
            overlay.prior_effective_value_hash,
            overlay.operation.as_str(),
            replacement,
            overlay.practitioner_id,
            overlay.reason,
            canonical_json_string(&canonical_overlay_material(overlay)?)?,
            overlay.overlay_hash,
            overlay.created_at,
        ],
    )?;
    Ok(())
}

fn insert_decision(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    decision: &IssueDecision,
    overlay_id: Option<&str>,
    reevaluation_id: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO issue_decisions (
             decision_id, schema_version, decision_attempt_id,
             job_id, validation_run_id, issue_id, disposition,
             predecessor_decision_id, predecessor_generation, generation,
             correction_overlay_id, reevaluation_id, reevaluation_state,
             practitioner_id, reason, evidence_reference, lifecycle_outcome,
             canonical_decision_json, decision_hash, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16, 'EFFECTIVE', ?17, ?18, ?19)",
        params![
            decision.decision_id,
            decision.schema_version,
            attempt_id,
            decision.scope.job_id,
            decision.scope.validation_run_id,
            decision.scope.issue_id,
            decision.disposition.as_str(),
            decision.concurrency.expected_active_decision_id,
            sqlite_i64(
                decision.concurrency.expected_active_decision_generation,
                "predecessor generation"
            )?,
            sqlite_i64(decision.active_generation, "decision generation")?,
            overlay_id,
            reevaluation_id,
            reevaluation_id.map(|_| "COMPLETE"),
            decision.practitioner_id,
            decision.reason,
            decision.evidence_reference,
            canonical_json_string(&canonical_decision_material(decision)?)?,
            decision.decision_hash,
            decision.application_timestamp,
        ],
    )?;
    Ok(())
}

fn append_attempt_event(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    let occurred_at = now_rfc3339();
    append_attempt_event_at(
        transaction,
        attempt_id,
        state,
        error_code,
        detail,
        &occurred_at,
    )
}

fn append_attempt_event_at(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    let previous: Option<String> = transaction
        .query_row(
            "SELECT event_hash FROM decision_attempt_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let material = decision_attempt_event_material(
        attempt_id,
        state,
        error_code,
        detail,
        previous.as_deref(),
        occurred_at,
    );
    transaction.execute(
        "INSERT INTO decision_attempt_events (
             attempt_id, state, error_code, detail,
             previous_event_hash, event_hash, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            attempt_id,
            state,
            error_code,
            detail,
            previous,
            canonical_hash(&material)?,
            occurred_at,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_complete_reevaluation(
    transaction: &Transaction<'_>,
    request: &IssueDecisionRequest,
    attempt_id: &str,
    overlay_id: &str,
    reevaluation_id: &str,
    identity_hash: &str,
    authority: &AuthorityContext,
    scope_json: &str,
    scope_hash: &str,
    result_json: &str,
    result_hash: &str,
    terminal_hash: &str,
    rows_processed: u64,
    issues: &[ReevaluationIssueRow],
    created_at: &str,
    completed_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO reevaluation_runs (
             reevaluation_id, identity_hash, decision_attempt_id,
             correction_overlay_id, disposition, job_id,
             source_validation_run_id, source_issue_id,
             rule_set_hash, engine_version, policy_json, policy_hash,
             dependency_scope_json, dependency_scope_hash, state,
             result_json, result_hash, terminal_hash, issue_count, rows_processed,
             created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'CORRECTED', ?5, ?6, ?7,
                   ?8, ?9, ?10, ?11, ?12, ?13, 'COMPLETE',
                   ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            reevaluation_id,
            identity_hash,
            attempt_id,
            overlay_id,
            request.scope.job_id,
            request.scope.validation_run_id,
            request.scope.issue_id,
            authority.rule_set_hash,
            authority.engine_version,
            authority.policy_json,
            authority.policy_hash,
            scope_json,
            scope_hash,
            result_json,
            result_hash,
            terminal_hash,
            sqlite_i64(
                u64::try_from(issues.len()).map_err(|_| KernelError::DecisionBound(
                    "re-evaluation evidence count overflow".to_owned()
                ))?,
                "re-evaluation issue count"
            )?,
            sqlite_i64(rows_processed, "re-evaluation row count")?,
            created_at,
            completed_at,
        ],
    )?;
    append_reevaluation_event(
        transaction,
        reevaluation_id,
        "PLANNED",
        None,
        None,
        created_at,
    )?;
    append_reevaluation_event(
        transaction,
        reevaluation_id,
        "RUNNING",
        None,
        None,
        created_at,
    )?;
    for issue in issues {
        insert_reevaluation_issue(transaction, reevaluation_id, issue)?;
    }
    append_reevaluation_event(
        transaction,
        reevaluation_id,
        "COMPLETE",
        None,
        None,
        completed_at,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_failed_reevaluation(
    writer: &WorkspaceWriter,
    request: &IssueDecisionRequest,
    attempt_id: &str,
    overlay_id: &str,
    reevaluation_id: &str,
    identity_hash: &str,
    authority: &AuthorityContext,
    scope_json: &str,
    scope_hash: &str,
    error: &KernelError,
    created_at: &str,
) -> Result<()> {
    let (state, attempt_state) = match error {
        KernelError::Cancelled(_) => ("INTERRUPTED", "REEVALUATION_INTERRUPTED"),
        KernelError::ValidationBound(_) | KernelError::DecisionBound(_) => {
            ("BOUND_EXCEEDED", "REEVALUATION_BOUND_EXCEEDED")
        }
        KernelError::DecisionIntegrity(_) | KernelError::IntegrityFailed(_) => {
            ("INTEGRITY_BLOCKED", "REEVALUATION_INTEGRITY_BLOCKED")
        }
        KernelError::DecisionConflict(_) => ("STALE_INPUT", "REEVALUATION_STALE_INPUT"),
        _ => ("FAILED", "REEVALUATION_FAILED"),
    };
    let completed_at = now_rfc3339();
    require_nondecreasing_timestamp(created_at, &completed_at)?;
    let error_message = bounded_error_message(error);
    let terminal_value = reevaluation_terminal_material(
        reevaluation_id,
        identity_hash,
        state,
        None,
        0,
        0,
        created_at,
        &completed_at,
        Some(error.code()),
        Some(&error_message),
        &[],
    );
    let transaction = writer.connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO reevaluation_runs (
             reevaluation_id, identity_hash, decision_attempt_id,
             correction_overlay_id, disposition, job_id,
             source_validation_run_id, source_issue_id,
             rule_set_hash, engine_version, policy_json, policy_hash,
             dependency_scope_json, dependency_scope_hash, state,
             terminal_hash, error_code, error_message,
             created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'CORRECTED', ?5, ?6, ?7,
                   ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            reevaluation_id,
            identity_hash,
            attempt_id,
            overlay_id,
            request.scope.job_id,
            request.scope.validation_run_id,
            request.scope.issue_id,
            authority.rule_set_hash,
            authority.engine_version,
            authority.policy_json,
            authority.policy_hash,
            scope_json,
            scope_hash,
            state,
            canonical_hash(&terminal_value)?,
            error.code(),
            error_message,
            created_at,
            completed_at,
        ],
    )?;
    append_reevaluation_event(
        &transaction,
        reevaluation_id,
        "PLANNED",
        None,
        None,
        created_at,
    )?;
    append_reevaluation_event(
        &transaction,
        reevaluation_id,
        "RUNNING",
        None,
        None,
        created_at,
    )?;
    append_reevaluation_event(
        &transaction,
        reevaluation_id,
        state,
        Some(error.code()),
        Some("bounded re-evaluation did not qualify for authority"),
        &completed_at,
    )?;
    append_attempt_event(
        &transaction,
        attempt_id,
        attempt_state,
        Some(error.code()),
        Some("no issue decision or effectivation was created"),
    )?;
    transaction.commit()?;
    Ok(())
}

/// Classify crash-aborted correction attempts whose inert overlay was durable
/// but whose re-evaluation terminal envelope was never appended.
///
/// # Errors
///
/// Fails closed if the pre-existing immutable evidence does not replay, the
/// canonical request or inherited validation authority is inconsistent, or
/// interrupted evidence cannot be appended atomically.
#[allow(clippy::too_many_lines)]
pub(crate) fn recover_interrupted_decision_attempts(
    transaction: &Transaction<'_>,
    recovery_time: &str,
) -> Result<usize> {
    type RecoveryRow = (
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
    crate::decision_integrity::verify_decision_integrity_connection(transaction)?;
    let candidates = {
        let mut statement = transaction.prepare(
            "SELECT a.attempt_id, a.request_hash, a.canonical_request_json, a.created_at,
                    o.overlay_id, vr.rule_set_hash, vr.engine_version,
                    vr.policy_json, vr.policy_hash
             FROM decision_attempts a
             JOIN correction_overlays o ON o.decision_attempt_id = a.attempt_id
             JOIN validation_runs vr ON vr.id = a.validation_run_id AND vr.job_id = a.job_id
             WHERE a.disposition = 'CORRECTED'
               AND NOT EXISTS (
                   SELECT 1 FROM reevaluation_runs r
                   WHERE r.decision_attempt_id = a.attempt_id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM issue_decisions d
                   WHERE d.decision_attempt_id = a.attempt_id
               )
               AND (
                   SELECT e.state FROM decision_attempt_events e
                   WHERE e.attempt_id = a.attempt_id
                   ORDER BY e.sequence DESC LIMIT 1
               ) = 'REEVALUATION_RUNNING'
             ORDER BY a.attempt_id",
        )?;
        statement
            .query_map([], |row| {
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
            })?
            .collect::<std::result::Result<Vec<RecoveryRow>, _>>()?
    };
    let recovery_timestamp = DateTime::parse_from_rfc3339(recovery_time).map_err(|error| {
        KernelError::DecisionIntegrity(format!("recovery timestamp is invalid: {error}"))
    })?;
    for (
        attempt_id,
        request_hash,
        canonical_request_json,
        created_at,
        overlay_id,
        rule_set_hash,
        engine_version,
        policy_json,
        policy_hash,
    ) in &candidates
    {
        let material = parse_unique_json(canonical_request_json.as_bytes()).map_err(|error| {
            KernelError::DecisionIntegrity(format!(
                "recovered decision request JSON is invalid: {error}"
            ))
        })?;
        let request: IssueDecisionRequest =
            serde_json::from_value(material.get("request").cloned().ok_or_else(|| {
                KernelError::DecisionIntegrity(
                    "recovered decision request material lacks request".to_owned(),
                )
            })?)?;
        request.validate()?;
        if canonical_decision_request_material(&request)? != material
            || canonical_json_string(&material)? != *canonical_request_json
            || canonical_decision_request_hash(&request)? != *request_hash
        {
            return Err(KernelError::DecisionIntegrity(
                "recovered decision request bytes/hash are inconsistent".to_owned(),
            ));
        }
        let policy_value = parse_unique_json(policy_json.as_bytes()).map_err(|error| {
            KernelError::DecisionIntegrity(format!(
                "recovered re-evaluation policy JSON is invalid: {error}"
            ))
        })?;
        if canonical_json_string(&policy_value)? != *policy_json
            || canonical_hash(&policy_value)? != *policy_hash
        {
            return Err(KernelError::DecisionIntegrity(
                "recovered re-evaluation policy bytes/hash are inconsistent".to_owned(),
            ));
        }
        let scope_value = reevaluation_scope_material(&request, "FULL_INHERITED_VALIDATION_SCOPE");
        let scope_json = canonical_json_string(&scope_value)?;
        let scope_hash = canonical_hash(&scope_value)?;
        let identity_value = reevaluation_identity_material(
            attempt_id,
            overlay_id,
            &request.scope.validation_run_id,
            &request.scope.issue_id,
            rule_set_hash,
            engine_version,
            policy_hash,
            &scope_hash,
        );
        let identity_hash = canonical_hash(&identity_value)?;
        let reevaluation_id = format!("reevaluation-recovered-{}", Uuid::new_v4());
        let created_timestamp = DateTime::parse_from_rfc3339(created_at).map_err(|error| {
            KernelError::DecisionIntegrity(format!(
                "recovered decision creation timestamp is invalid: {error}"
            ))
        })?;
        let completed_at = if recovery_timestamp < created_timestamp {
            created_at.clone()
        } else {
            recovery_time.to_owned()
        };
        let error_code = "INTERRUPTED_RECOVERED";
        let error_message =
            "process stopped after inert overlay persistence and before re-evaluation completion";
        let terminal_value = reevaluation_terminal_material(
            &reevaluation_id,
            &identity_hash,
            "INTERRUPTED",
            None,
            0,
            0,
            created_at,
            &completed_at,
            Some(error_code),
            Some(error_message),
            &[],
        );
        transaction.execute(
            "INSERT INTO reevaluation_runs (
                 reevaluation_id, identity_hash, decision_attempt_id,
                 correction_overlay_id, disposition, job_id,
                 source_validation_run_id, source_issue_id,
                 rule_set_hash, engine_version, policy_json, policy_hash,
                 dependency_scope_json, dependency_scope_hash, state,
                 terminal_hash, error_code, error_message,
                 created_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, 'CORRECTED', ?5, ?6, ?7,
                       ?8, ?9, ?10, ?11, ?12, ?13, 'INTERRUPTED',
                       ?14, ?15, ?16, ?17, ?18)",
            params![
                reevaluation_id,
                identity_hash,
                attempt_id,
                overlay_id,
                request.scope.job_id,
                request.scope.validation_run_id,
                request.scope.issue_id,
                rule_set_hash,
                engine_version,
                policy_json,
                policy_hash,
                scope_json,
                scope_hash,
                canonical_hash(&terminal_value)?,
                error_code,
                error_message,
                created_at,
                completed_at,
            ],
        )?;
        append_reevaluation_event(
            transaction,
            &reevaluation_id,
            "PLANNED",
            None,
            None,
            created_at,
        )?;
        append_reevaluation_event(
            transaction,
            &reevaluation_id,
            "RUNNING",
            None,
            None,
            created_at,
        )?;
        append_reevaluation_event(
            transaction,
            &reevaluation_id,
            "INTERRUPTED",
            Some(error_code),
            Some("bounded re-evaluation did not qualify for authority"),
            &completed_at,
        )?;
        append_attempt_event_at(
            transaction,
            attempt_id,
            "REEVALUATION_INTERRUPTED",
            Some(error_code),
            Some("no issue decision or effectivation was created"),
            &completed_at,
        )?;
    }
    crate::decision_integrity::verify_decision_integrity_connection(transaction)?;
    Ok(candidates.len())
}

fn append_reevaluation_event(
    transaction: &Transaction<'_>,
    reevaluation_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    let previous: Option<String> = transaction
        .query_row(
            "SELECT event_hash FROM reevaluation_events
             WHERE reevaluation_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [reevaluation_id],
            |row| row.get(0),
        )
        .optional()?;
    let material = reevaluation_event_material(
        reevaluation_id,
        state,
        error_code,
        detail,
        previous.as_deref(),
        occurred_at,
    );
    transaction.execute(
        "INSERT INTO reevaluation_events (
             reevaluation_id, state, error_code, detail,
             previous_event_hash, event_hash, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            reevaluation_id,
            state,
            error_code,
            detail,
            previous,
            canonical_hash(&material)?,
            occurred_at,
        ],
    )?;
    Ok(())
}

fn require_nondecreasing_timestamp(created_at: &str, completed_at: &str) -> Result<()> {
    let created = DateTime::parse_from_rfc3339(created_at).map_err(|error| {
        KernelError::DecisionIntegrity(format!("decision attempt timestamp is invalid: {error}"))
    })?;
    let completed = DateTime::parse_from_rfc3339(completed_at).map_err(|error| {
        KernelError::DecisionIntegrity(format!(
            "re-evaluation completion timestamp is invalid: {error}"
        ))
    })?;
    if completed < created {
        return Err(KernelError::DecisionIntegrity(
            "re-evaluation completion timestamp precedes immutable attempt evidence".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ReevaluationIssueRow {
    issue_id: String,
    outcome: String,
    source_validation_run_id: Option<String>,
    source_issue_id: Option<String>,
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
    issue_hash: String,
}

#[allow(clippy::too_many_lines)]
fn build_reevaluation_issues(
    connection: &Connection,
    reevaluation_id: &str,
    validation_run_id: &str,
    evaluation: &EffectiveValidationEvaluation,
) -> Result<Vec<ReevaluationIssueRow>> {
    let mut statement = connection.prepare(
        "SELECT issue_id, severity, rule_id, issue_code, display_message,
                entity_id, field_id, source_file, source_ordinal,
                line_start, line_end, lineage_json, issue_hash
         FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id",
    )?;
    let original = statement
        .query_map([validation_run_id], |row| {
            Ok(IssueEvidence {
                issue_id: row.get(0)?,
                severity: row.get(1)?,
                rule_id: row.get(2)?,
                issue_code: row.get(3)?,
                display_message: row.get(4)?,
                entity_id: row.get(5)?,
                field_id: row.get(6)?,
                source_file: row.get(7)?,
                source_ordinal: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(u64::MAX),
                line_start: u64::try_from(row.get::<_, i64>(9)?).unwrap_or(u64::MAX),
                line_end: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(u64::MAX),
                lineage_json: row.get(11)?,
                issue_hash: row.get(12)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let original_by_id = original
        .into_iter()
        .map(|issue| (issue.issue_id.clone(), issue))
        .collect::<BTreeMap<_, _>>();
    let current_by_id = evaluation
        .issues
        .iter()
        .cloned()
        .map(|issue| (issue.issue_id.clone(), issue))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(original_by_id.len().max(current_by_id.len()));
    for (issue_id, issue) in &current_by_id {
        let source = original_by_id.get(issue_id);
        rows.push(seal_reevaluation_issue(
            reevaluation_id,
            issue,
            if source.is_some() { "RESIDUAL" } else { "NEW" },
            source.map(|_| validation_run_id.to_owned()),
            source.map(|_| issue_id.clone()),
        )?);
    }
    for (issue_id, issue) in original_by_id {
        if !current_by_id.contains_key(&issue_id) {
            rows.push(seal_reevaluation_issue(
                reevaluation_id,
                &issue,
                "RESOLVED",
                Some(validation_run_id.to_owned()),
                Some(issue_id),
            )?);
        }
    }
    rows.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));
    Ok(rows)
}

fn seal_reevaluation_issue(
    reevaluation_id: &str,
    issue: &IssueEvidence,
    outcome: &str,
    source_validation_run_id: Option<String>,
    source_issue_id: Option<String>,
) -> Result<ReevaluationIssueRow> {
    let material = reevaluation_issue_material(
        reevaluation_id,
        &issue.issue_id,
        outcome,
        source_validation_run_id.as_deref(),
        source_issue_id.as_deref(),
        issue,
    )?;
    Ok(ReevaluationIssueRow {
        issue_id: issue.issue_id.clone(),
        outcome: outcome.to_owned(),
        source_validation_run_id,
        source_issue_id,
        severity: issue.severity.clone(),
        rule_id: issue.rule_id.clone(),
        issue_code: issue.issue_code.clone(),
        display_message: issue.display_message.clone(),
        entity_id: issue.entity_id.clone(),
        field_id: issue.field_id.clone(),
        source_file: issue.source_file.clone(),
        source_ordinal: issue.source_ordinal,
        line_start: issue.line_start,
        line_end: issue.line_end,
        lineage_json: issue.lineage_json.clone(),
        issue_hash: canonical_hash(&material)?,
    })
}

fn insert_reevaluation_issue(
    transaction: &Transaction<'_>,
    reevaluation_id: &str,
    issue: &ReevaluationIssueRow,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO reevaluation_issues (
             reevaluation_id, issue_id, outcome,
             source_validation_run_id, source_issue_id,
             severity, rule_id, issue_code, display_message,
             entity_id, field_id, source_file, source_ordinal,
             line_start, line_end, lineage_json, issue_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                   ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            reevaluation_id,
            issue.issue_id,
            issue.outcome,
            issue.source_validation_run_id,
            issue.source_issue_id,
            issue.severity,
            issue.rule_id,
            issue.issue_code,
            issue.display_message,
            issue.entity_id,
            issue.field_id,
            issue.source_file,
            sqlite_i64(issue.source_ordinal, "re-evaluation source ordinal")?,
            sqlite_i64(issue.line_start, "re-evaluation line start")?,
            sqlite_i64(issue.line_end, "re-evaluation line end")?,
            issue.lineage_json,
            issue.issue_hash,
        ],
    )?;
    Ok(())
}

pub(crate) fn load_command_result(
    connection: &Connection,
    decision_id: &str,
    idempotent_replay: bool,
) -> Result<DecisionCommandResult> {
    type ResultRow = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
    );
    let row: ResultRow = connection
        .query_row(
            "SELECT d.disposition, d.lifecycle_outcome,
                    d.correction_overlay_id, d.reevaluation_id, d.reevaluation_state,
                    COALESCE(SUM(CASE WHEN ri.outcome = 'RESIDUAL' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN ri.outcome = 'NEW' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN ri.outcome = 'RESIDUAL'
                        AND ri.issue_id = d.issue_id THEN 1 ELSE 0 END), 0)
             FROM issue_decisions d
             LEFT JOIN reevaluation_issues ri ON ri.reevaluation_id = d.reevaluation_id
             WHERE d.decision_id = ?1
             GROUP BY d.decision_id",
            [decision_id],
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
                ))
            },
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("decision {decision_id}")))?;
    let original_issue_present = row.0 != "CORRECTED" || row.7 > 0;
    let mut response = DecisionCommandResult {
        schema_version: COMMAND_RESULT_SCHEMA.to_owned(),
        data: DecisionCommandResultData {
            decision_id: decision_id.to_owned(),
            disposition: row.0,
            lifecycle_outcome: row.1,
            active: decision_is_active(connection, decision_id)?,
            idempotent_replay,
            correction_overlay_id: row.2,
            reevaluation_id: row.3,
            reevaluation_state: row.4.unwrap_or_else(|| "NOT_REQUIRED".to_owned()),
            original_issue_present,
            residual_issue_count: nonnegative_u64(row.5, "residual issue count")?,
            new_issue_count: nonnegative_u64(row.6, "new issue count")?,
            integrity_status: "VERIFIED".to_owned(),
        },
        canonical_hash: String::new(),
    };
    response.canonical_hash = response_hash(&response)?;
    Ok(response)
}

fn decision_is_active(connection: &Connection, decision_id: &str) -> Result<bool> {
    let active: i64 = connection.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM issue_decisions current
             WHERE current.decision_id = ?1
               AND current.generation = (
                   SELECT MAX(candidate.generation) FROM issue_decisions candidate
                   WHERE candidate.validation_run_id = current.validation_run_id
                     AND candidate.issue_id = current.issue_id
               )
         )",
        [decision_id],
        |row| row.get(0),
    )?;
    Ok(active == 1)
}

pub(crate) fn decision_attempt_event_material(
    attempt_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Value {
    json!({
        "contract": "migration-stream-decision-attempt-event/v1",
        "attemptId": attempt_id,
        "state": state,
        "errorCode": error_code,
        "detail": detail,
        "previousEventHash": previous_event_hash,
        "occurredAt": occurred_at,
    })
}

pub(crate) fn reevaluation_event_material(
    reevaluation_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Value {
    json!({
        "contract": "migration-stream-reevaluation-event/v1",
        "reevaluationId": reevaluation_id,
        "state": state,
        "errorCode": error_code,
        "detail": detail,
        "previousEventHash": previous_event_hash,
        "occurredAt": occurred_at,
    })
}

pub(crate) fn reevaluation_scope_material(
    request: &IssueDecisionRequest,
    scope_kind: &str,
) -> Value {
    json!({
        "contract": "migration-stream-reevaluation-scope/v1",
        "sourceValidationRunId": request.scope.validation_run_id,
        "sourceIssueId": request.scope.issue_id,
        "sourceFile": request.scope.accepted_source_file,
        "sourceOrdinal": request.scope.source_ordinal,
        "entityId": request.scope.canonical_entity_id,
        "fieldId": request.scope.canonical_field_id,
        "scopeKind": scope_kind,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn reevaluation_identity_material(
    attempt_id: &str,
    overlay_id: &str,
    source_validation_run_id: &str,
    source_issue_id: &str,
    rule_set_hash: &str,
    engine_version: &str,
    policy_hash: &str,
    dependency_scope_hash: &str,
) -> Value {
    json!({
        "contract": "migration-stream-reevaluation-identity/v1",
        "attemptId": attempt_id,
        "correctionOverlayId": overlay_id,
        "sourceValidationRunId": source_validation_run_id,
        "sourceIssueId": source_issue_id,
        "ruleSetHash": rule_set_hash,
        "engineVersion": engine_version,
        "policyHash": policy_hash,
        "dependencyScopeHash": dependency_scope_hash,
    })
}

fn reevaluation_issue_material(
    reevaluation_id: &str,
    issue_id: &str,
    outcome: &str,
    source_validation_run_id: Option<&str>,
    source_issue_id: Option<&str>,
    issue: &IssueEvidence,
) -> Result<Value> {
    let lineage = parse_unique_json(issue.lineage_json.as_bytes()).map_err(|error| {
        KernelError::DecisionIntegrity(format!(
            "re-evaluation issue lineage is not canonical JSON: {error}"
        ))
    })?;
    Ok(json!({
        "contract": "migration-stream-reevaluation-issue/v1",
        "reevaluationId": reevaluation_id,
        "issueId": issue_id,
        "outcome": outcome,
        "sourceValidationRunId": source_validation_run_id,
        "sourceIssueId": source_issue_id,
        "severity": issue.severity,
        "ruleId": issue.rule_id,
        "issueCode": issue.issue_code,
        "displayMessage": issue.display_message,
        "entityId": issue.entity_id,
        "fieldId": issue.field_id,
        "sourceFile": issue.source_file,
        "sourceOrdinal": issue.source_ordinal,
        "lineStart": issue.line_start,
        "lineEnd": issue.line_end,
        "lineage": lineage,
        "engineIssueHash": issue.issue_hash,
    }))
}

#[allow(clippy::too_many_arguments)]
fn reevaluation_result_material(
    reevaluation_id: &str,
    identity_hash: &str,
    source_validation_run_id: &str,
    source_issue_id: &str,
    scope_hash: &str,
    rows_processed: u64,
    current_issue_count: u64,
    resolved_count: u64,
    residual_count: u64,
    new_count: u64,
    issues: &[ReevaluationIssueRow],
) -> Value {
    let issue_summaries = issues
        .iter()
        .map(|issue| {
            json!({
                "issueId": issue.issue_id,
                "issueHash": issue.issue_hash,
                "outcome": issue.outcome,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "contract": "migration-stream-reevaluation-result/v1",
        "reevaluationId": reevaluation_id,
        "identityHash": identity_hash,
        "sourceValidationRunId": source_validation_run_id,
        "sourceIssueId": source_issue_id,
        "dependencyScopeHash": scope_hash,
        "rowsProcessed": rows_processed,
        "currentIssueCount": current_issue_count,
        "resolvedCount": resolved_count,
        "residualCount": residual_count,
        "newCount": new_count,
        "issues": issue_summaries,
    })
}

#[allow(clippy::too_many_arguments)]
fn reevaluation_terminal_material(
    reevaluation_id: &str,
    identity_hash: &str,
    state: &str,
    result_hash: Option<&str>,
    issue_count: u64,
    rows_processed: u64,
    created_at: &str,
    completed_at: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
    issues: &[ReevaluationIssueRow],
) -> Value {
    let issue_summaries = issues
        .iter()
        .map(|issue| {
            json!({
                "issueId": issue.issue_id,
                "issueHash": issue.issue_hash,
                "outcome": issue.outcome,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "contract": "migration-stream-reevaluation-terminal/v1",
        "reevaluationId": reevaluation_id,
        "identityHash": identity_hash,
        "state": state,
        "resultHash": result_hash,
        "issueCount": issue_count,
        "rowsProcessed": rows_processed,
        "createdAt": created_at,
        "completedAt": completed_at,
        "errorCode": error_code,
        "errorMessage": error_message,
        "issues": issue_summaries,
    })
}

fn count_outcome(issues: &[ReevaluationIssueRow], outcome: &str) -> Result<u64> {
    u64::try_from(
        issues
            .iter()
            .filter(|issue| issue.outcome == outcome)
            .count(),
    )
    .map_err(|_| KernelError::DecisionBound("re-evaluation outcome count overflow".to_owned()))
}

fn response_hash<T: Serialize>(response: &T) -> Result<String> {
    let mut value = serde_json::to_value(response)?;
    let object = value.as_object_mut().ok_or_else(|| {
        KernelError::DecisionIntegrity("command result must serialize as an object".to_owned())
    })?;
    object.insert("canonical_hash".to_owned(), Value::String(String::new()));
    canonical_hash(&value)
}

fn internal_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn sqlite_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::DecisionBound(format!("{label} does not fit SQLite INTEGER")))
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| KernelError::DecisionIntegrity(format!("{label} is negative")))
}

fn bounded_error_message(error: &KernelError) -> String {
    let value = error.to_string();
    value.chars().take(4_096).collect()
}

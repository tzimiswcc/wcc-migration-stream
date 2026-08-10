//! Governed revised-source validation inside one engagement.
//!
//! This module deliberately stops at a durable comparison of immutable
//! validation findings. It does not grant correction, decision, reconciliation,
//! result-approval, publication, target, or transmission authority.

#![allow(
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::wildcard_imports
)]

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;
use crate::{
    compared_validation::{
        COMPARED_FINDING_SET_SCHEMA, COMPARED_VALIDATION_INPUT_SCHEMA, ComparedFindingGroup,
        ComparedFindingSet, ComparedIdentityBasis, ComparedSubjectAlgorithm, ComparedSubjectKind,
        ComparedValidation, ComparedValidationAttribution, ComparedValidationCounts,
        ComparedValidationInput, ComparedValidationNamespaceKey, RelationshipRole,
        build_compared_finding_set, compare_finding_sets, verify_compared_finding_set_replay,
        verify_compared_validation_replay,
    },
    mapping::load_stored_mapping,
    validation::{
        compared_finding_set_input, run_validation_with_source_reuse_writer, validation_status,
        verify_validation_run_for_read,
    },
};

pub const START_REVISED_TOUR_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/start-revised-tour/v1";
pub const RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/reserve-revised-source-preflight/v1";
pub const ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/accept-revised-source/v1";
pub const LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/launch-revised-validation/v1";
pub const COMPARED_VALIDATION_SCHEMA: &str = "migration-stream/engagement/compared-validation/v1";
pub(crate) const MAPPING_SOURCE_REUSE_SCHEMA: &str = "migration-stream/mapping-source-reuse/v1";
const NORMALIZED_MAPPING_AUTHORITY_SCHEMA: &str =
    "migration-stream/engagement/mapping-authority/v1";
pub const REVISED_SOURCE_ACKNOWLEDGEMENT: &str =
    "migration-stream/revised-source-compared-validation/v1";
const TOUR_ATTEMPT_SCHEMA: &str = "migration-stream/engagement/tour-attempt/v1";
const REGISTERED_TOUR_SCHEMA: &str = "migration-stream/engagement/tour/v1";
const SOURCE_STRUCTURE_SCHEMA: &str = "migration-stream/source-structure/v1";
const TOUR_COMMAND_RESULT_SCHEMA: &str = "migration-stream/engagement/tour-command-result/v1";
const TOUR_CHAIN_SCHEMA: &str = "migration-stream/engagement/tour-chain/v1";
const COMPARISON_FINGERPRINT_ALGORITHM: &str =
    "migration-stream/compared-validation-fingerprint/v1";
const MAXIMUM_COMPARISON_PAGE_SIZE: usize = 200;
const MAXIMUM_TOURS_PER_ENGAGEMENT: u64 = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RevisedTourAttemptState {
    Started,
    PreflightReceiving,
    PreflightReady,
    SourceAccepted,
    ReadyToValidate,
    Validating,
    Complete,
    Incompatible,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRevisedTourCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub actor: String,
    pub reason: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveRevisedPreflightCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub tour_attempt_id: String,
    pub source_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptRevisedPreflightCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub tour_attempt_id: String,
    pub preflight_id: String,
    pub expected_source_hash: String,
    pub expected_manifest_hash: String,
    pub actor: String,
    pub reason: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchRevisedValidationCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub tour_attempt_id: String,
    pub expected_reuse_authority_id: String,
    pub expected_reuse_authority_hash: String,
    pub actor: String,
    pub reason: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TourSummaryView {
    pub tour_id: String,
    pub tour_number: u64,
    pub predecessor_tour_id: Option<String>,
    pub source_hash: String,
    pub manifest_hash: String,
    pub profiling_run_id: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub mapping_authority_id: String,
    pub mapping_authority_hash: String,
    pub validation_run_id: String,
    pub validation_result_hash: String,
    pub tour_hash: String,
    pub completed_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisedTourAttemptView {
    pub tour_attempt_id: String,
    pub tour_number: u64,
    pub baseline_tour_id: String,
    pub state: RevisedTourAttemptState,
    pub generation: u64,
    pub preflight_id: Option<String>,
    pub source_hash: Option<String>,
    pub manifest_hash: Option<String>,
    pub job_id: Option<String>,
    pub profiling_run_id: Option<String>,
    pub structure_hash: Option<String>,
    pub reuse_authority_id: Option<String>,
    pub reuse_authority_hash: Option<String>,
    pub validation_run_id: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingReuseAuthorityView {
    pub reuse_authority_id: String,
    pub reuse_authority_hash: String,
    pub baseline_tour_id: String,
    pub candidate_tour_attempt_id: String,
    pub baseline_source_hash: String,
    pub mapping_origin_source_hash: String,
    pub candidate_source_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub profile_approval_id: String,
    pub profile_approval_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub mapping_approval_id: Option<String>,
    pub mapping_approval_hash: Option<String>,
    pub mapping_authority_id: String,
    pub mapping_authority_hash: String,
    pub structure_hash: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComparedFindingCategory {
    Closed,
    Remaining,
    New,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingView {
    pub finding_id: String,
    pub category: ComparedFindingCategory,
    pub fingerprint_hash: String,
    pub occurrence: u64,
    pub identity_basis: ComparedIdentityBasis,
    pub identity_basis_explanation: String,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    pub relationship_id: Option<String>,
    pub relationship_role: Option<String>,
    pub baseline_issue_ids: Vec<String>,
    pub baseline_issue_hashes: Vec<String>,
    pub candidate_issue_ids: Vec<String>,
    pub candidate_issue_hashes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedValidationView {
    pub schema_version: String,
    pub comparison_id: String,
    pub baseline_tour_id: String,
    pub candidate_tour_id: String,
    pub baseline_validation_run_id: String,
    pub candidate_validation_run_id: String,
    pub baseline_result_hash: String,
    pub candidate_result_hash: String,
    pub algorithm_version: String,
    pub closed_count: u64,
    pub remaining_count: u64,
    pub new_count: u64,
    pub input_hash: String,
    pub comparison_hash: String,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
    pub findings: Vec<ComparedFindingView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TourChainView {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub active_tour: Option<TourSummaryView>,
    pub pending_attempt: Option<RevisedTourAttemptView>,
    pub latest_comparison: Option<ComparedValidationView>,
    pub tours: Vec<TourSummaryView>,
    pub comparisons: Vec<ComparedValidationView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptRevisedPreflightResult {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub tour_attempt: RevisedTourAttemptView,
    pub reuse_authority: MappingReuseAuthorityView,
    pub profiling_result_hash: String,
    pub reused_profile: bool,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchRevisedValidationResult {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub active_tour: TourSummaryView,
    pub comparison: ComparedValidationView,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceStructure {
    schema_version: String,
    files: Vec<SourceStructureFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceStructureFile {
    path: String,
    entity_id: Option<String>,
    headers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TourAttemptEvidence {
    schema_version: String,
    tour_attempt_id: String,
    engagement_id: String,
    planned_tour_id: String,
    tour_number: u64,
    predecessor_tour_id: String,
    actor: String,
    reason: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReuseCandidateEvidence {
    preflight_id: String,
    acceptance_attempt_id: String,
    job_id: String,
    profiling_run_id: String,
    source_hash: String,
    manifest_hash: String,
    profiling_result_hash: String,
    structure_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReuseBaselineEvidence {
    job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    predecessor_source_hash: Option<String>,
    mapping_source_hash: String,
    structure_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReuseProfileEvidence {
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    approval_id: String,
    approval_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReuseMappingEvidence {
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MappingSourceReuseEvidence {
    schema_version: String,
    source_reuse_id: String,
    engagement_id: String,
    tour_attempt_id: String,
    predecessor_tour_id: String,
    candidate: ReuseCandidateEvidence,
    baseline: ReuseBaselineEvidence,
    profile: ReuseProfileEvidence,
    mapping: ReuseMappingEvidence,
    verified_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisteredTourEvidence {
    schema_version: String,
    tour_id: String,
    engagement_id: String,
    tour_attempt_id: Option<String>,
    tour_number: u64,
    predecessor_tour_id: Option<String>,
    source_reuse_id: Option<String>,
    source_reuse_hash: Option<String>,
    preflight_id: String,
    acceptance_attempt_id: String,
    job_id: String,
    profiling_run_id: String,
    source_hash: String,
    manifest_hash: String,
    profiling_result_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping_approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping_approval_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping_authority_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping_authority_hash: Option<String>,
    validation_run_id: String,
    validation_identity_hash: String,
    validation_result_hash: String,
    registered_at: String,
}

#[derive(Clone, Debug)]
struct RegisteredTourRow {
    evidence: RegisteredTourEvidence,
    canonical_json: String,
    tour_hash: String,
    mapping_authority_id: String,
    mapping_authority_hash: String,
}

#[derive(Clone, Debug)]
struct TourAttemptRow {
    evidence: TourAttemptEvidence,
}

#[derive(Clone, Debug)]
struct ReuseAuthorityRow {
    evidence: MappingSourceReuseEvidence,
    source_reuse_hash: String,
    mapping_authority_id: String,
    mapping_authority_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum NormalizedMappingAuthorityKind {
    OperatorApproval,
    ControlledPreparation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NormalizedMappingAuthorityEvidence {
    schema_version: String,
    authority_id: String,
    authority_kind: NormalizedMappingAuthorityKind,
    engagement_id: String,
    job_id: String,
    source_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    mapping_approval_id: Option<String>,
    mapping_approval_hash: Option<String>,
    preparation_id: Option<String>,
    mapping_validation_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapping_validation_attempt_hash: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug)]
struct NormalizedMappingAuthorityRow {
    evidence: NormalizedMappingAuthorityEvidence,
    authority_hash: String,
}

#[derive(Clone, Debug)]
struct TourHeadRow {
    generation: u64,
    active_tour_id: Option<String>,
    pending_tour_attempt_id: Option<String>,
}

#[derive(Clone, Debug)]
struct TourCommandRow {
    command_kind: String,
    request_hash: String,
    result_generation: u64,
    result_reference: String,
    result_hash: String,
}

#[derive(Clone, Debug)]
struct ProfileApprovalIdentity {
    approval_id: String,
    approval_hash: String,
}

#[derive(Clone, Debug)]
struct FindingSetPersistence {
    finding_set_id: String,
    membership_hash: String,
    member_hashes: BTreeMap<(String, u64), String>,
}

#[derive(Clone)]
struct ComparisonNamespace {
    namespace_id: String,
    secret_hash: String,
    key: ComparedValidationNamespaceKey,
}

struct StoredComparisonIntegrityRow {
    comparison_id: String,
    baseline_tour_id: String,
    revised_tour_id: String,
    baseline_finding_set_id: String,
    baseline_finding_set_hash: String,
    baseline_membership_hash: String,
    revised_finding_set_id: String,
    revised_finding_set_hash: String,
    revised_membership_hash: String,
    namespace_id: String,
    namespace_secret_hash: String,
    algorithm_version: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    rule_set_hash: String,
    validation_engine_version: String,
    validation_policy_hash: String,
    closed_count: i64,
    remaining_count: i64,
    new_count: i64,
    canonical_comparison_json: String,
    comparison_hash: String,
    created_at: String,
}

struct BaselineSourceEvidence {
    preflight_id: String,
    acceptance_attempt_id: String,
    source_hash: String,
    manifest_hash: String,
    profiling_result_hash: String,
}

struct CompleteValidationIdentity {
    identity_hash: String,
    result_hash: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TourIntegritySummary {
    pub heads: usize,
    pub attempts: usize,
    pub commands: usize,
    pub reuse_authorities: usize,
    pub namespaces: usize,
    pub finding_sets: usize,
    pub finding_members: usize,
    pub comparisons: usize,
    pub comparison_items: usize,
}

impl EngagementService {
    /// Read the immutable tour chain and its latest value-suppressed comparison.
    pub fn tour_chain(&self) -> Result<TourChainView> {
        let connection = self.open_read()?;
        load_tour_chain(&connection, None)
    }

    /// Register the current completed validation as the first immutable
    /// revision when needed, then open the next bounded source-revision
    /// attempt. The current engagement remains authoritative.
    #[allow(clippy::too_many_lines)]
    pub fn start_revised_tour(&self, command: &StartRevisedTourCommand) -> Result<TourChainView> {
        require_schema(&command.schema_version, START_REVISED_TOUR_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_actor_reason(&command.actor, &command.reason)?;
        require_revised_acknowledgement(&command.acknowledgement)?;
        let canonical_request = canonical_command(command)?;
        let request_hash = sha256_hex(canonical_request.as_bytes());
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        if let Some(existing) = load_tour_command(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )? {
            require_tour_command_replay(&existing, "START_TOUR", &request_hash)?;
            return load_tour_chain(writer.connection(), None);
        }

        let state = load_engagement_state(writer.connection())?;
        if !phase_has_completed_validation(&state.phase)
            || state.profile_stage != "APPROVED"
            || state.mapping_stage != "VALIDATED"
            || state.validation_stage != "COMPLETE"
        {
            return Err(KernelError::Conflict(
                "a source revision requires an exact completed governed validation".to_owned(),
            ));
        }
        if let Some(head) = load_tour_head(writer.connection(), &engagement_id)? {
            if head.generation != command.expected_engagement_generation {
                return Err(KernelError::Conflict(format!(
                    "revised-tour start expected tour generation {}, found {}",
                    command.expected_engagement_generation, head.generation
                )));
            }
            if head.pending_tour_attempt_id.is_some() {
                return Err(KernelError::Conflict(
                    "an engagement tour attempt is already pending".to_owned(),
                ));
            }
            let active_tour_id = head.active_tour_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed("tour head has no active tour".to_owned())
            })?;
            let active_tour = load_registered_tour(writer.connection(), active_tour_id)?;
            require_active_tour_context(&state, &active_tour)?;
            if active_tour.evidence.tour_number >= MAXIMUM_TOURS_PER_ENGAGEMENT {
                return Err(KernelError::Conflict(format!(
                    "this engagement has reached its bounded revision limit of {MAXIMUM_TOURS_PER_ENGAGEMENT} tours"
                )));
            }
            let tour_number = active_tour
                .evidence
                .tour_number
                .checked_add(1)
                .ok_or_else(|| KernelError::PolicyInvalid("tour number overflow".to_owned()))?;
            let tour_attempt_id = format!("tour-attempt-{}", Uuid::new_v4());
            let planned_tour_id = format!("tour-{}", Uuid::new_v4());
            let now = now_rfc3339();
            let attempt_evidence = TourAttemptEvidence {
                schema_version: TOUR_ATTEMPT_SCHEMA.to_owned(),
                tour_attempt_id: tour_attempt_id.clone(),
                engagement_id: engagement_id.clone(),
                planned_tour_id,
                tour_number,
                predecessor_tour_id: active_tour_id.to_owned(),
                actor: command.actor.clone(),
                reason: command.reason.clone(),
                created_at: now.clone(),
            };
            let attempt_json = canonical_json_string(&serde_json::to_value(&attempt_evidence)?)?;
            let attempt_hash = sha256_hex(attempt_json.as_bytes());
            let result_generation = head
                .generation
                .checked_add(1)
                .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
            let transaction = writer.connection().unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO engagement_tour_attempts (
                     tour_attempt_id, engagement_id, planned_tour_id,
                     tour_number, predecessor_tour_id, started_by, reason,
                     canonical_attempt_json, attempt_hash, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    tour_attempt_id,
                    engagement_id,
                    attempt_evidence.planned_tour_id,
                    sqlite_tour_integer(tour_number, "tour number")?,
                    active_tour_id,
                    command.actor,
                    command.reason,
                    attempt_json,
                    attempt_hash,
                    now,
                ],
            )?;
            update_tour_head(
                &transaction,
                &engagement_id,
                head.generation,
                result_generation,
                Some(active_tour_id),
                Some(&tour_attempt_id),
                &now,
            )?;
            insert_tour_command(
                &transaction,
                &format!("tour-command-{}", Uuid::new_v4()),
                &engagement_id,
                None,
                "START_TOUR",
                &command.idempotency_key,
                &request_hash,
                &canonical_request,
                head.generation,
                result_generation,
                &tour_attempt_id,
                &attempt_hash,
                &now,
            )?;
            transaction.commit()?;
            return load_tour_chain(writer.connection(), None);
        }
        if state.generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(format!(
                "revised-tour start expected engagement generation {}, found {}",
                command.expected_engagement_generation, state.generation
            )));
        }

        let baseline_job_id = required_active(state.active.job_id.as_deref(), "active job")?;
        let baseline_profiling_run_id = required_active(
            state.active.profiling_run_id.as_deref(),
            "active profiling run",
        )?;
        let profile_id = required_active(state.active.profile_id.as_deref(), "active profile")?;
        let profile_version = required_active(
            state.active.profile_version.as_deref(),
            "active profile version",
        )?;
        let profile_hash =
            required_active(state.active.profile_hash.as_deref(), "active profile hash")?;
        let mapping_id = required_active(state.active.mapping_id.as_deref(), "active mapping")?;
        let mapping_version = required_active(
            state.active.mapping_version.as_deref(),
            "active mapping version",
        )?;
        let mapping_hash =
            required_active(state.active.mapping_hash.as_deref(), "active mapping hash")?;
        let validation_run_id = required_active(
            state.active.validation_run_id.as_deref(),
            "active validation run",
        )?;
        verify_validation_run_for_read(writer.connection(), validation_run_id)?;

        let mapping = load_stored_mapping(writer.connection(), mapping_id, mapping_version)?;
        if mapping.mapping_hash != mapping_hash
            || mapping.document.source.package_sha256
                != load_job_source_hash(writer.connection(), baseline_job_id)?
        {
            return Err(KernelError::IntegrityFailed(
                "active Tour 1 mapping/source authority differs from immutable evidence".to_owned(),
            ));
        }
        let profile_approval =
            exact_profile_approval(&self.workspace, profile_id, profile_version, profile_hash)?;
        let baseline_source = load_baseline_source_evidence(
            writer.connection(),
            &engagement_id,
            baseline_job_id,
            baseline_profiling_run_id,
        )?;
        let validation = load_complete_validation_identity(
            writer.connection(),
            validation_run_id,
            baseline_job_id,
            profile_id,
            profile_version,
            profile_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
        )?;

        let transaction = writer.connection().unchecked_transaction()?;
        let mapping_authority = ensure_normalized_mapping_authority(
            &transaction,
            &engagement_id,
            baseline_job_id,
            &baseline_source.source_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
        )?;
        let namespace = load_or_create_namespace(&transaction, &engagement_id)?;
        let baseline_tour_id = format!("tour-{}", Uuid::new_v4());
        let registered_at = now_rfc3339();
        let baseline_evidence = RegisteredTourEvidence {
            schema_version: REGISTERED_TOUR_SCHEMA.to_owned(),
            tour_id: baseline_tour_id.clone(),
            engagement_id: engagement_id.clone(),
            tour_attempt_id: None,
            tour_number: 1,
            predecessor_tour_id: None,
            source_reuse_id: None,
            source_reuse_hash: None,
            preflight_id: baseline_source.preflight_id,
            acceptance_attempt_id: baseline_source.acceptance_attempt_id,
            job_id: baseline_job_id.to_owned(),
            profiling_run_id: baseline_profiling_run_id.to_owned(),
            source_hash: baseline_source.source_hash,
            manifest_hash: baseline_source.manifest_hash,
            profiling_result_hash: baseline_source.profiling_result_hash,
            profile_id: profile_id.to_owned(),
            profile_version: profile_version.to_owned(),
            profile_hash: profile_hash.to_owned(),
            profile_approval_id: profile_approval.approval_id,
            profile_approval_hash: profile_approval.approval_hash,
            mapping_id: mapping_id.to_owned(),
            mapping_version: mapping_version.to_owned(),
            mapping_hash: mapping_hash.to_owned(),
            mapping_approval_id: mapping_authority.evidence.mapping_approval_id.clone(),
            mapping_approval_hash: mapping_authority.evidence.mapping_approval_hash.clone(),
            mapping_authority_id: Some(mapping_authority.evidence.authority_id.clone()),
            mapping_authority_hash: Some(mapping_authority.authority_hash.clone()),
            validation_run_id: validation_run_id.to_owned(),
            validation_identity_hash: validation.identity_hash,
            validation_result_hash: validation.result_hash,
            registered_at: registered_at.clone(),
        };
        let baseline_tour = canonical_registered_tour(baseline_evidence)?;

        let tour_attempt_id = format!("tour-attempt-{}", Uuid::new_v4());
        let planned_tour_id = format!("tour-{}", Uuid::new_v4());
        let attempt_evidence = TourAttemptEvidence {
            schema_version: TOUR_ATTEMPT_SCHEMA.to_owned(),
            tour_attempt_id: tour_attempt_id.clone(),
            engagement_id: engagement_id.clone(),
            planned_tour_id,
            tour_number: 2,
            predecessor_tour_id: baseline_tour_id.clone(),
            actor: command.actor.clone(),
            reason: command.reason.clone(),
            created_at: registered_at.clone(),
        };
        let attempt_json = canonical_json_string(&serde_json::to_value(&attempt_evidence)?)?;
        let attempt_hash = sha256_hex(attempt_json.as_bytes());
        let result_generation = command
            .expected_engagement_generation
            .checked_add(1)
            .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
        let command_attempt_id = format!("tour-command-{}", Uuid::new_v4());

        insert_registered_tour(&transaction, &baseline_tour)?;
        let baseline_input =
            compared_finding_set_input(&transaction, validation_run_id, &baseline_tour_id)?;
        let baseline_set = build_compared_finding_set(&namespace.key, &baseline_input)?;
        let _baseline_persistence = insert_finding_set(
            &transaction,
            &namespace,
            &baseline_tour_id,
            &baseline_set,
            &registered_at,
        )?;
        transaction.execute(
            "INSERT INTO engagement_tour_attempts (
                 tour_attempt_id, engagement_id, planned_tour_id, tour_number,
                 predecessor_tour_id, started_by, reason,
                 canonical_attempt_json, attempt_hash, created_at
             ) VALUES (?1, ?2, ?3, 2, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                tour_attempt_id,
                engagement_id,
                attempt_evidence.planned_tour_id,
                baseline_tour_id,
                command.actor,
                command.reason,
                attempt_json,
                attempt_hash,
                registered_at,
            ],
        )?;
        transaction.execute(
            "INSERT INTO engagement_tour_heads (
                 engagement_id, generation, active_tour_id,
                 pending_tour_attempt_id, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                engagement_id,
                sqlite_tour_integer(result_generation, "tour generation")?,
                baseline_tour_id,
                tour_attempt_id,
                registered_at,
            ],
        )?;
        insert_tour_command(
            &transaction,
            &command_attempt_id,
            &engagement_id,
            None,
            "START_TOUR",
            &command.idempotency_key,
            &request_hash,
            &canonical_request,
            command.expected_engagement_generation,
            result_generation,
            &tour_attempt_id,
            &attempt_hash,
            &registered_at,
        )?;
        transaction.commit()?;
        load_tour_chain(writer.connection(), None)
    }

    /// Reserve an isolated revised-source preflight without changing the
    /// currently active revision source.
    pub fn reserve_revised_preflight(
        &self,
        command: &ReserveRevisedPreflightCommand,
    ) -> Result<PreflightReservation> {
        require_schema(
            &command.schema_version,
            RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA,
        )?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.tour_attempt_id, "tour attempt ID")?;
        validate_source_name(&command.source_name)?;
        let canonical_request = canonical_command(command)?;
        let request_hash = sha256_hex(canonical_request.as_bytes());
        let policy_value = serde_json::to_value(&self.intake_policy)?;
        let policy_json = canonical_json_string(&policy_value)?;
        let policy_hash = canonical_hash(&policy_value)?;
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        if let Some(existing) = load_tour_command(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )? {
            require_tour_command_replay(&existing, "RESERVE_SOURCE", &request_hash)?;
            let row = load_preflight(writer.connection(), &existing.result_reference)?;
            drop(writer);
            let controlled_path = if row.state == "RECEIVING" {
                self.ensure_reserved_preflight_file(&row)?
            } else {
                controlled_preflight_path(&self.workspace, &row)?
            };
            return Ok(PreflightReservation {
                controlled_path,
                preflight: preflight_view(row)?,
            });
        }
        let head = require_pending_head(
            writer.connection(),
            &engagement_id,
            &command.tour_attempt_id,
            command.expected_engagement_generation,
        )?;
        ensure_no_tour_command_kind(
            writer.connection(),
            &command.tour_attempt_id,
            "RESERVE_SOURCE",
        )?;

        let preflight_id = Uuid::new_v4().to_string();
        let relative_path = format!("engagement/preflights/{preflight_id}.part");
        let now = now_rfc3339();
        let result_generation = head
            .generation
            .checked_add(1)
            .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
        let result_hash = canonical_hash(&json!({
            "schemaVersion": TOUR_COMMAND_RESULT_SCHEMA,
            "commandKind": "RESERVE_SOURCE",
            "tourAttemptId": command.tour_attempt_id,
            "preflightId": preflight_id,
            "generation": result_generation,
        }))?;
        let transaction = writer.connection().unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO engagement_source_preflights (
                 preflight_id, engagement_id, state, source_name,
                 idempotency_key, request_hash, canonical_request_json,
                 partial_relative_path, policy_json, policy_hash,
                 created_at, updated_at
             ) VALUES (?1, ?2, 'RECEIVING', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                preflight_id,
                engagement_id,
                command.source_name,
                command.idempotency_key,
                request_hash,
                canonical_request,
                relative_path,
                policy_json,
                policy_hash,
                now,
            ],
        )?;
        insert_preflight_event(&transaction, &preflight_id, "RECEIVING", None, None, &now)?;
        insert_tour_command(
            &transaction,
            &format!("tour-command-{}", Uuid::new_v4()),
            &engagement_id,
            Some(&command.tour_attempt_id),
            "RESERVE_SOURCE",
            &command.idempotency_key,
            &request_hash,
            &canonical_request,
            head.generation,
            result_generation,
            &preflight_id,
            &result_hash,
            &now,
        )?;
        update_tour_head(
            &transaction,
            &engagement_id,
            head.generation,
            result_generation,
            head.active_tour_id.as_deref(),
            Some(&command.tour_attempt_id),
            &now,
        )?;
        transaction.commit()?;
        let row = load_preflight(writer.connection(), &preflight_id)?;
        drop(writer);
        let controlled_path = match self.ensure_reserved_preflight_file(&row) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.fail_preflight(&PreflightTerminalCommand {
                    preflight_id: row.preflight_id.clone(),
                    error_code: "PREFLIGHT_RESERVATION_FAILED".to_owned(),
                    detail: "the controlled revised-source reservation could not be established"
                        .to_owned(),
                });
                return Err(error);
            }
        };
        Ok(PreflightReservation {
            preflight: preflight_view(row)?,
            controlled_path,
        })
    }

    /// Accept and sample-free profile a revised package, then independently
    /// prove whether the exact approved mapping authority may be reused.
    #[allow(clippy::too_many_lines)]
    pub fn accept_revised_preflight(
        &self,
        command: &AcceptRevisedPreflightCommand,
    ) -> Result<AcceptRevisedPreflightResult> {
        require_schema(
            &command.schema_version,
            ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA,
        )?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.tour_attempt_id, "tour attempt ID")?;
        validate_identifier(&command.preflight_id, "revised preflight ID")?;
        validate_sha256(&command.expected_source_hash, "revised source hash")?;
        validate_sha256(&command.expected_manifest_hash, "revised manifest hash")?;
        validate_actor_reason(&command.actor, &command.reason)?;
        require_revised_acknowledgement(&command.acknowledgement)?;
        let canonical_request = canonical_command(command)?;
        let request_hash = sha256_hex(canonical_request.as_bytes());

        {
            let connection = self.open_read()?;
            let engagement_id = require_engagement_id(&connection)?;
            if let Some(existing) =
                load_tour_command(&connection, &engagement_id, &command.idempotency_key)?
            {
                require_tour_command_replay(&existing, "ACCEPT_SOURCE", &request_hash)?;
                if existing.result_reference == "MAPPING_REQUIRED" {
                    return Err(KernelError::Conflict(
                        "revised source structure differs from the approved mapping; a new governed mapping is required"
                            .to_owned(),
                    ));
                }
                let reuse = load_reuse_authority(&connection, &existing.result_reference, true)?;
                let chain = load_tour_chain(&connection, None)?;
                let pending = chain.pending_attempt.ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "idempotent revised-source acceptance lost its pending tour".to_owned(),
                    )
                })?;
                return Ok(AcceptRevisedPreflightResult {
                    schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
                    engagement_generation: existing.result_generation,
                    tour_attempt: pending,
                    reuse_authority: reuse_authority_view(&reuse),
                    profiling_result_hash: reuse.evidence.candidate.profiling_result_hash.clone(),
                    reused_profile: true,
                    idempotent_replay: true,
                });
            }
            let head = require_pending_head(
                &connection,
                &engagement_id,
                &command.tour_attempt_id,
                command.expected_engagement_generation,
            )?;
            let reserved_preflight =
                load_tour_command_by_kind(&connection, &command.tour_attempt_id, "RESERVE_SOURCE")?
                    .ok_or_else(|| {
                        KernelError::Conflict(
                            "revised source acceptance requires its exact reserved preflight"
                                .to_owned(),
                        )
                    })?;
            if reserved_preflight.result_reference != command.preflight_id
                || head.pending_tour_attempt_id.as_deref() != Some(command.tour_attempt_id.as_str())
            {
                return Err(KernelError::Conflict(
                    "revised source acceptance differs from the pending tour reservation"
                        .to_owned(),
                ));
            }
        }

        let accepted = self.accept_preflight_evidence(
            &AcceptPreflightCommand {
                schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
                preflight_id: command.preflight_id.clone(),
                expected_source_hash: command.expected_source_hash.clone(),
                expected_manifest_hash: command.expected_manifest_hash.clone(),
                idempotency_key: command.idempotency_key.clone(),
                acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
            },
            false,
        )?;

        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        if let Some(existing) = load_tour_command(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )? {
            require_tour_command_replay(&existing, "ACCEPT_SOURCE", &request_hash)?;
            if existing.result_reference == "MAPPING_REQUIRED" {
                return Err(KernelError::Conflict(
                    "revised source structure differs from the approved mapping; a new governed mapping is required"
                        .to_owned(),
                ));
            }
            let reuse =
                load_reuse_authority(writer.connection(), &existing.result_reference, true)?;
            let chain = load_tour_chain(writer.connection(), None)?;
            return Ok(AcceptRevisedPreflightResult {
                schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
                engagement_generation: existing.result_generation,
                tour_attempt: chain.pending_attempt.ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "accepted revised source lost its pending tour".to_owned(),
                    )
                })?,
                reuse_authority: reuse_authority_view(&reuse),
                profiling_result_hash: accepted.result.profiling_result_hash,
                reused_profile: accepted.result.reused_profile,
                idempotent_replay: true,
            });
        }
        let head = require_pending_head(
            writer.connection(),
            &engagement_id,
            &command.tour_attempt_id,
            command.expected_engagement_generation,
        )?;
        ensure_no_tour_command_kind(
            writer.connection(),
            &command.tour_attempt_id,
            "ACCEPT_SOURCE",
        )?;
        let attempt = load_tour_attempt(writer.connection(), &command.tour_attempt_id)?;
        let baseline_tour =
            load_registered_tour(writer.connection(), &attempt.evidence.predecessor_tour_id)?;
        if accepted.result.job_id == baseline_tour.evidence.job_id
            || accepted.result.source_hash == baseline_tour.evidence.source_hash
        {
            return Err(KernelError::Conflict(
                "a revised validation tour requires a different accepted source package".to_owned(),
            ));
        }
        require_sample_free_profiling(
            writer.connection(),
            &accepted.result.job_id,
            &accepted.result.profiling_run_id,
            &accepted.result.source_hash,
            &accepted.result.profiling_result_hash,
        )?;

        let mapping = load_stored_mapping(
            writer.connection(),
            &baseline_tour.evidence.mapping_id,
            &baseline_tour.evidence.mapping_version,
        )?;
        if mapping.mapping_hash != baseline_tour.evidence.mapping_hash {
            return Err(KernelError::IntegrityFailed(
                "baseline tour mapping differs from immutable registration".to_owned(),
            ));
        }
        let baseline_structure = source_structure(
            writer.connection(),
            &baseline_tour.evidence.job_id,
            &mapping.document,
        )?;
        let candidate_structure = source_structure(
            writer.connection(),
            &accepted.result.job_id,
            &mapping.document,
        )?;
        let baseline_structure_json =
            canonical_json_string(&serde_json::to_value(&baseline_structure)?)?;
        let candidate_structure_json =
            canonical_json_string(&serde_json::to_value(&candidate_structure)?)?;
        let baseline_structure_hash = sha256_hex(baseline_structure_json.as_bytes());
        let candidate_structure_hash = sha256_hex(candidate_structure_json.as_bytes());
        if baseline_structure != candidate_structure
            || baseline_structure_hash != candidate_structure_hash
        {
            let now = now_rfc3339();
            let result_generation = head
                .generation
                .checked_add(1)
                .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
            let mismatch_hash = mapping_required_result_hash(
                &command.tour_attempt_id,
                &baseline_structure_hash,
                &candidate_structure_hash,
            )?;
            let transaction = writer.connection().unchecked_transaction()?;
            insert_tour_command(
                &transaction,
                &format!("tour-command-{}", Uuid::new_v4()),
                &engagement_id,
                Some(&command.tour_attempt_id),
                "ACCEPT_SOURCE",
                &command.idempotency_key,
                &request_hash,
                &canonical_request,
                head.generation,
                result_generation,
                "MAPPING_REQUIRED",
                &mismatch_hash,
                &now,
            )?;
            update_tour_head(
                &transaction,
                &engagement_id,
                head.generation,
                result_generation,
                head.active_tour_id.as_deref(),
                Some(&command.tour_attempt_id),
                &now,
            )?;
            transaction.commit()?;
            return Err(KernelError::Conflict(
                "revised source structure differs from the approved file/header/entity routing; a new governed mapping is required"
                    .to_owned(),
            ));
        }

        let profile_approval = exact_profile_approval(
            &self.workspace,
            &baseline_tour.evidence.profile_id,
            &baseline_tour.evidence.profile_version,
            &baseline_tour.evidence.profile_hash,
        )?;
        if profile_approval.approval_id != baseline_tour.evidence.profile_approval_id
            || profile_approval.approval_hash != baseline_tour.evidence.profile_approval_hash
        {
            return Err(KernelError::IntegrityFailed(
                "baseline tour profile approval differs from independent replay".to_owned(),
            ));
        }
        let transaction = writer.connection().unchecked_transaction()?;
        let mapping_authority = ensure_normalized_mapping_authority(
            &transaction,
            &engagement_id,
            &accepted.result.job_id,
            &accepted.result.source_hash,
            &baseline_tour.evidence.mapping_id,
            &baseline_tour.evidence.mapping_version,
            &baseline_tour.evidence.mapping_hash,
        )?;
        let source_reuse_id = format!("mapping-reuse-{}", Uuid::new_v4());
        let verified_at = now_rfc3339();
        let evidence = MappingSourceReuseEvidence {
            schema_version: MAPPING_SOURCE_REUSE_SCHEMA.to_owned(),
            source_reuse_id: source_reuse_id.clone(),
            engagement_id: engagement_id.clone(),
            tour_attempt_id: command.tour_attempt_id.clone(),
            predecessor_tour_id: baseline_tour.evidence.tour_id.clone(),
            candidate: ReuseCandidateEvidence {
                preflight_id: command.preflight_id.clone(),
                acceptance_attempt_id: accepted.acceptance_attempt_id.clone(),
                job_id: accepted.result.job_id.clone(),
                profiling_run_id: accepted.result.profiling_run_id.clone(),
                source_hash: accepted.result.source_hash.clone(),
                manifest_hash: command.expected_manifest_hash.clone(),
                profiling_result_hash: accepted.result.profiling_result_hash.clone(),
                structure_hash: candidate_structure_hash.clone(),
            },
            baseline: ReuseBaselineEvidence {
                job_id: baseline_tour.evidence.job_id.clone(),
                predecessor_source_hash: Some(baseline_tour.evidence.source_hash.clone()),
                mapping_source_hash: mapping.document.source.package_sha256.clone(),
                structure_hash: baseline_structure_hash.clone(),
            },
            profile: ReuseProfileEvidence {
                profile_id: baseline_tour.evidence.profile_id.clone(),
                profile_version: baseline_tour.evidence.profile_version.clone(),
                profile_hash: baseline_tour.evidence.profile_hash.clone(),
                approval_id: profile_approval.approval_id,
                approval_hash: profile_approval.approval_hash,
            },
            mapping: ReuseMappingEvidence {
                mapping_id: baseline_tour.evidence.mapping_id.clone(),
                mapping_version: baseline_tour.evidence.mapping_version.clone(),
                mapping_hash: baseline_tour.evidence.mapping_hash.clone(),
                approval_id: mapping_authority.evidence.mapping_approval_id.clone(),
                approval_hash: mapping_authority.evidence.mapping_approval_hash.clone(),
                authority_id: Some(mapping_authority.evidence.authority_id.clone()),
                authority_hash: Some(mapping_authority.authority_hash.clone()),
            },
            verified_at: verified_at.clone(),
        };
        let canonical_reuse_json = canonical_json_string(&serde_json::to_value(&evidence)?)?;
        let source_reuse_hash = sha256_hex(canonical_reuse_json.as_bytes());
        let result_generation = head
            .generation
            .checked_add(1)
            .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
        transaction.execute(
            "INSERT INTO engagement_mapping_reuse_authorities (
                 source_reuse_id, engagement_id, tour_attempt_id,
                 predecessor_tour_id, preflight_id, acceptance_attempt_id,
                 candidate_job_id, candidate_profiling_run_id,
                 candidate_source_hash, candidate_manifest_hash,
                 candidate_profiling_result_hash, baseline_job_id,
                 mapping_source_hash, structure_json,
                 baseline_structure_hash, candidate_structure_hash,
                 profile_id, profile_version, profile_hash,
                 profile_approval_id, profile_approval_hash,
                 mapping_id, mapping_version, mapping_hash,
                 mapping_approval_id, mapping_approval_hash,
                 algorithm_version, canonical_reuse_json,
                 source_reuse_hash, verified_at,
                 mapping_authority_id, mapping_authority_hash
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                 ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                 'migration-stream/mapping-source-reuse/v1', ?27, ?28, ?29,
                 ?30, ?31
             )",
            params![
                source_reuse_id,
                engagement_id,
                command.tour_attempt_id,
                baseline_tour.evidence.tour_id,
                command.preflight_id,
                accepted.acceptance_attempt_id,
                accepted.result.job_id,
                accepted.result.profiling_run_id,
                accepted.result.source_hash,
                command.expected_manifest_hash,
                accepted.result.profiling_result_hash,
                baseline_tour.evidence.job_id,
                mapping.document.source.package_sha256,
                candidate_structure_json,
                baseline_structure_hash,
                candidate_structure_hash,
                evidence.profile.profile_id,
                evidence.profile.profile_version,
                evidence.profile.profile_hash,
                evidence.profile.approval_id,
                evidence.profile.approval_hash,
                evidence.mapping.mapping_id,
                evidence.mapping.mapping_version,
                evidence.mapping.mapping_hash,
                evidence.mapping.approval_id,
                evidence.mapping.approval_hash,
                canonical_reuse_json,
                source_reuse_hash,
                verified_at,
                mapping_authority.evidence.authority_id,
                mapping_authority.authority_hash,
            ],
        )?;
        insert_tour_command(
            &transaction,
            &format!("tour-command-{}", Uuid::new_v4()),
            &engagement_id,
            Some(&command.tour_attempt_id),
            "ACCEPT_SOURCE",
            &command.idempotency_key,
            &request_hash,
            &canonical_request,
            head.generation,
            result_generation,
            &source_reuse_id,
            &source_reuse_hash,
            &verified_at,
        )?;
        update_tour_head(
            &transaction,
            &engagement_id,
            head.generation,
            result_generation,
            head.active_tour_id.as_deref(),
            Some(&command.tour_attempt_id),
            &verified_at,
        )?;
        transaction.commit()?;

        let reuse = load_reuse_authority(writer.connection(), &source_reuse_id, true)?;
        let chain = load_tour_chain(writer.connection(), None)?;
        Ok(AcceptRevisedPreflightResult {
            schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
            engagement_generation: result_generation,
            tour_attempt: chain.pending_attempt.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "accepted revised source lost its pending tour".to_owned(),
                )
            })?,
            reuse_authority: reuse_authority_view(&reuse),
            profiling_result_hash: accepted.result.profiling_result_hash,
            reused_profile: accepted.result.reused_profile,
            idempotent_replay: false,
        })
    }

    /// Validate the accepted revised source under the independently replayed
    /// exact profile/mapping authority, persist the compared multiset, and
    /// advance the active tour only in the final evidence transaction.
    #[allow(clippy::too_many_lines)]
    pub fn launch_revised_validation(
        &self,
        command: &LaunchRevisedValidationCommand,
    ) -> Result<LaunchRevisedValidationResult> {
        require_schema(
            &command.schema_version,
            LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA,
        )?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.tour_attempt_id, "tour attempt ID")?;
        validate_identifier(
            &command.expected_reuse_authority_id,
            "mapping reuse authority ID",
        )?;
        validate_sha256(
            &command.expected_reuse_authority_hash,
            "mapping reuse authority hash",
        )?;
        validate_actor_reason(&command.actor, &command.reason)?;
        require_revised_acknowledgement(&command.acknowledgement)?;
        let canonical_request = canonical_command(command)?;
        let request_hash = sha256_hex(canonical_request.as_bytes());

        let (writer, run_id, source_reuse, idempotent_attempt) = {
            let writer = self.open_writer()?;
            let engagement_id = require_engagement_id(writer.connection())?;
            if let Some(completed) =
                load_completed_launch_result(writer.connection(), &command.tour_attempt_id)?
            {
                let existing = load_tour_command(
                    writer.connection(),
                    &engagement_id,
                    &command.idempotency_key,
                )?
                .ok_or_else(|| {
                    KernelError::Conflict(
                        "completed revised tour belongs to a different launch identity".to_owned(),
                    )
                })?;
                require_tour_command_replay(&existing, "LAUNCH_VALIDATION", &request_hash)?;
                return Ok(LaunchRevisedValidationResult {
                    schema_version: LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA.to_owned(),
                    engagement_generation: load_tour_head(writer.connection(), &engagement_id)?
                        .ok_or_else(|| {
                            KernelError::IntegrityFailed(
                                "completed tour has no active tour head".to_owned(),
                            )
                        })?
                        .generation,
                    active_tour: completed.0,
                    comparison: completed.1,
                    idempotent_replay: true,
                });
            }

            if let Some(existing) = load_tour_command(
                writer.connection(),
                &engagement_id,
                &command.idempotency_key,
            )? {
                require_tour_command_replay(&existing, "LAUNCH_VALIDATION", &request_hash)?;
                let reuse = load_reuse_authority(
                    writer.connection(),
                    &command.expected_reuse_authority_id,
                    true,
                )?;
                if reuse.source_reuse_hash != command.expected_reuse_authority_hash
                    || existing.result_reference.is_empty()
                {
                    return Err(KernelError::Conflict(
                        "validation retry differs from the exact approved source-reuse authority"
                            .to_owned(),
                    ));
                }
                (writer, existing.result_reference, reuse, true)
            } else {
                let head = require_pending_head(
                    writer.connection(),
                    &engagement_id,
                    &command.tour_attempt_id,
                    command.expected_engagement_generation,
                )?;
                ensure_no_tour_command_kind(
                    writer.connection(),
                    &command.tour_attempt_id,
                    "LAUNCH_VALIDATION",
                )?;
                let reuse = load_reuse_authority(
                    writer.connection(),
                    &command.expected_reuse_authority_id,
                    true,
                )?;
                if reuse.source_reuse_hash != command.expected_reuse_authority_hash
                    || reuse.evidence.tour_attempt_id != command.tour_attempt_id
                {
                    return Err(KernelError::Conflict(
                        "validation request differs from the pending tour's exact source-reuse authority"
                        .to_owned(),
                    ));
                }
                let active_tour_id = head.active_tour_id.as_deref().ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "pending revised validation has no active predecessor tour".to_owned(),
                    )
                })?;
                if active_tour_id != reuse.evidence.predecessor_tour_id {
                    return Err(KernelError::IntegrityFailed(
                        "pending revised validation predecessor differs from the active tour head"
                            .to_owned(),
                    ));
                }
                require_exact_active_tour_context(
                    self,
                    writer.connection(),
                    &engagement_id,
                    active_tour_id,
                )?;
                require_reused_validation_semantics(
                    writer.connection(),
                    &reuse,
                    &self.validation_policy,
                )?;
                let run_id = Uuid::new_v4().to_string();
                let result_generation = head.generation.checked_add(1).ok_or_else(|| {
                    KernelError::PolicyInvalid("tour generation overflow".to_owned())
                })?;
                let launch_plan_hash = canonical_hash(&json!({
                    "schemaVersion": TOUR_COMMAND_RESULT_SCHEMA,
                    "commandKind": "LAUNCH_VALIDATION",
                    "tourAttemptId": command.tour_attempt_id,
                    "runId": run_id,
                    "sourceReuseId": reuse.evidence.source_reuse_id,
                    "sourceReuseHash": reuse.source_reuse_hash,
                    "generation": result_generation,
                }))?;
                let now = now_rfc3339();
                let transaction = writer.connection().unchecked_transaction()?;
                insert_tour_command(
                    &transaction,
                    &format!("tour-command-{}", Uuid::new_v4()),
                    &engagement_id,
                    Some(&command.tour_attempt_id),
                    "LAUNCH_VALIDATION",
                    &command.idempotency_key,
                    &request_hash,
                    &canonical_request,
                    head.generation,
                    result_generation,
                    &run_id,
                    &launch_plan_hash,
                    &now,
                )?;
                update_tour_head(
                    &transaction,
                    &engagement_id,
                    head.generation,
                    result_generation,
                    head.active_tour_id.as_deref(),
                    Some(&command.tour_attempt_id),
                    &now,
                )?;
                transaction.commit()?;
                (writer, run_id, reuse, false)
            }
        };

        require_reused_validation_semantics(
            writer.connection(),
            &source_reuse,
            &self.validation_policy,
        )?;
        require_exact_active_tour_context(
            self,
            writer.connection(),
            &source_reuse.evidence.engagement_id,
            &source_reuse.evidence.predecessor_tour_id,
        )?;
        let validation_report = run_validation_with_source_reuse_writer(
            &writer,
            &run_id,
            &source_reuse.evidence.candidate.job_id,
            &source_reuse.evidence.mapping.mapping_id,
            &source_reuse.evidence.mapping.mapping_version,
            &source_reuse.evidence.source_reuse_id,
            &source_reuse.source_reuse_hash,
            &self.profile_schema_path,
            &self.validation_policy,
        )?;
        if validation_report.run_id != run_id {
            return Err(KernelError::IntegrityFailed(
                "revised validation returned a different preallocated run identity".to_owned(),
            ));
        }

        let engagement_id = require_engagement_id(writer.connection())?;
        if let Some(completed) =
            load_completed_launch_result(writer.connection(), &command.tour_attempt_id)?
        {
            return Ok(LaunchRevisedValidationResult {
                schema_version: LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA.to_owned(),
                engagement_generation: load_tour_head(writer.connection(), &engagement_id)?
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed("completed tour has no head".to_owned())
                    })?
                    .generation,
                active_tour: completed.0,
                comparison: completed.1,
                idempotent_replay: true,
            });
        }
        let launch_attempt = load_tour_command(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )?
        .ok_or_else(|| {
            KernelError::IntegrityFailed("revised validation launch evidence vanished".to_owned())
        })?;
        require_tour_command_replay(&launch_attempt, "LAUNCH_VALIDATION", &request_hash)?;
        let head = require_pending_head(
            writer.connection(),
            &engagement_id,
            &command.tour_attempt_id,
            launch_attempt.result_generation,
        )?;
        let attempt = load_tour_attempt(writer.connection(), &command.tour_attempt_id)?;
        let reuse = load_reuse_authority(
            writer.connection(),
            &command.expected_reuse_authority_id,
            true,
        )?;
        if reuse.source_reuse_hash != command.expected_reuse_authority_hash
            || reuse.evidence != source_reuse.evidence
        {
            return Err(KernelError::IntegrityFailed(
                "mapping reuse authority changed between launch and completion".to_owned(),
            ));
        }
        verify_validation_run_for_read(writer.connection(), &run_id)?;
        let status = validation_status(&self.workspace, &run_id)?;
        if status.state != crate::validation::ValidationRunState::Complete
            || status.result_hash.as_deref() != Some(validation_report.result_hash.as_str())
            || status.source_hash != reuse.evidence.candidate.source_hash
            || status.mapping_hash != reuse.evidence.mapping.mapping_hash
            || status.profile_hash != reuse.evidence.profile.profile_hash
        {
            return Err(KernelError::IntegrityFailed(
                "revised validation completion differs from exact reused authority".to_owned(),
            ));
        }

        let baseline_tour =
            load_registered_tour(writer.connection(), &attempt.evidence.predecessor_tour_id)?;
        if head.active_tour_id.as_deref() != Some(baseline_tour.evidence.tour_id.as_str()) {
            return Err(KernelError::IntegrityFailed(
                "pending revised validation no longer targets the exact active predecessor tour"
                    .to_owned(),
            ));
        }
        let baseline_context = require_exact_active_tour_context(
            self,
            writer.connection(),
            &engagement_id,
            &baseline_tour.evidence.tour_id,
        )?;
        let namespace = load_namespace(writer.connection(), &engagement_id)?;
        let baseline_set =
            load_finding_set_for_tour(writer.connection(), &baseline_tour.evidence.tour_id)?;
        let baseline_input = compared_finding_set_input(
            writer.connection(),
            &baseline_tour.evidence.validation_run_id,
            &baseline_tour.evidence.tour_id,
        )?;
        verify_compared_finding_set_replay(&namespace.key, &baseline_input, &baseline_set)?;

        let registered_at = now_rfc3339();
        let candidate_tour = canonical_registered_tour(RegisteredTourEvidence {
            schema_version: REGISTERED_TOUR_SCHEMA.to_owned(),
            tour_id: attempt.evidence.planned_tour_id.clone(),
            engagement_id: engagement_id.clone(),
            tour_attempt_id: Some(attempt.evidence.tour_attempt_id.clone()),
            tour_number: attempt.evidence.tour_number,
            predecessor_tour_id: Some(baseline_tour.evidence.tour_id.clone()),
            source_reuse_id: Some(reuse.evidence.source_reuse_id.clone()),
            source_reuse_hash: Some(reuse.source_reuse_hash.clone()),
            preflight_id: reuse.evidence.candidate.preflight_id.clone(),
            acceptance_attempt_id: reuse.evidence.candidate.acceptance_attempt_id.clone(),
            job_id: reuse.evidence.candidate.job_id.clone(),
            profiling_run_id: reuse.evidence.candidate.profiling_run_id.clone(),
            source_hash: reuse.evidence.candidate.source_hash.clone(),
            manifest_hash: reuse.evidence.candidate.manifest_hash.clone(),
            profiling_result_hash: reuse.evidence.candidate.profiling_result_hash.clone(),
            profile_id: reuse.evidence.profile.profile_id.clone(),
            profile_version: reuse.evidence.profile.profile_version.clone(),
            profile_hash: reuse.evidence.profile.profile_hash.clone(),
            profile_approval_id: reuse.evidence.profile.approval_id.clone(),
            profile_approval_hash: reuse.evidence.profile.approval_hash.clone(),
            mapping_id: reuse.evidence.mapping.mapping_id.clone(),
            mapping_version: reuse.evidence.mapping.mapping_version.clone(),
            mapping_hash: reuse.evidence.mapping.mapping_hash.clone(),
            mapping_approval_id: reuse.evidence.mapping.approval_id.clone(),
            mapping_approval_hash: reuse.evidence.mapping.approval_hash.clone(),
            mapping_authority_id: Some(reuse.mapping_authority_id.clone()),
            mapping_authority_hash: Some(reuse.mapping_authority_hash.clone()),
            validation_run_id: run_id.clone(),
            validation_identity_hash: validation_report.identity_hash,
            validation_result_hash: validation_report.result_hash,
            registered_at: registered_at.clone(),
        })?;

        let transaction = writer.connection().unchecked_transaction()?;
        insert_registered_tour(&transaction, &candidate_tour)?;
        let candidate_input =
            compared_finding_set_input(&transaction, &run_id, &attempt.evidence.planned_tour_id)?;
        let candidate_set = build_compared_finding_set(&namespace.key, &candidate_input)?;
        let comparison_input = ComparedValidationInput {
            schema_version: COMPARED_VALIDATION_INPUT_SCHEMA.to_owned(),
            prior: baseline_set.clone(),
            current: candidate_set.clone(),
            attribution: ComparedValidationAttribution {
                actor: command.actor.clone(),
                reason: command.reason.clone(),
            },
        };
        let comparison = compare_finding_sets(&comparison_input)?;
        verify_compared_validation_replay(&comparison_input, &comparison)?;
        let candidate_persistence = insert_finding_set(
            &transaction,
            &namespace,
            &candidate_tour.evidence.tour_id,
            &candidate_set,
            &registered_at,
        )?;
        let baseline_persistence = load_finding_set_persistence(
            &transaction,
            &namespace,
            &baseline_tour.evidence.tour_id,
            &baseline_set,
        )?;
        insert_comparison(
            &transaction,
            &engagement_id,
            &namespace,
            &baseline_tour,
            &candidate_tour,
            &baseline_set,
            &candidate_set,
            &baseline_persistence,
            &candidate_persistence,
            &comparison,
            &registered_at,
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_active_context
             SET current_preflight_id = ?1, active_job_id = ?2,
                 active_profiling_run_id = ?3,
                 active_validation_run_id = ?4,
                 validation_stage = 'COMPLETE',
                 generation = generation + 1, updated_at = ?5
             WHERE engagement_id = ?6 AND generation = ?7
               AND current_preflight_id IS ?8
               AND active_job_id IS ?9
               AND active_profiling_run_id IS ?10
               AND selected_object_pack_id IS ?11
               AND destination_origin IS ?12
               AND destination_authority_id IS ?13
               AND destination_origin_hash IS ?14
               AND selected_profile_id IS ?15
               AND selected_profile_version IS ?16
               AND selected_profile_hash IS ?17
               AND profile_stage = ?18
               AND active_mapping_id IS ?19
               AND active_mapping_version IS ?20
               AND active_mapping_hash IS ?21
               AND mapping_stage = ?22
               AND active_validation_run_id IS ?23
               AND validation_stage = ?24",
            params![
                candidate_tour.evidence.preflight_id,
                candidate_tour.evidence.job_id,
                candidate_tour.evidence.profiling_run_id,
                candidate_tour.evidence.validation_run_id,
                registered_at,
                engagement_id,
                sqlite_tour_integer(
                    baseline_context.generation,
                    "baseline active-context generation"
                )?,
                baseline_context.active.preflight_id,
                baseline_context.active.job_id,
                baseline_context.active.profiling_run_id,
                baseline_context.active.object_pack_id,
                baseline_context.active.destination_origin,
                baseline_context.active.destination_authority_id,
                baseline_context.active.destination_origin_hash,
                baseline_context.active.profile_id,
                baseline_context.active.profile_version,
                baseline_context.active.profile_hash,
                baseline_context.profile_stage,
                baseline_context.active.mapping_id,
                baseline_context.active.mapping_version,
                baseline_context.active.mapping_hash,
                baseline_context.mapping_stage,
                baseline_context.active.validation_run_id,
                baseline_context.validation_stage,
            ],
        )?;
        require_one(
            changed,
            "advance active engagement to revised validation tour",
        )?;
        let final_generation = head
            .generation
            .checked_add(1)
            .ok_or_else(|| KernelError::PolicyInvalid("tour generation overflow".to_owned()))?;
        update_tour_head(
            &transaction,
            &engagement_id,
            head.generation,
            final_generation,
            Some(&candidate_tour.evidence.tour_id),
            None,
            &registered_at,
        )?;
        transaction.commit()?;

        let completed =
            load_completed_launch_result(writer.connection(), &command.tour_attempt_id)?
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "committed revised validation is not readable from its immutable chain"
                            .to_owned(),
                    )
                })?;
        Ok(LaunchRevisedValidationResult {
            schema_version: LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA.to_owned(),
            engagement_generation: final_generation,
            active_tour: completed.0,
            comparison: completed.1,
            idempotent_replay: idempotent_attempt,
        })
    }
}

fn validate_actor_reason(actor: &str, reason: &str) -> Result<()> {
    validate_bounded_text(actor, "tour actor", MAXIMUM_IDENTIFIER_BYTES)?;
    validate_bounded_text(reason, "tour reason", MAXIMUM_REASON_BYTES)
}

fn require_revised_acknowledgement(value: &str) -> Result<()> {
    if value != REVISED_SOURCE_ACKNOWLEDGEMENT {
        return Err(KernelError::PolicyInvalid(format!(
            "revised-source validation requires acknowledgement {REVISED_SOURCE_ACKNOWLEDGEMENT:?}"
        )));
    }
    Ok(())
}

fn required_active<'a>(value: Option<&'a str>, label: &str) -> Result<&'a str> {
    value.ok_or_else(|| KernelError::Conflict(format!("engagement has no {label}")))
}

fn require_active_tour_context(
    state: &EngagementState,
    active_tour: &RegisteredTourRow,
) -> Result<()> {
    let evidence = &active_tour.evidence;
    if state.active.preflight_id.as_deref() != Some(evidence.preflight_id.as_str())
        || state.active.job_id.as_deref() != Some(evidence.job_id.as_str())
        || state.active.profiling_run_id.as_deref() != Some(evidence.profiling_run_id.as_str())
        || state.active.profile_id.as_deref() != Some(evidence.profile_id.as_str())
        || state.active.profile_version.as_deref() != Some(evidence.profile_version.as_str())
        || state.active.profile_hash.as_deref() != Some(evidence.profile_hash.as_str())
        || state.active.mapping_id.as_deref() != Some(evidence.mapping_id.as_str())
        || state.active.mapping_version.as_deref() != Some(evidence.mapping_version.as_str())
        || state.active.mapping_hash.as_deref() != Some(evidence.mapping_hash.as_str())
        || state.active.validation_run_id.as_deref() != Some(evidence.validation_run_id.as_str())
    {
        return Err(KernelError::IntegrityFailed(
            "active engagement context differs from its immutable active revision".to_owned(),
        ));
    }
    Ok(())
}

fn require_exact_active_tour_context(
    service: &EngagementService,
    connection: &Connection,
    engagement_id: &str,
    expected_active_tour_id: &str,
) -> Result<EngagementState> {
    let state = load_engagement_state(connection)?;
    if state.engagement_id != engagement_id
        || !phase_has_completed_validation(&state.phase)
        || state.profile_stage != "APPROVED"
        || state.mapping_stage != "VALIDATED"
        || state.validation_stage != "COMPLETE"
    {
        return Err(KernelError::IntegrityFailed(
            "active engagement phase/stages differ from the immutable completed baseline revision"
                .to_owned(),
        ));
    }
    let active_tour = load_registered_tour(connection, expected_active_tour_id)?;
    if active_tour.evidence.engagement_id != engagement_id {
        return Err(KernelError::IntegrityFailed(
            "active tour belongs to a different engagement".to_owned(),
        ));
    }
    require_active_tour_context(&state, &active_tour)?;
    if service.selected_pack_for_state(&state)?.is_none() {
        return Err(KernelError::IntegrityFailed(
            "active completed revision has no independently replayed destination authority"
                .to_owned(),
        ));
    }
    Ok(state)
}

fn phase_has_completed_validation(phase: &EngagementPhase) -> bool {
    matches!(
        phase,
        EngagementPhase::ValidationComplete | EngagementPhase::FirstIssuesReady
    )
}

fn sqlite_tour_integer(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::PolicyInvalid(format!("{label} does not fit SQLite INTEGER")))
}

fn nonnegative_i64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| KernelError::IntegrityFailed(format!("{label} is negative")))
}

fn load_tour_command(
    connection: &Connection,
    engagement_id: &str,
    idempotency_key: &str,
) -> Result<Option<TourCommandRow>> {
    connection
        .query_row(
            "SELECT command_kind, request_hash, result_generation,
                    result_reference, result_hash
             FROM engagement_tour_command_attempts
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| {
                let generation = row.get::<_, i64>(2)?;
                Ok(TourCommandRow {
                    command_kind: row.get(0)?,
                    request_hash: row.get(1)?,
                    result_generation: u64::try_from(generation).unwrap_or(u64::MAX),
                    result_reference: row.get(3)?,
                    result_hash: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_tour_command_by_kind(
    connection: &Connection,
    tour_attempt_id: &str,
    command_kind: &str,
) -> Result<Option<TourCommandRow>> {
    let rows = connection
        .prepare(
            "SELECT command_kind, request_hash, result_generation,
                    result_reference, result_hash
             FROM engagement_tour_command_attempts
             WHERE tour_attempt_id = ?1 AND command_kind = ?2
             ORDER BY created_at, attempt_id",
        )?
        .query_map(params![tour_attempt_id, command_kind], |row| {
            let generation = row.get::<_, i64>(2)?;
            Ok(TourCommandRow {
                command_kind: row.get(0)?,
                request_hash: row.get(1)?,
                result_generation: u64::try_from(generation).unwrap_or(u64::MAX),
                result_reference: row.get(3)?,
                result_hash: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "tour attempt {tour_attempt_id} has multiple {command_kind} commands"
        )));
    }
    Ok(rows.into_iter().next())
}

fn ensure_no_tour_command_kind(
    connection: &Connection,
    tour_attempt_id: &str,
    command_kind: &str,
) -> Result<()> {
    if load_tour_command_by_kind(connection, tour_attempt_id, command_kind)?.is_some() {
        return Err(KernelError::Conflict(format!(
            "tour attempt already has a {command_kind} command under a different idempotency key"
        )));
    }
    Ok(())
}

fn require_tour_command_replay(
    command: &TourCommandRow,
    expected_kind: &str,
    request_hash: &str,
) -> Result<()> {
    if command.command_kind != expected_kind || command.request_hash != request_hash {
        return Err(KernelError::Conflict(
            "tour idempotency key was used for a different command".to_owned(),
        ));
    }
    if !is_canonical_sha256(&command.result_hash) {
        return Err(KernelError::IntegrityFailed(
            "tour command result hash is invalid".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_tour_command(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    engagement_id: &str,
    tour_attempt_id: Option<&str>,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
    canonical_request_json: &str,
    expected_generation: u64,
    result_generation: u64,
    result_reference: &str,
    result_hash: &str,
    created_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO engagement_tour_command_attempts (
             attempt_id, engagement_id, tour_attempt_id, command_kind,
             idempotency_key, request_hash, canonical_request_json,
             expected_generation, result_generation, result_reference,
             result_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            attempt_id,
            engagement_id,
            tour_attempt_id,
            command_kind,
            idempotency_key,
            request_hash,
            canonical_request_json,
            sqlite_tour_integer(expected_generation, "expected tour generation")?,
            sqlite_tour_integer(result_generation, "result tour generation")?,
            result_reference,
            result_hash,
            created_at,
        ],
    )?;
    Ok(())
}

fn load_tour_head(connection: &Connection, engagement_id: &str) -> Result<Option<TourHeadRow>> {
    connection
        .query_row(
            "SELECT generation, active_tour_id, pending_tour_attempt_id
             FROM engagement_tour_heads WHERE engagement_id = ?1",
            [engagement_id],
            |row| {
                let generation = row.get::<_, i64>(0)?;
                Ok(TourHeadRow {
                    generation: u64::try_from(generation).unwrap_or(u64::MAX),
                    active_tour_id: row.get(1)?,
                    pending_tour_attempt_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn require_pending_head(
    connection: &Connection,
    engagement_id: &str,
    tour_attempt_id: &str,
    expected_generation: u64,
) -> Result<TourHeadRow> {
    let head = load_tour_head(connection, engagement_id)?
        .ok_or_else(|| KernelError::Conflict("engagement has no revised-tour chain".to_owned()))?;
    if head.generation != expected_generation
        || head.pending_tour_attempt_id.as_deref() != Some(tour_attempt_id)
    {
        return Err(KernelError::Conflict(format!(
            "pending revised tour/generation differs: expected {tour_attempt_id}/{expected_generation}, found {:?}/{}",
            head.pending_tour_attempt_id, head.generation
        )));
    }
    Ok(head)
}

fn update_tour_head(
    transaction: &Transaction<'_>,
    engagement_id: &str,
    expected_generation: u64,
    result_generation: u64,
    active_tour_id: Option<&str>,
    pending_tour_attempt_id: Option<&str>,
    updated_at: &str,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE engagement_tour_heads
         SET generation = ?1, active_tour_id = ?2,
             pending_tour_attempt_id = ?3, updated_at = ?4
         WHERE engagement_id = ?5 AND generation = ?6",
        params![
            sqlite_tour_integer(result_generation, "result tour generation")?,
            active_tour_id,
            pending_tour_attempt_id,
            updated_at,
            engagement_id,
            sqlite_tour_integer(expected_generation, "expected tour generation")?,
        ],
    )?;
    require_one(changed, "advance revised-tour head")
}

fn load_job_source_hash(connection: &Connection, job_id: &str) -> Result<String> {
    let (state, hash): (String, Option<String>) = connection.query_row(
        "SELECT state, source_hash FROM jobs WHERE id = ?1",
        [job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if state != "COMPLETE" {
        return Err(KernelError::IntegrityFailed(format!(
            "tour intake job {job_id} is not COMPLETE"
        )));
    }
    hash.ok_or_else(|| {
        KernelError::IntegrityFailed(format!("tour intake job {job_id} has no source hash"))
    })
}

fn exact_profile_approval(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<ProfileApprovalIdentity> {
    let profile = inspect_profile(workspace, profile_id, profile_version)?;
    if profile.profile_hash != profile_hash
        || profile.lifecycle_status != ProfileLifecycleStatus::Approved
        || !profile.can_start_new_runs
    {
        return Err(KernelError::IntegrityFailed(
            "profile approval authority differs from the exact tour profile".to_owned(),
        ));
    }
    let approval = profile.approval.ok_or_else(|| {
        KernelError::IntegrityFailed("approved profile has no exact approval evidence".to_owned())
    })?;
    if !approval.acknowledged || !is_canonical_sha256(&approval.approval_hash) {
        return Err(KernelError::IntegrityFailed(
            "profile approval acknowledgement/hash is invalid".to_owned(),
        ));
    }
    Ok(ProfileApprovalIdentity {
        approval_id: approval.approval_id,
        approval_hash: approval.approval_hash,
    })
}

fn exact_mapping_approval(
    connection: &Connection,
    engagement_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    require_active: bool,
) -> Result<MappingApprovalRow> {
    verify_mapping_authority_guards(connection)?;
    let approval_ids = connection
        .prepare(
            "SELECT approval_id FROM engagement_mapping_approvals
             WHERE engagement_id = ?1 AND mapping_id = ?2
               AND mapping_version = ?3 AND mapping_hash = ?4
             ORDER BY approval_id",
        )?
        .query_map(
            params![engagement_id, mapping_id, mapping_version, mapping_hash],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if approval_ids.len() != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "exact mapping authority has {} approvals; expected one",
            approval_ids.len()
        )));
    }
    let approval = load_mapping_approval_row(connection, &approval_ids[0])?;
    verify_mapping_approval(connection, &approval, require_active)?;
    Ok(approval)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn ensure_normalized_mapping_authority(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<NormalizedMappingAuthorityRow> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT authority_id, authority_hash
             FROM engagement_mapping_authorities
             WHERE engagement_id = ?1 AND job_id = ?2 AND source_hash = ?3
               AND mapping_id = ?4 AND mapping_version = ?5 AND mapping_hash = ?6",
            params![
                engagement_id,
                job_id,
                source_hash,
                mapping_id,
                mapping_version,
                mapping_hash,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((authority_id, authority_hash)) = existing {
        return load_normalized_mapping_authority(connection, &authority_id, &authority_hash);
    }

    if load_job_source_hash(connection, job_id)? != source_hash {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority source differs from its exact intake job".to_owned(),
        ));
    }
    let mapping = load_stored_mapping(connection, mapping_id, mapping_version)?;
    if mapping.mapping_hash != mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority differs from immutable mapping bytes".to_owned(),
        ));
    }

    let approval_ids = connection
        .prepare(
            "SELECT approval_id FROM engagement_mapping_approvals
             WHERE engagement_id = ?1 AND mapping_id = ?2
               AND mapping_version = ?3 AND mapping_hash = ?4
             ORDER BY approval_id",
        )?
        .query_map(
            params![engagement_id, mapping_id, mapping_version, mapping_hash],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (
        authority_kind,
        mapping_approval_id,
        mapping_approval_hash,
        preparation_id,
        mapping_validation_attempt_id,
        mapping_validation_attempt_hash,
    ) = if approval_ids.len() == 1 {
        let approval = exact_mapping_approval(
            connection,
            engagement_id,
            mapping_id,
            mapping_version,
            mapping_hash,
            false,
        )?;
        (
            NormalizedMappingAuthorityKind::OperatorApproval,
            Some(approval.approval_id),
            Some(approval.approval_hash),
            None,
            None,
            None,
        )
    } else if approval_ids.is_empty() {
        let controlled = connection
            .prepare(
                "SELECT p.preparation_id, v.id, v.attempt_hash
                 FROM engagement_mapping_preparations p
                 JOIN mapping_versions m
                   ON m.mapping_id = p.mapping_id
                  AND m.mapping_version = p.mapping_version
                  AND m.mapping_hash = p.mapping_hash
                 JOIN mapping_validation_attempts v
                   ON v.id = m.attempt_id
                  AND v.mapping_id = m.mapping_id
                  AND v.mapping_version = m.mapping_version
                  AND v.mapping_hash = m.mapping_hash
                  AND v.state = 'VALID'
                 WHERE p.engagement_id = ?1 AND p.mapping_id = ?2
                   AND p.mapping_version = ?3 AND p.mapping_hash = ?4
                 ORDER BY p.preparation_id",
            )?
            .query_map(
                params![engagement_id, mapping_id, mapping_version, mapping_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if controlled.len() != 1 {
            return Err(KernelError::IntegrityFailed(format!(
                "controlled mapping has {} exact preparation/validation authorities; expected one",
                controlled.len()
            )));
        }
        let (preparation_id, attempt_id, attempt_hash) = controlled[0].clone();
        if !is_canonical_sha256(&attempt_hash) {
            return Err(KernelError::IntegrityFailed(
                "controlled mapping validation attempt hash is invalid".to_owned(),
            ));
        }
        (
            NormalizedMappingAuthorityKind::ControlledPreparation,
            None,
            None,
            Some(preparation_id),
            Some(attempt_id),
            Some(attempt_hash),
        )
    } else {
        return Err(KernelError::IntegrityFailed(
            "mapping has ambiguous operator approval authority".to_owned(),
        ));
    };

    let authority_id = format!("mapping-authority-{}", Uuid::new_v4());
    let created_at = now_rfc3339();
    let evidence = NormalizedMappingAuthorityEvidence {
        schema_version: NORMALIZED_MAPPING_AUTHORITY_SCHEMA.to_owned(),
        authority_id: authority_id.clone(),
        authority_kind: authority_kind.clone(),
        engagement_id: engagement_id.to_owned(),
        job_id: job_id.to_owned(),
        source_hash: source_hash.to_owned(),
        mapping_id: mapping_id.to_owned(),
        mapping_version: mapping_version.to_owned(),
        mapping_hash: mapping_hash.to_owned(),
        mapping_approval_id: mapping_approval_id.clone(),
        mapping_approval_hash: mapping_approval_hash.clone(),
        preparation_id: preparation_id.clone(),
        mapping_validation_attempt_id: mapping_validation_attempt_id.clone(),
        mapping_validation_attempt_hash,
        created_at: created_at.clone(),
    };
    let canonical_authority_json = canonical_json_string(&serde_json::to_value(&evidence)?)?;
    let authority_hash = sha256_hex(canonical_authority_json.as_bytes());
    connection.execute(
        "INSERT INTO engagement_mapping_authorities (
             authority_id, authority_kind, engagement_id, job_id, source_hash,
             mapping_id, mapping_version, mapping_hash,
             mapping_approval_id, mapping_approval_hash,
             preparation_id, mapping_validation_attempt_id,
             canonical_authority_json, authority_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15)",
        params![
            authority_id,
            match authority_kind {
                NormalizedMappingAuthorityKind::OperatorApproval => "OPERATOR_APPROVAL",
                NormalizedMappingAuthorityKind::ControlledPreparation => "CONTROLLED_PREPARATION",
            },
            engagement_id,
            job_id,
            source_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
            mapping_approval_id,
            mapping_approval_hash,
            preparation_id,
            mapping_validation_attempt_id,
            canonical_authority_json,
            authority_hash,
            created_at,
        ],
    )?;
    load_normalized_mapping_authority(connection, &authority_id, &authority_hash)
}

fn load_normalized_mapping_authority(
    connection: &Connection,
    authority_id: &str,
    expected_hash: &str,
) -> Result<NormalizedMappingAuthorityRow> {
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT authority_kind, engagement_id, job_id, source_hash,
                mapping_id, mapping_version, mapping_hash, authority_id,
                mapping_approval_id, mapping_approval_hash,
                preparation_id, mapping_validation_attempt_id,
                canonical_authority_json, authority_hash, created_at
         FROM engagement_mapping_authorities WHERE authority_id = ?1",
        [authority_id],
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
    )?;
    let value: Value = serde_json::from_str(&row.12).map_err(|error| {
        KernelError::IntegrityFailed(format!("mapping authority JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != row.12
        || sha256_hex(row.12.as_bytes()) != row.13
        || row.13 != expected_hash
    {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority canonical bytes/hash mismatch".to_owned(),
        ));
    }
    let evidence: NormalizedMappingAuthorityEvidence =
        serde_json::from_value(value).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "normalized mapping authority contract is invalid: {error}"
            ))
        })?;
    let authority_kind = match row.0.as_str() {
        "OPERATOR_APPROVAL" => NormalizedMappingAuthorityKind::OperatorApproval,
        "CONTROLLED_PREPARATION" => NormalizedMappingAuthorityKind::ControlledPreparation,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "normalized mapping authority kind is unsupported".to_owned(),
            ));
        }
    };
    if evidence.schema_version != NORMALIZED_MAPPING_AUTHORITY_SCHEMA
        || evidence.authority_id != authority_id
        || evidence.authority_kind != authority_kind
        || evidence.engagement_id != row.1
        || evidence.job_id != row.2
        || evidence.source_hash != row.3
        || evidence.mapping_id != row.4
        || evidence.mapping_version != row.5
        || evidence.mapping_hash != row.6
        || evidence.authority_id != row.7
        || evidence.mapping_approval_id != row.8
        || evidence.mapping_approval_hash != row.9
        || evidence.preparation_id != row.10
        || evidence.mapping_validation_attempt_id != row.11
        || evidence.created_at != row.14
        || load_job_source_hash(connection, &evidence.job_id)? != evidence.source_hash
    {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority differs from immutable row/source evidence".to_owned(),
        ));
    }
    let mapping = load_stored_mapping(connection, &evidence.mapping_id, &evidence.mapping_version)?;
    if mapping.mapping_hash != evidence.mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority differs from immutable mapping".to_owned(),
        ));
    }
    match evidence.authority_kind {
        NormalizedMappingAuthorityKind::OperatorApproval => {
            let approval_id = evidence.mapping_approval_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "operator mapping authority has no approval".to_owned(),
                )
            })?;
            let approval = load_mapping_approval_row(connection, approval_id)?;
            verify_mapping_approval(connection, &approval, false)?;
            if Some(approval.approval_hash) != evidence.mapping_approval_hash {
                return Err(KernelError::IntegrityFailed(
                    "operator mapping authority approval failed replay".to_owned(),
                ));
            }
        }
        NormalizedMappingAuthorityKind::ControlledPreparation => {
            let preparation_id = evidence.preparation_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "controlled mapping authority has no preparation".to_owned(),
                )
            })?;
            let attempt_id = evidence
                .mapping_validation_attempt_id
                .as_deref()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "controlled mapping authority has no validation attempt".to_owned(),
                    )
                })?;
            let support: (String, String) = connection.query_row(
                "SELECT v.attempt_hash, p.mapping_hash
                 FROM engagement_mapping_preparations p
                 JOIN mapping_versions m ON m.mapping_id = p.mapping_id
                    AND m.mapping_version = p.mapping_version
                    AND m.mapping_hash = p.mapping_hash
                 JOIN mapping_validation_attempts v ON v.id = m.attempt_id
                    AND v.id = ?2 AND v.state = 'VALID'
                 WHERE p.preparation_id = ?1 AND p.engagement_id = ?3
                   AND p.mapping_id = ?4 AND p.mapping_version = ?5
                   AND p.mapping_hash = ?6",
                params![
                    preparation_id,
                    attempt_id,
                    evidence.engagement_id,
                    evidence.mapping_id,
                    evidence.mapping_version,
                    evidence.mapping_hash,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if evidence.mapping_validation_attempt_hash.as_deref() != Some(support.0.as_str())
                || support.1 != evidence.mapping_hash
            {
                return Err(KernelError::IntegrityFailed(
                    "controlled mapping authority support failed replay".to_owned(),
                ));
            }
        }
    }
    Ok(NormalizedMappingAuthorityRow {
        evidence,
        authority_hash: row.13,
    })
}

fn load_baseline_source_evidence(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    profiling_run_id: &str,
) -> Result<BaselineSourceEvidence> {
    let rows = connection
        .prepare(
            "SELECT a.preflight_id, a.attempt_id, a.accepted_source_hash,
                    a.accepted_manifest_hash, p.result_hash
             FROM engagement_acceptance_attempts a
             JOIN profiling_runs p
               ON p.id = a.profiling_run_id AND p.job_id = a.job_id
             WHERE a.engagement_id = ?1 AND a.job_id = ?2
               AND a.profiling_run_id = ?3 AND p.state = 'COMPLETE'
             ORDER BY a.attempt_id",
        )?
        .query_map(params![engagement_id, job_id, profiling_run_id], |row| {
            Ok(BaselineSourceEvidence {
                preflight_id: row.get(0)?,
                acceptance_attempt_id: row.get(1)?,
                source_hash: row.get(2)?,
                manifest_hash: row.get(3)?,
                profiling_result_hash: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "active Tour 1 source has {} acceptance/profiling evidence rows; expected one",
            rows.len()
        )));
    }
    let evidence = rows.into_iter().next().expect("one source evidence row");
    if evidence.source_hash != load_job_source_hash(connection, job_id)? {
        return Err(KernelError::IntegrityFailed(
            "active Tour 1 acceptance source differs from its intake job".to_owned(),
        ));
    }
    let preflight = load_preflight(connection, &evidence.preflight_id)?;
    if preflight.state != "READY"
        || preflight.source_hash.as_deref() != Some(evidence.source_hash.as_str())
        || preflight.manifest_hash.as_deref() != Some(evidence.manifest_hash.as_str())
    {
        return Err(KernelError::IntegrityFailed(
            "active Tour 1 preflight differs from acceptance evidence".to_owned(),
        ));
    }
    require_sample_free_profiling(
        connection,
        job_id,
        profiling_run_id,
        &evidence.source_hash,
        &evidence.profiling_result_hash,
    )?;
    Ok(evidence)
}

#[allow(clippy::too_many_arguments)]
fn load_complete_validation_identity(
    connection: &Connection,
    run_id: &str,
    job_id: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<CompleteValidationIdentity> {
    let row: Option<(
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
    )> = connection
        .query_row(
            "SELECT state, job_id, profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    identity_hash, source_hash, result_hash
             FROM validation_runs WHERE id = ?1",
            [run_id],
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
                ))
            },
        )
        .optional()?;
    let Some((
        state,
        stored_job,
        stored_profile,
        stored_profile_version,
        stored_profile_hash,
        stored_mapping,
        stored_mapping_version,
        stored_mapping_hash,
        identity_hash,
        source_hash,
        result_hash,
    )) = row
    else {
        return Err(KernelError::IntegrityFailed(format!(
            "missing complete validation run {run_id}"
        )));
    };
    if state != "COMPLETE"
        || stored_job != job_id
        || stored_profile != profile_id
        || stored_profile_version != profile_version
        || stored_profile_hash != profile_hash
        || stored_mapping != mapping_id
        || stored_mapping_version != mapping_version
        || stored_mapping_hash != mapping_hash
        || source_hash != load_job_source_hash(connection, job_id)?
        || !is_canonical_sha256(&identity_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "Tour 1 validation run differs from active source/profile/mapping authority".to_owned(),
        ));
    }
    let result_hash = result_hash.ok_or_else(|| {
        KernelError::IntegrityFailed("complete Tour 1 validation has no result hash".to_owned())
    })?;
    Ok(CompleteValidationIdentity {
        identity_hash,
        result_hash,
    })
}

fn require_sample_free_profiling(
    connection: &Connection,
    job_id: &str,
    profiling_run_id: &str,
    source_hash: &str,
    result_hash: &str,
) -> Result<()> {
    let (state, stored_job, stored_source, stored_result): (
        String,
        String,
        String,
        Option<String>,
    ) = connection.query_row(
        "SELECT state, job_id, source_hash, result_hash
         FROM profiling_runs WHERE id = ?1",
        [profiling_run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if state != "COMPLETE"
        || stored_job != job_id
        || stored_source != source_hash
        || stored_result.as_deref() != Some(result_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "tour profiling identity differs from accepted source evidence".to_owned(),
        ));
    }
    let unsafe_columns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM profiling_columns
         WHERE run_id = ?1 AND samples_json <> '[]'",
        [profiling_run_id],
        |row| row.get(0),
    )?;
    if unsafe_columns != 0 {
        return Err(KernelError::IntegrityFailed(
            "revised-source profiling retained sample evidence".to_owned(),
        ));
    }
    Ok(())
}

fn new_namespace_key() -> ComparedValidationNamespaceKey {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut key = [0_u8; 32];
    key[..16].copy_from_slice(first.as_bytes());
    key[16..].copy_from_slice(second.as_bytes());
    key
}

fn load_or_create_namespace(
    connection: &Connection,
    engagement_id: &str,
) -> Result<ComparisonNamespace> {
    if let Some(existing) = load_namespace_optional(connection, engagement_id)? {
        return Ok(existing);
    }
    let namespace_id = format!("comparison-namespace-{}", Uuid::new_v4());
    let key = new_namespace_key();
    let secret_hash = sha256_hex(key);
    connection.execute(
        "INSERT INTO engagement_comparison_namespaces (
             namespace_id, engagement_id, algorithm_version,
             secret, secret_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            namespace_id,
            engagement_id,
            COMPARISON_FINGERPRINT_ALGORITHM,
            key.as_slice(),
            secret_hash,
            now_rfc3339(),
        ],
    )?;
    Ok(ComparisonNamespace {
        namespace_id,
        secret_hash,
        key,
    })
}

fn load_namespace(connection: &Connection, engagement_id: &str) -> Result<ComparisonNamespace> {
    load_namespace_optional(connection, engagement_id)?.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "engagement tour chain has no comparison pseudonym namespace".to_owned(),
        )
    })
}

fn load_namespace_optional(
    connection: &Connection,
    engagement_id: &str,
) -> Result<Option<ComparisonNamespace>> {
    let row: Option<(String, String, Vec<u8>, String)> = connection
        .query_row(
            "SELECT namespace_id, algorithm_version, secret, secret_hash
             FROM engagement_comparison_namespaces WHERE engagement_id = ?1",
            [engagement_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((namespace_id, algorithm, secret, secret_hash)) = row else {
        return Ok(None);
    };
    if algorithm != COMPARISON_FINGERPRINT_ALGORITHM
        || secret.len() != 32
        || sha256_hex(&secret) != secret_hash
    {
        return Err(KernelError::IntegrityFailed(
            "comparison pseudonym namespace failed independent replay".to_owned(),
        ));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&secret);
    Ok(Some(ComparisonNamespace {
        namespace_id,
        secret_hash,
        key,
    }))
}

fn canonical_registered_tour(evidence: RegisteredTourEvidence) -> Result<RegisteredTourRow> {
    if evidence.schema_version != REGISTERED_TOUR_SCHEMA
        || evidence.tour_number == 0
        || !is_canonical_sha256(&evidence.source_hash)
        || !is_canonical_sha256(&evidence.manifest_hash)
        || !is_canonical_sha256(&evidence.profiling_result_hash)
        || !is_canonical_sha256(&evidence.profile_hash)
        || !is_canonical_sha256(&evidence.profile_approval_hash)
        || !is_canonical_sha256(&evidence.mapping_hash)
        || evidence
            .mapping_approval_hash
            .as_deref()
            .is_some_and(|hash| !is_canonical_sha256(hash))
        || evidence.mapping_approval_id.is_some() != evidence.mapping_approval_hash.is_some()
        || evidence
            .mapping_authority_hash
            .as_deref()
            .is_some_and(|hash| !is_canonical_sha256(hash))
        || evidence.mapping_authority_id.is_some() != evidence.mapping_authority_hash.is_some()
        || !is_canonical_sha256(&evidence.validation_identity_hash)
        || !is_canonical_sha256(&evidence.validation_result_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "registered tour evidence is outside its canonical contract".to_owned(),
        ));
    }
    let canonical_json = canonical_json_string(&serde_json::to_value(&evidence)?)?;
    let tour_hash = sha256_hex(canonical_json.as_bytes());
    Ok(RegisteredTourRow {
        mapping_authority_id: evidence.mapping_authority_id.clone().unwrap_or_default(),
        mapping_authority_hash: evidence.mapping_authority_hash.clone().unwrap_or_default(),
        evidence,
        canonical_json,
        tour_hash,
    })
}

fn insert_registered_tour(transaction: &Transaction<'_>, tour: &RegisteredTourRow) -> Result<()> {
    let evidence = &tour.evidence;
    transaction.execute(
        "INSERT INTO engagement_tours (
             tour_id, engagement_id, tour_attempt_id, tour_number,
             predecessor_tour_id, source_reuse_id, source_reuse_hash,
             preflight_id, acceptance_attempt_id, job_id, profiling_run_id,
             source_hash, manifest_hash, profiling_result_hash,
             profile_id, profile_version, profile_hash,
             profile_approval_id, profile_approval_hash,
             mapping_id, mapping_version, mapping_hash,
             mapping_approval_id, mapping_approval_hash,
             validation_run_id, validation_identity_hash,
             validation_result_hash, canonical_tour_json,
             tour_hash, registered_at, mapping_authority_id,
             mapping_authority_hash
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
             ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
             ?29, ?30, ?31, ?32
         )",
        params![
            evidence.tour_id,
            evidence.engagement_id,
            evidence.tour_attempt_id,
            sqlite_tour_integer(evidence.tour_number, "tour number")?,
            evidence.predecessor_tour_id,
            evidence.source_reuse_id,
            evidence.source_reuse_hash,
            evidence.preflight_id,
            evidence.acceptance_attempt_id,
            evidence.job_id,
            evidence.profiling_run_id,
            evidence.source_hash,
            evidence.manifest_hash,
            evidence.profiling_result_hash,
            evidence.profile_id,
            evidence.profile_version,
            evidence.profile_hash,
            evidence.profile_approval_id,
            evidence.profile_approval_hash,
            evidence.mapping_id,
            evidence.mapping_version,
            evidence.mapping_hash,
            evidence.mapping_approval_id,
            evidence.mapping_approval_hash,
            evidence.validation_run_id,
            evidence.validation_identity_hash,
            evidence.validation_result_hash,
            tour.canonical_json,
            tour.tour_hash,
            evidence.registered_at,
            tour.mapping_authority_id,
            tour.mapping_authority_hash,
        ],
    )?;
    Ok(())
}

fn load_registered_tour(connection: &Connection, tour_id: &str) -> Result<RegisteredTourRow> {
    let row: (String, String, Option<String>, Option<String>) = connection.query_row(
        "SELECT canonical_tour_json, tour_hash,
                mapping_authority_id, mapping_authority_hash
         FROM engagement_tours WHERE tour_id = ?1",
        [tour_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let value: Value = serde_json::from_str(&row.0).map_err(|error| {
        KernelError::IntegrityFailed(format!("registered tour JSON is invalid: {error}"))
    })?;
    let canonical = canonical_json_string(&value)?;
    if canonical != row.0 || sha256_hex(canonical.as_bytes()) != row.1 {
        return Err(KernelError::IntegrityFailed(
            "registered tour canonical bytes/hash mismatch".to_owned(),
        ));
    }
    let evidence: RegisteredTourEvidence = serde_json::from_value(value).map_err(|error| {
        KernelError::IntegrityFailed(format!("registered tour contract is invalid: {error}"))
    })?;
    let mut expected = canonical_registered_tour(evidence)?;
    if expected.tour_hash != row.1 || expected.evidence.tour_id != tour_id {
        return Err(KernelError::IntegrityFailed(
            "registered tour differs from its immutable row identity".to_owned(),
        ));
    }
    let mapping_authority_id = row.2.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "registered tour has no normalized mapping authority".to_owned(),
        )
    })?;
    let mapping_authority_hash = row.3.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "registered tour has no normalized mapping authority hash".to_owned(),
        )
    })?;
    if expected
        .evidence
        .mapping_authority_id
        .as_deref()
        .is_some_and(|value| value != mapping_authority_id)
        || expected
            .evidence
            .mapping_authority_hash
            .as_deref()
            .is_some_and(|value| value != mapping_authority_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "registered tour canonical mapping authority differs from its immutable row".to_owned(),
        ));
    }
    let authority = load_normalized_mapping_authority(
        connection,
        &mapping_authority_id,
        &mapping_authority_hash,
    )?;
    if authority.evidence.engagement_id != expected.evidence.engagement_id
        || authority.evidence.job_id != expected.evidence.job_id
        || authority.evidence.source_hash != expected.evidence.source_hash
        || authority.evidence.mapping_id != expected.evidence.mapping_id
        || authority.evidence.mapping_version != expected.evidence.mapping_version
        || authority.evidence.mapping_hash != expected.evidence.mapping_hash
    {
        return Err(KernelError::IntegrityFailed(
            "registered tour normalized mapping authority differs from tour authority".to_owned(),
        ));
    }
    expected.mapping_authority_id = mapping_authority_id;
    expected.mapping_authority_hash = mapping_authority_hash;
    verify_registered_tour_row(connection, &expected)?;
    Ok(expected)
}

fn verify_registered_tour_row(connection: &Connection, expected: &RegisteredTourRow) -> Result<()> {
    let evidence = &expected.evidence;
    let actual: (
        String,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
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
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT engagement_id, tour_attempt_id, tour_number,
                predecessor_tour_id, source_reuse_id, source_reuse_hash,
                preflight_id, acceptance_attempt_id, job_id, profiling_run_id,
                source_hash, manifest_hash, profiling_result_hash,
                profile_id, profile_version, profile_hash,
                profile_approval_id, profile_approval_hash,
                mapping_id, mapping_version, mapping_hash,
                mapping_approval_id, mapping_approval_hash,
                validation_run_id, validation_identity_hash,
                validation_result_hash, registered_at, tour_hash
         FROM engagement_tours WHERE tour_id = ?1",
        [&evidence.tour_id],
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
                row.get(17)?,
                row.get(18)?,
                row.get(19)?,
                row.get(20)?,
                row.get(21)?,
                row.get(22)?,
                row.get(23)?,
                row.get(24)?,
                row.get(25)?,
                row.get(26)?,
                row.get(27)?,
            ))
        },
    )?;
    let row_matches = actual.0 == evidence.engagement_id
        && actual.1 == evidence.tour_attempt_id
        && u64::try_from(actual.2).ok() == Some(evidence.tour_number)
        && actual.3 == evidence.predecessor_tour_id
        && actual.4 == evidence.source_reuse_id
        && actual.5 == evidence.source_reuse_hash
        && actual.6 == evidence.preflight_id
        && actual.7 == evidence.acceptance_attempt_id
        && actual.8 == evidence.job_id
        && actual.9 == evidence.profiling_run_id
        && actual.10 == evidence.source_hash
        && actual.11 == evidence.manifest_hash
        && actual.12 == evidence.profiling_result_hash
        && actual.13 == evidence.profile_id
        && actual.14 == evidence.profile_version
        && actual.15 == evidence.profile_hash
        && actual.16 == evidence.profile_approval_id
        && actual.17 == evidence.profile_approval_hash
        && actual.18 == evidence.mapping_id
        && actual.19 == evidence.mapping_version
        && actual.20 == evidence.mapping_hash
        && actual.21 == evidence.mapping_approval_id
        && actual.22 == evidence.mapping_approval_hash
        && actual.23 == evidence.validation_run_id
        && actual.24 == evidence.validation_identity_hash
        && actual.25 == evidence.validation_result_hash
        && actual.26 == evidence.registered_at
        && actual.27 == expected.tour_hash;
    if !row_matches {
        return Err(KernelError::IntegrityFailed(
            "registered tour canonical JSON differs from its immutable row".to_owned(),
        ));
    }
    verify_validation_run_for_read(connection, &evidence.validation_run_id)?;
    let validation = load_complete_validation_identity(
        connection,
        &evidence.validation_run_id,
        &evidence.job_id,
        &evidence.profile_id,
        &evidence.profile_version,
        &evidence.profile_hash,
        &evidence.mapping_id,
        &evidence.mapping_version,
        &evidence.mapping_hash,
    )?;
    if validation.identity_hash != evidence.validation_identity_hash
        || validation.result_hash != evidence.validation_result_hash
    {
        return Err(KernelError::IntegrityFailed(
            "registered tour validation identity/result failed replay".to_owned(),
        ));
    }
    Ok(())
}

fn load_tour_attempt(connection: &Connection, tour_attempt_id: &str) -> Result<TourAttemptRow> {
    let (canonical_json, attempt_hash): (String, String) = connection.query_row(
        "SELECT canonical_attempt_json, attempt_hash
         FROM engagement_tour_attempts WHERE tour_attempt_id = ?1",
        [tour_attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let value: Value = serde_json::from_str(&canonical_json).map_err(|error| {
        KernelError::IntegrityFailed(format!("tour attempt JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != canonical_json
        || sha256_hex(canonical_json.as_bytes()) != attempt_hash
    {
        return Err(KernelError::IntegrityFailed(
            "tour attempt canonical bytes/hash mismatch".to_owned(),
        ));
    }
    let evidence: TourAttemptEvidence = serde_json::from_value(value).map_err(|error| {
        KernelError::IntegrityFailed(format!("tour attempt contract is invalid: {error}"))
    })?;
    if evidence.schema_version != TOUR_ATTEMPT_SCHEMA
        || evidence.tour_attempt_id != tour_attempt_id
        || evidence.tour_number < 2
    {
        return Err(KernelError::IntegrityFailed(
            "tour attempt contract identity is invalid".to_owned(),
        ));
    }
    let row: (String, String, i64, String, String, String, String) = connection.query_row(
        "SELECT engagement_id, planned_tour_id, tour_number,
                predecessor_tour_id, started_by, reason, created_at
         FROM engagement_tour_attempts WHERE tour_attempt_id = ?1",
        [tour_attempt_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    if row.0 != evidence.engagement_id
        || row.1 != evidence.planned_tour_id
        || u64::try_from(row.2).ok() != Some(evidence.tour_number)
        || row.3 != evidence.predecessor_tour_id
        || row.4 != evidence.actor
        || row.5 != evidence.reason
        || row.6 != evidence.created_at
    {
        return Err(KernelError::IntegrityFailed(
            "tour attempt canonical JSON differs from its immutable row".to_owned(),
        ));
    }
    Ok(TourAttemptRow { evidence })
}

fn source_structure(
    connection: &Connection,
    job_id: &str,
    mapping: &MappingDocument,
) -> Result<SourceStructure> {
    let source_hash = load_job_source_hash(connection, job_id)?;
    if source_hash.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "source structure has no complete job identity".to_owned(),
        ));
    }
    let rows = connection
        .prepare(
            "SELECT path, header_json FROM csv_files
             WHERE job_id = ?1 ORDER BY path",
        )?
        .query_map([job_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "accepted source structure contains no CSV files".to_owned(),
        ));
    }
    let mut files = Vec::with_capacity(rows.len());
    for (path, header_json) in rows {
        let headers: Vec<String> =
            serde_json::from_str(header_json.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(format!("accepted CSV {path:?} has no durable header"))
            })?)
            .map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "accepted CSV {path:?} header JSON is invalid: {error}"
                ))
            })?;
        if headers.is_empty() || headers.iter().any(String::is_empty) {
            return Err(KernelError::IntegrityFailed(format!(
                "accepted CSV {path:?} has an empty header identity"
            )));
        }
        files.push(SourceStructureFile {
            entity_id: if mapping.document_source_files().contains(&path) {
                Some(mapping.entity_for_source_file(&path)?.to_owned())
            } else {
                None
            },
            path,
            headers,
        });
    }
    Ok(SourceStructure {
        schema_version: SOURCE_STRUCTURE_SCHEMA.to_owned(),
        files,
    })
}

trait MappingDocumentTourExt {
    fn document_source_files(&self) -> &[String];
}

impl MappingDocumentTourExt for MappingDocument {
    fn document_source_files(&self) -> &[String] {
        &self.source.source_files
    }
}

fn load_reuse_authority(
    connection: &Connection,
    source_reuse_id: &str,
    verify_approvals: bool,
) -> Result<ReuseAuthorityRow> {
    let row: Option<(String, String, String)> = connection
        .query_row(
            "SELECT structure_json, canonical_reuse_json, source_reuse_hash
             FROM engagement_mapping_reuse_authorities
             WHERE source_reuse_id = ?1",
            [source_reuse_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((structure_json, canonical_json, source_reuse_hash)) = row else {
        return Err(KernelError::NotFound(format!(
            "mapping source-reuse authority {source_reuse_id}"
        )));
    };
    let canonical_value: Value = serde_json::from_str(&canonical_json).map_err(|error| {
        KernelError::IntegrityFailed(format!("mapping reuse JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&canonical_value)? != canonical_json
        || sha256_hex(canonical_json.as_bytes()) != source_reuse_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse canonical bytes/hash mismatch".to_owned(),
        ));
    }
    let evidence: MappingSourceReuseEvidence =
        serde_json::from_value(canonical_value).map_err(|error| {
            KernelError::IntegrityFailed(format!("mapping reuse contract is invalid: {error}"))
        })?;
    if evidence.schema_version != MAPPING_SOURCE_REUSE_SCHEMA
        || evidence.source_reuse_id != source_reuse_id
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse contract identity is invalid".to_owned(),
        ));
    }
    let predecessor = load_registered_tour(connection, &evidence.predecessor_tour_id)?;
    let predecessor_source_hash = evidence
        .baseline
        .predecessor_source_hash
        .as_deref()
        .unwrap_or(evidence.baseline.mapping_source_hash.as_str());
    if predecessor.evidence.engagement_id != evidence.engagement_id
        || predecessor.evidence.tour_id != evidence.predecessor_tour_id
        || predecessor.evidence.job_id != evidence.baseline.job_id
        || predecessor.evidence.source_hash != predecessor_source_hash
        || predecessor.evidence.profile_id != evidence.profile.profile_id
        || predecessor.evidence.profile_version != evidence.profile.profile_version
        || predecessor.evidence.profile_hash != evidence.profile.profile_hash
        || predecessor.evidence.mapping_id != evidence.mapping.mapping_id
        || predecessor.evidence.mapping_version != evidence.mapping.mapping_version
        || predecessor.evidence.mapping_hash != evidence.mapping.mapping_hash
        || predecessor.evidence.source_hash == evidence.candidate.source_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse predecessor authority differs from the immediate immutable revision"
                .to_owned(),
        ));
    }
    let structure_value: Value = serde_json::from_str(&structure_json).map_err(|error| {
        KernelError::IntegrityFailed(format!("mapping reuse structure JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&structure_value)? != structure_json {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse structure JSON is not canonical".to_owned(),
        ));
    }
    let structure: SourceStructure = serde_json::from_value(structure_value).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "mapping reuse structure contract is invalid: {error}"
        ))
    })?;
    let structure_hash = sha256_hex(structure_json.as_bytes());
    if structure.schema_version != SOURCE_STRUCTURE_SCHEMA
        || evidence.baseline.structure_hash != structure_hash
        || evidence.candidate.structure_hash != structure_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse structure hash/linkage differs".to_owned(),
        ));
    }
    let mapping = load_stored_mapping(
        connection,
        &evidence.mapping.mapping_id,
        &evidence.mapping.mapping_version,
    )?;
    if mapping.mapping_hash != evidence.mapping.mapping_hash
        || mapping.document.profile.profile_id != evidence.profile.profile_id
        || mapping.document.profile.version != evidence.profile.profile_version
        || mapping.document.profile.sha256 != evidence.profile.profile_hash
        || mapping.document.source.package_sha256 != evidence.baseline.mapping_source_hash
        || source_structure(connection, &evidence.baseline.job_id, &mapping.document)? != structure
        || source_structure(connection, &evidence.candidate.job_id, &mapping.document)? != structure
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse source/profile/mapping/structure replay failed".to_owned(),
        ));
    }
    let candidate_source = load_baseline_source_evidence(
        connection,
        &evidence.engagement_id,
        &evidence.candidate.job_id,
        &evidence.candidate.profiling_run_id,
    )?;
    if candidate_source.preflight_id != evidence.candidate.preflight_id
        || candidate_source.acceptance_attempt_id != evidence.candidate.acceptance_attempt_id
        || candidate_source.source_hash != evidence.candidate.source_hash
        || candidate_source.manifest_hash != evidence.candidate.manifest_hash
        || candidate_source.profiling_result_hash != evidence.candidate.profiling_result_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse candidate acceptance/profiling replay failed".to_owned(),
        ));
    }
    let normalized_identity: (Option<String>, Option<String>) = connection.query_row(
        "SELECT mapping_authority_id, mapping_authority_hash
         FROM engagement_mapping_reuse_authorities WHERE source_reuse_id = ?1",
        [source_reuse_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mapping_authority_id = normalized_identity.0.ok_or_else(|| {
        KernelError::IntegrityFailed("mapping reuse has no normalized mapping authority".to_owned())
    })?;
    let mapping_authority_hash = normalized_identity.1.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "mapping reuse has no normalized mapping authority hash".to_owned(),
        )
    })?;
    if evidence
        .mapping
        .authority_id
        .as_deref()
        .is_some_and(|value| value != mapping_authority_id)
        || evidence
            .mapping
            .authority_hash
            .as_deref()
            .is_some_and(|value| value != mapping_authority_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse canonical normalized authority differs from its immutable row"
                .to_owned(),
        ));
    }
    let mapping_authority = load_normalized_mapping_authority(
        connection,
        &mapping_authority_id,
        &mapping_authority_hash,
    )?;
    if mapping_authority.evidence.engagement_id != evidence.engagement_id
        || mapping_authority.evidence.job_id != evidence.candidate.job_id
        || mapping_authority.evidence.source_hash != evidence.candidate.source_hash
        || mapping_authority.evidence.mapping_id != evidence.mapping.mapping_id
        || mapping_authority.evidence.mapping_version != evidence.mapping.mapping_version
        || mapping_authority.evidence.mapping_hash != evidence.mapping.mapping_hash
        || mapping_authority.evidence.mapping_approval_id != evidence.mapping.approval_id
        || mapping_authority.evidence.mapping_approval_hash != evidence.mapping.approval_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse normalized authority failed exact candidate replay".to_owned(),
        ));
    }
    if verify_approvals {
        let profile_approval: (String, String) = connection.query_row(
            "SELECT approval_id, approval_hash FROM profile_approvals
             WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3",
            params![
                evidence.profile.profile_id,
                evidence.profile.profile_version,
                evidence.profile.profile_hash,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if profile_approval.0 != evidence.profile.approval_id
            || profile_approval.1 != evidence.profile.approval_hash
        {
            return Err(KernelError::IntegrityFailed(
                "mapping reuse profile approval replay failed".to_owned(),
            ));
        }
    }
    let stored: (
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
        Option<String>,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT engagement_id, tour_attempt_id, predecessor_tour_id,
                preflight_id, acceptance_attempt_id, candidate_job_id,
                candidate_profiling_run_id, candidate_source_hash,
                candidate_manifest_hash, candidate_profiling_result_hash,
                baseline_job_id, mapping_source_hash,
                baseline_structure_hash, candidate_structure_hash,
                profile_id, profile_version, profile_hash,
                profile_approval_id, profile_approval_hash,
                mapping_id, mapping_version, mapping_hash,
                mapping_approval_id, mapping_approval_hash,
                algorithm_version, verified_at, source_reuse_hash
         FROM engagement_mapping_reuse_authorities
         WHERE source_reuse_id = ?1",
        [source_reuse_id],
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
                row.get(17)?,
                row.get(18)?,
                row.get(19)?,
                row.get(20)?,
                row.get(21)?,
                row.get(22)?,
                row.get(23)?,
                row.get(24)?,
                row.get(25)?,
                row.get(26)?,
            ))
        },
    )?;
    let row_matches = stored.0 == evidence.engagement_id
        && stored.1 == evidence.tour_attempt_id
        && stored.2 == evidence.predecessor_tour_id
        && stored.3 == evidence.candidate.preflight_id
        && stored.4 == evidence.candidate.acceptance_attempt_id
        && stored.5 == evidence.candidate.job_id
        && stored.6 == evidence.candidate.profiling_run_id
        && stored.7 == evidence.candidate.source_hash
        && stored.8 == evidence.candidate.manifest_hash
        && stored.9 == evidence.candidate.profiling_result_hash
        && stored.10 == evidence.baseline.job_id
        && stored.11 == evidence.baseline.mapping_source_hash
        && stored.12 == evidence.baseline.structure_hash
        && stored.13 == evidence.candidate.structure_hash
        && stored.14 == evidence.profile.profile_id
        && stored.15 == evidence.profile.profile_version
        && stored.16 == evidence.profile.profile_hash
        && stored.17 == evidence.profile.approval_id
        && stored.18 == evidence.profile.approval_hash
        && stored.19 == evidence.mapping.mapping_id
        && stored.20 == evidence.mapping.mapping_version
        && stored.21 == evidence.mapping.mapping_hash
        && stored.22 == evidence.mapping.approval_id
        && stored.23 == evidence.mapping.approval_hash
        && stored.24 == MAPPING_SOURCE_REUSE_SCHEMA
        && stored.25 == evidence.verified_at
        && stored.26 == source_reuse_hash;
    if !row_matches {
        return Err(KernelError::IntegrityFailed(
            "mapping reuse canonical JSON differs from its immutable row".to_owned(),
        ));
    }
    Ok(ReuseAuthorityRow {
        evidence,
        source_reuse_hash,
        mapping_authority_id,
        mapping_authority_hash,
    })
}

fn reuse_authority_view(reuse: &ReuseAuthorityRow) -> MappingReuseAuthorityView {
    MappingReuseAuthorityView {
        reuse_authority_id: reuse.evidence.source_reuse_id.clone(),
        reuse_authority_hash: reuse.source_reuse_hash.clone(),
        baseline_tour_id: reuse.evidence.predecessor_tour_id.clone(),
        candidate_tour_attempt_id: reuse.evidence.tour_attempt_id.clone(),
        baseline_source_hash: reuse
            .evidence
            .baseline
            .predecessor_source_hash
            .clone()
            .unwrap_or_else(|| reuse.evidence.baseline.mapping_source_hash.clone()),
        mapping_origin_source_hash: reuse.evidence.baseline.mapping_source_hash.clone(),
        candidate_source_hash: reuse.evidence.candidate.source_hash.clone(),
        profile_id: reuse.evidence.profile.profile_id.clone(),
        profile_version: reuse.evidence.profile.profile_version.clone(),
        profile_hash: reuse.evidence.profile.profile_hash.clone(),
        profile_approval_id: reuse.evidence.profile.approval_id.clone(),
        profile_approval_hash: reuse.evidence.profile.approval_hash.clone(),
        mapping_id: reuse.evidence.mapping.mapping_id.clone(),
        mapping_version: reuse.evidence.mapping.mapping_version.clone(),
        mapping_hash: reuse.evidence.mapping.mapping_hash.clone(),
        mapping_approval_id: reuse.evidence.mapping.approval_id.clone(),
        mapping_approval_hash: reuse.evidence.mapping.approval_hash.clone(),
        mapping_authority_id: reuse.mapping_authority_id.clone(),
        mapping_authority_hash: reuse.mapping_authority_hash.clone(),
        structure_hash: reuse.evidence.candidate.structure_hash.clone(),
        created_at: reuse.evidence.verified_at.clone(),
    }
}

fn require_reused_validation_semantics(
    connection: &Connection,
    reuse: &ReuseAuthorityRow,
    validation_policy: &ValidationPolicy,
) -> Result<()> {
    let baseline = load_registered_tour(connection, &reuse.evidence.predecessor_tour_id)?;
    let finding_set = load_finding_set_for_tour(connection, &baseline.evidence.tour_id)?;
    let current_policy_hash = canonical_hash(&serde_json::to_value(validation_policy)?)?;
    if finding_set.authority.profile_id != reuse.evidence.profile.profile_id
        || finding_set.authority.profile_version != reuse.evidence.profile.profile_version
        || finding_set.authority.profile_hash != reuse.evidence.profile.profile_hash
        || finding_set.authority.mapping_id != reuse.evidence.mapping.mapping_id
        || finding_set.authority.mapping_version != reuse.evidence.mapping.mapping_version
        || finding_set.authority.mapping_hash != reuse.evidence.mapping.mapping_hash
        || finding_set.authority.validation_policy_hash != current_policy_hash
        || finding_set.authority.validation_engine_version
            != crate::validation::VALIDATION_ENGINE_VERSION
    {
        return Err(KernelError::Conflict(
            "source revision cannot reuse validation authority because its exact profile, mapping, rules, policy, or engine differs"
                .to_owned(),
        ));
    }
    Ok(())
}

fn subject_kind_sql(kind: ComparedSubjectKind) -> &'static str {
    match kind {
        ComparedSubjectKind::RuleRecord => "RULE_RECORD",
        ComparedSubjectKind::UniqueGroup => "UNIQUE_GROUP",
        ComparedSubjectKind::RelationshipRecord => "RELATIONSHIP_RECORD",
    }
}

fn subject_algorithm_sql(algorithm: ComparedSubjectAlgorithm) -> &'static str {
    match algorithm {
        ComparedSubjectAlgorithm::EntityKeyHmacSha256V1 => "ENTITY_KEY_HMAC_SHA256_V1",
        ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1 => "PROJECTED_ROW_HMAC_SHA256_V1",
        ComparedSubjectAlgorithm::UniqueScopeHmacSha256V1 => "UNIQUE_SCOPE_HMAC_SHA256_V1",
        ComparedSubjectAlgorithm::RelationshipEntityKeyHmacSha256V1 => {
            "RELATIONSHIP_ENTITY_KEY_HMAC_SHA256_V1"
        }
        ComparedSubjectAlgorithm::RelationshipProjectedRowHmacSha256V1 => {
            "RELATIONSHIP_PROJECTED_ROW_HMAC_SHA256_V1"
        }
    }
}

fn parse_subject_algorithm(value: &str) -> Result<ComparedSubjectAlgorithm> {
    match value {
        "ENTITY_KEY_HMAC_SHA256_V1" => Ok(ComparedSubjectAlgorithm::EntityKeyHmacSha256V1),
        "PROJECTED_ROW_HMAC_SHA256_V1" => Ok(ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1),
        "UNIQUE_SCOPE_HMAC_SHA256_V1" => Ok(ComparedSubjectAlgorithm::UniqueScopeHmacSha256V1),
        "RELATIONSHIP_ENTITY_KEY_HMAC_SHA256_V1" => {
            Ok(ComparedSubjectAlgorithm::RelationshipEntityKeyHmacSha256V1)
        }
        "RELATIONSHIP_PROJECTED_ROW_HMAC_SHA256_V1" => {
            Ok(ComparedSubjectAlgorithm::RelationshipProjectedRowHmacSha256V1)
        }
        _ => Err(KernelError::IntegrityFailed(
            "compared finding has an unsupported identity algorithm".to_owned(),
        )),
    }
}

fn compared_identity_basis_explanation(basis: ComparedIdentityBasis) -> &'static str {
    match basis {
        ComparedIdentityBasis::DeclaredKey => {
            "Compared by a private fingerprint of the complete declared business key; row and line positions are evidence only."
        }
        ComparedIdentityBasis::FullRowFallback => {
            "Compared as a multiset by a private full-row fingerprint because the declared key was incomplete. An edited row can appear as one CLOSED and one NEW finding; this does not assert record-to-record lineage."
        }
        ComparedIdentityBasis::UniqueScope => {
            "Compared as a multiset by a private fingerprint of the rule's ordered unique-scope tuple; row and line positions are evidence only."
        }
    }
}

fn relationship_role_sql(role: Option<RelationshipRole>) -> Option<&'static str> {
    role.map(|role| match role {
        RelationshipRole::Parent => "PARENT",
        RelationshipRole::Child => "CHILD",
    })
}

fn relationship_key_pair_identity_hash(
    mapping: &MappingDocument,
    group: &ComparedFindingGroup,
) -> Result<Option<String>> {
    if group.subject_kind != ComparedSubjectKind::RelationshipRecord {
        if group.relationship_id.is_some()
            || group.relationship_role.is_some()
            || group.relationship_cardinality.is_some()
        {
            return Err(KernelError::IntegrityFailed(
                "non-relationship finding exposes relationship metadata".to_owned(),
            ));
        }
        return Ok(None);
    }
    let relationship_id = group.relationship_id.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed("relationship finding has no relationship ID".to_owned())
    })?;
    let relationship = mapping
        .relationship_bindings
        .iter()
        .find(|candidate| candidate.relationship_id == relationship_id)
        .ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "relationship finding references unknown relationship {relationship_id:?}"
            ))
        })?;
    if group.relationship_cardinality.as_deref() != Some(relationship.cardinality.as_str()) {
        return Err(KernelError::IntegrityFailed(
            "relationship finding cardinality differs from immutable mapping".to_owned(),
        ));
    }
    canonical_hash(&json!({
        "contract": "migration-stream/compared-validation/relationship-key-pair-identity/v1",
        "relationshipId": relationship.relationship_id,
        "role": group.relationship_role,
        "cardinality": relationship.cardinality,
        "fromEntityId": relationship.from_entity_id,
        "toEntityId": relationship.to_entity_id,
        "keyPairs": relationship.key_pairs,
    }))
    .map(Some)
}

fn finding_member_value(
    finding_set_id: &str,
    set: &ComparedFindingSet,
    group: &ComparedFindingGroup,
    occurrence: &crate::compared_validation::ComparedFindingOccurrence,
    occurrence_index: u64,
    relationship_key_pair_identity_hash: Option<&str>,
) -> Value {
    json!({
        "contract": "migration-stream/compared-validation/finding-member/v1",
        "findingSetId": finding_set_id,
        "findingFingerprint": group.fingerprint,
        "occurrenceIndex": occurrence_index,
        "subjectKind": group.subject_kind,
        "subjectAlgorithm": group.subject_algorithm,
        "recordIdentityHash": group.fingerprint,
        "validationRunId": set.validation_run_id,
        "issueId": occurrence.issue_id,
        "issueHash": occurrence.issue_hash,
        "severity": group.severity,
        "ruleId": group.rule_id,
        "issueCode": group.issue_code,
        "displayMessage": group.display_message,
        "entityId": group.entity_id,
        "fieldId": group.field_id,
        "relationshipId": group.relationship_id,
        "relationshipRole": group.relationship_role,
        "relationshipCardinality": group.relationship_cardinality,
        "relationshipKeyPairIdentityHash": relationship_key_pair_identity_hash,
        "sourceFile": occurrence.source_file,
        "sourceOrdinal": occurrence.source_ordinal,
        "lineStart": occurrence.line_start,
        "lineEnd": occurrence.line_end,
    })
}

fn insert_finding_set(
    transaction: &Transaction<'_>,
    namespace: &ComparisonNamespace,
    tour_id: &str,
    set: &ComparedFindingSet,
    created_at: &str,
) -> Result<FindingSetPersistence> {
    if set.schema_version != COMPARED_FINDING_SET_SCHEMA || set.tour_id != tour_id {
        return Err(KernelError::IntegrityFailed(
            "compared finding set differs from its registered tour".to_owned(),
        ));
    }
    let finding_set_id = format!("finding-set-{}", Uuid::new_v4());
    let mapping = load_stored_mapping(
        transaction,
        &set.authority.mapping_id,
        &set.authority.mapping_version,
    )?;
    if mapping.mapping_hash != set.authority.mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "finding-set mapping authority differs from immutable mapping".to_owned(),
        ));
    }
    let mut member_rows = Vec::new();
    let mut member_hashes = BTreeMap::new();
    for group in &set.findings {
        let relationship_key_pair_identity_hash =
            relationship_key_pair_identity_hash(&mapping.document, group)?;
        for (offset, occurrence) in group.occurrences.iter().enumerate() {
            let occurrence_index = u64::try_from(offset + 1).map_err(|_| {
                KernelError::PolicyInvalid("finding occurrence index overflow".to_owned())
            })?;
            let member_value = finding_member_value(
                &finding_set_id,
                set,
                group,
                occurrence,
                occurrence_index,
                relationship_key_pair_identity_hash.as_deref(),
            );
            let member_json = canonical_json_string(&member_value)?;
            let member_hash = sha256_hex(member_json.as_bytes());
            member_hashes.insert(
                (group.fingerprint.clone(), occurrence_index),
                member_hash.clone(),
            );
            member_rows.push((
                group,
                occurrence,
                occurrence_index,
                relationship_key_pair_identity_hash.clone(),
                member_json,
                member_hash,
            ));
        }
    }
    let membership_hash = canonical_hash(&json!({
        "contract": "migration-stream/compared-validation/finding-membership/v1",
        "findingSetId": finding_set_id,
        "members": member_hashes.iter().map(|((fingerprint, occurrence), hash)| json!({
            "fingerprint": fingerprint,
            "occurrence": occurrence,
            "memberHash": hash,
        })).collect::<Vec<_>>(),
    }))?;
    let canonical_set_json = canonical_json_string(&serde_json::to_value(set)?)?;
    transaction.execute(
        "INSERT INTO engagement_tour_finding_sets (
             finding_set_id, tour_id, namespace_id, namespace_secret_hash,
             algorithm_version, validation_run_id, validation_result_hash,
             profile_hash, mapping_hash, rule_set_hash,
             validation_engine_version, validation_policy_hash,
             distinct_finding_count, total_occurrence_count,
             membership_hash, canonical_set_json, finding_set_hash, created_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
             ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
        params![
            finding_set_id,
            tour_id,
            namespace.namespace_id,
            namespace.secret_hash,
            COMPARISON_FINGERPRINT_ALGORITHM,
            set.validation_run_id,
            set.validation_result_hash,
            set.authority.profile_hash,
            set.authority.mapping_hash,
            set.authority.rule_set_hash,
            set.authority.validation_engine_version,
            set.authority.validation_policy_hash,
            sqlite_tour_integer(set.finding_count, "distinct finding count")?,
            sqlite_tour_integer(set.issue_occurrence_count, "finding occurrence count")?,
            membership_hash,
            canonical_set_json,
            set.canonical_hash,
            created_at,
        ],
    )?;
    for (
        group,
        occurrence,
        occurrence_index,
        relationship_key_pair_identity_hash,
        member_json,
        member_hash,
    ) in member_rows
    {
        transaction.execute(
            "INSERT INTO engagement_tour_finding_members (
                 finding_set_id, finding_fingerprint, occurrence_index,
                 subject_kind, subject_algorithm, record_identity_hash,
                 validation_run_id, issue_id, issue_hash, severity,
                 rule_id, issue_code, display_message, entity_id, field_id,
                 relationship_id, relationship_role,
                 relationship_cardinality, relationship_key_pair_hash,
                 source_file, source_ordinal, line_start, line_end,
                 canonical_member_json, member_hash
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                 ?20, ?21, ?22, ?23, ?24, ?25
             )",
            params![
                finding_set_id,
                group.fingerprint,
                sqlite_tour_integer(occurrence_index, "finding occurrence index")?,
                subject_kind_sql(group.subject_kind),
                subject_algorithm_sql(group.subject_algorithm),
                group.fingerprint,
                set.validation_run_id,
                occurrence.issue_id,
                occurrence.issue_hash,
                group.severity,
                group.rule_id,
                group.issue_code,
                group.display_message,
                group.entity_id,
                group.field_id,
                group.relationship_id,
                relationship_role_sql(group.relationship_role),
                group.relationship_cardinality,
                relationship_key_pair_identity_hash,
                occurrence.source_file,
                sqlite_tour_integer(occurrence.source_ordinal, "finding source ordinal")?,
                sqlite_tour_integer(occurrence.line_start, "finding line start")?,
                sqlite_tour_integer(occurrence.line_end, "finding line end")?,
                member_json,
                member_hash,
            ],
        )?;
    }
    Ok(FindingSetPersistence {
        finding_set_id,
        membership_hash,
        member_hashes,
    })
}

fn load_finding_set_for_tour(connection: &Connection, tour_id: &str) -> Result<ComparedFindingSet> {
    let (canonical_json, finding_set_hash): (String, String) = connection.query_row(
        "SELECT canonical_set_json, finding_set_hash
         FROM engagement_tour_finding_sets WHERE tour_id = ?1",
        [tour_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let value: Value = serde_json::from_str(&canonical_json).map_err(|error| {
        KernelError::IntegrityFailed(format!("finding set JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != canonical_json {
        return Err(KernelError::IntegrityFailed(
            "finding set JSON is not canonical".to_owned(),
        ));
    }
    let set: ComparedFindingSet = serde_json::from_value(value).map_err(|error| {
        KernelError::IntegrityFailed(format!("finding set contract is invalid: {error}"))
    })?;
    if set.tour_id != tour_id || set.canonical_hash != finding_set_hash {
        return Err(KernelError::IntegrityFailed(
            "finding set identity/hash differs from its immutable row".to_owned(),
        ));
    }
    Ok(set)
}

fn load_finding_set_persistence(
    connection: &Connection,
    namespace: &ComparisonNamespace,
    tour_id: &str,
    set: &ComparedFindingSet,
) -> Result<FindingSetPersistence> {
    let row: (
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
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT finding_set_id, namespace_id, namespace_secret_hash,
                algorithm_version, validation_run_id, validation_result_hash,
                profile_hash, mapping_hash, rule_set_hash,
                validation_engine_version, validation_policy_hash,
                distinct_finding_count, total_occurrence_count,
                membership_hash, canonical_set_json, finding_set_hash, created_at
         FROM engagement_tour_finding_sets WHERE tour_id = ?1",
        [tour_id],
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
    )?;
    let tour = load_registered_tour(connection, tour_id)?;
    let expected_set_json = canonical_json_string(&serde_json::to_value(set)?)?;
    if set.tour_id != tour_id
        || row.1 != namespace.namespace_id
        || row.2 != namespace.secret_hash
        || row.3 != COMPARISON_FINGERPRINT_ALGORITHM
        || row.4 != set.validation_run_id
        || row.5 != set.validation_result_hash
        || row.6 != set.authority.profile_hash
        || row.7 != set.authority.mapping_hash
        || row.8 != set.authority.rule_set_hash
        || row.9 != set.authority.validation_engine_version
        || row.10 != set.authority.validation_policy_hash
        || nonnegative_i64(row.11, "finding-set distinct count")? != set.finding_count
        || nonnegative_i64(row.12, "finding-set occurrence count")? != set.issue_occurrence_count
        || row.14 != expected_set_json
        || row.15 != set.canonical_hash
        || row.16 != tour.evidence.registered_at
    {
        return Err(KernelError::IntegrityFailed(
            "finding-set persisted authority/count/time differs from independent replay".to_owned(),
        ));
    }

    let mapping = load_stored_mapping(
        connection,
        &set.authority.mapping_id,
        &set.authority.mapping_version,
    )?;
    if mapping.mapping_hash != set.authority.mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "finding-set member replay mapping differs from exact authority".to_owned(),
        ));
    }
    let mut expected_members = BTreeMap::<(String, u64), (String, String)>::new();
    for group in &set.findings {
        let relationship_hash = relationship_key_pair_identity_hash(&mapping.document, group)?;
        for (offset, occurrence) in group.occurrences.iter().enumerate() {
            let occurrence_index = u64::try_from(offset + 1).map_err(|_| {
                KernelError::PolicyInvalid("finding occurrence index overflow".to_owned())
            })?;
            let value = finding_member_value(
                &row.0,
                set,
                group,
                occurrence,
                occurrence_index,
                relationship_hash.as_deref(),
            );
            let canonical = canonical_json_string(&value)?;
            let hash = sha256_hex(canonical.as_bytes());
            if expected_members
                .insert(
                    (group.fingerprint.clone(), occurrence_index),
                    (canonical, hash),
                )
                .is_some()
            {
                return Err(KernelError::IntegrityFailed(
                    "independently replayed finding set repeats a member identity".to_owned(),
                ));
            }
        }
    }

    let mut actual_member_hashes = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT finding_fingerprint, occurrence_index,
                subject_kind, subject_algorithm, record_identity_hash,
                validation_run_id, issue_id, issue_hash, severity,
                rule_id, issue_code, display_message, entity_id, field_id,
                relationship_id, relationship_role,
                relationship_cardinality, relationship_key_pair_hash,
                source_file, source_ordinal, line_start, line_end,
                canonical_member_json, member_hash
             FROM engagement_tour_finding_members
             WHERE finding_set_id = ?1
             ORDER BY finding_fingerprint, occurrence_index",
    )?;
    let mut rows = statement.query([&row.0])?;
    while let Some(member) = rows.next()? {
        let fingerprint: String = member.get(0)?;
        let occurrence_index = nonnegative_i64(member.get(1)?, "finding occurrence index")?;
        let member_value = json!({
            "contract": "migration-stream/compared-validation/finding-member/v1",
            "findingSetId": row.0,
            "findingFingerprint": fingerprint,
            "occurrenceIndex": occurrence_index,
            "subjectKind": member.get::<_, String>(2)?,
            "subjectAlgorithm": member.get::<_, String>(3)?,
            "recordIdentityHash": member.get::<_, String>(4)?,
            "validationRunId": member.get::<_, String>(5)?,
            "issueId": member.get::<_, String>(6)?,
            "issueHash": member.get::<_, String>(7)?,
            "severity": member.get::<_, String>(8)?,
            "ruleId": member.get::<_, String>(9)?,
            "issueCode": member.get::<_, String>(10)?,
            "displayMessage": member.get::<_, String>(11)?,
            "entityId": member.get::<_, String>(12)?,
            "fieldId": member.get::<_, String>(13)?,
            "relationshipId": member.get::<_, Option<String>>(14)?,
            "relationshipRole": member.get::<_, Option<String>>(15)?,
            "relationshipCardinality": member.get::<_, Option<String>>(16)?,
            "relationshipKeyPairIdentityHash": member.get::<_, Option<String>>(17)?,
            "sourceFile": member.get::<_, String>(18)?,
            "sourceOrdinal": nonnegative_i64(member.get(19)?, "finding source ordinal")?,
            "lineStart": nonnegative_i64(member.get(20)?, "finding line start")?,
            "lineEnd": nonnegative_i64(member.get(21)?, "finding line end")?,
        });
        let actual_json = canonical_json_string(&member_value)?;
        let stored_json: String = member.get(22)?;
        let stored_hash: String = member.get(23)?;
        let Some((expected_json, expected_hash)) =
            expected_members.remove(&(fingerprint.clone(), occurrence_index))
        else {
            return Err(KernelError::IntegrityFailed(format!(
                "finding member {fingerprint}/{occurrence_index} is absent from independent finding-set replay"
            )));
        };
        if actual_json != stored_json
            || sha256_hex(actual_json.as_bytes()) != stored_hash
            || actual_json != expected_json
            || stored_hash != expected_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "finding member {fingerprint}/{occurrence_index} differs from independent finding-set replay"
            )));
        }
        actual_member_hashes.insert((fingerprint, occurrence_index), stored_hash);
    }
    if !expected_members.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "finding member storage omits independently replayed occurrences".to_owned(),
        ));
    }
    let expected_membership_hash = canonical_hash(&json!({
        "contract": "migration-stream/compared-validation/finding-membership/v1",
        "findingSetId": row.0,
        "members": actual_member_hashes.iter().map(|((fingerprint, occurrence), hash)| json!({
            "fingerprint": fingerprint,
            "occurrence": occurrence,
            "memberHash": hash,
        })).collect::<Vec<_>>(),
    }))?;
    if expected_membership_hash != row.13 {
        return Err(KernelError::IntegrityFailed(
            "finding-set membership hash failed replay".to_owned(),
        ));
    }
    Ok(FindingSetPersistence {
        finding_set_id: row.0,
        membership_hash: row.13,
        member_hashes: actual_member_hashes,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn insert_comparison(
    transaction: &Transaction<'_>,
    engagement_id: &str,
    namespace: &ComparisonNamespace,
    baseline_tour: &RegisteredTourRow,
    candidate_tour: &RegisteredTourRow,
    baseline_set: &ComparedFindingSet,
    candidate_set: &ComparedFindingSet,
    baseline_persistence: &FindingSetPersistence,
    candidate_persistence: &FindingSetPersistence,
    comparison: &ComparedValidation,
    created_at: &str,
) -> Result<()> {
    if comparison.prior.tour_id != baseline_tour.evidence.tour_id
        || comparison.current.tour_id != candidate_tour.evidence.tour_id
        || comparison.prior.finding_set_hash != baseline_set.canonical_hash
        || comparison.current.finding_set_hash != candidate_set.canonical_hash
        || comparison.pseudonym_namespace_id != baseline_set.pseudonym_namespace_id
    {
        return Err(KernelError::IntegrityFailed(
            "comparison inputs differ from exact immutable tour finding sets".to_owned(),
        ));
    }
    let comparison_id = format!("comparison-{}", Uuid::new_v4());
    let comparison_json = canonical_json_string(&serde_json::to_value(comparison)?)?;
    transaction.execute(
        "INSERT INTO engagement_validation_comparisons (
             comparison_id, engagement_id, baseline_tour_id, revised_tour_id,
             baseline_finding_set_id, baseline_finding_set_hash,
             baseline_membership_hash, revised_finding_set_id,
             revised_finding_set_hash, revised_membership_hash,
             namespace_id, namespace_secret_hash, algorithm_version,
             profile_id, profile_version, profile_hash,
             mapping_id, mapping_version, mapping_hash,
             rule_set_hash, validation_engine_version,
             validation_policy_hash, closed_count, remaining_count, new_count,
             canonical_comparison_json, comparison_hash, created_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
             ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
         )",
        params![
            comparison_id,
            engagement_id,
            baseline_tour.evidence.tour_id,
            candidate_tour.evidence.tour_id,
            baseline_persistence.finding_set_id,
            baseline_set.canonical_hash,
            baseline_persistence.membership_hash,
            candidate_persistence.finding_set_id,
            candidate_set.canonical_hash,
            candidate_persistence.membership_hash,
            namespace.namespace_id,
            namespace.secret_hash,
            COMPARISON_FINGERPRINT_ALGORITHM,
            comparison.authority.profile_id,
            comparison.authority.profile_version,
            comparison.authority.profile_hash,
            comparison.authority.mapping_id,
            comparison.authority.mapping_version,
            comparison.authority.mapping_hash,
            comparison.authority.rule_set_hash,
            comparison.authority.validation_engine_version,
            comparison.authority.validation_policy_hash,
            sqlite_tour_integer(comparison.counts.closed, "closed finding count")?,
            sqlite_tour_integer(comparison.counts.remaining, "remaining finding count")?,
            sqlite_tour_integer(comparison.counts.new, "new finding count")?,
            comparison_json,
            comparison.canonical_hash,
            created_at,
        ],
    )?;

    for delta in &comparison.findings {
        for index in 1..=delta.remaining_count {
            insert_comparison_item(
                transaction,
                &comparison_id,
                "REMAINING",
                &delta.fingerprint,
                index,
                Some((baseline_persistence, index, "baseline remaining occurrence")),
                Some((candidate_persistence, index, "revised remaining occurrence")),
                "MULTISET_REMAINING",
            )?;
        }
        for index in 1..=delta.closed_count {
            let baseline_index = delta.remaining_count.checked_add(index).ok_or_else(|| {
                KernelError::PolicyInvalid("closed finding occurrence index overflow".to_owned())
            })?;
            insert_comparison_item(
                transaction,
                &comparison_id,
                "CLOSED",
                &delta.fingerprint,
                index,
                Some((
                    baseline_persistence,
                    baseline_index,
                    "baseline closed occurrence",
                )),
                None,
                "MULTISET_CLOSED",
            )?;
        }
        for index in 1..=delta.new_count {
            let candidate_index = delta.remaining_count.checked_add(index).ok_or_else(|| {
                KernelError::PolicyInvalid("new finding occurrence index overflow".to_owned())
            })?;
            insert_comparison_item(
                transaction,
                &comparison_id,
                "NEW",
                &delta.fingerprint,
                index,
                None,
                Some((
                    candidate_persistence,
                    candidate_index,
                    "revised new occurrence",
                )),
                "MULTISET_NEW",
            )?;
        }
    }
    Ok(())
}

fn comparison_item_value(
    comparison_id: &str,
    category: &str,
    fingerprint: &str,
    category_occurrence_index: u64,
    baseline: Option<(&str, u64, &str)>,
    revised: Option<(&str, u64, &str)>,
    explanation_code: &str,
) -> Value {
    json!({
        "contract": "migration-stream/compared-validation/item/v1",
        "comparisonId": comparison_id,
        "category": category,
        "findingFingerprint": fingerprint,
        "categoryOccurrenceIndex": category_occurrence_index,
        "baseline": baseline.map(|(set, index, hash)| json!({
            "findingSetId": set,
            "occurrenceIndex": index,
            "memberHash": hash,
        })),
        "revised": revised.map(|(set, index, hash)| json!({
            "findingSetId": set,
            "occurrenceIndex": index,
            "memberHash": hash,
        })),
        "explanationCode": explanation_code,
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_comparison_item(
    transaction: &Transaction<'_>,
    comparison_id: &str,
    category: &str,
    fingerprint: &str,
    category_occurrence_index: u64,
    baseline: Option<(&FindingSetPersistence, u64, &str)>,
    candidate: Option<(&FindingSetPersistence, u64, &str)>,
    explanation_code: &str,
) -> Result<()> {
    let baseline_member_hash = baseline
        .as_ref()
        .and_then(|(set, index, _)| set.member_hashes.get(&(fingerprint.to_owned(), *index)))
        .cloned();
    let candidate_member_hash = candidate
        .as_ref()
        .and_then(|(set, index, _)| set.member_hashes.get(&(fingerprint.to_owned(), *index)))
        .cloned();
    if baseline.is_some() != baseline_member_hash.is_some()
        || candidate.is_some() != candidate_member_hash.is_some()
    {
        return Err(KernelError::IntegrityFailed(
            "comparison occurrence does not resolve to exact finding membership".to_owned(),
        ));
    }
    let baseline_identity = match (baseline.as_ref(), baseline_member_hash.as_deref()) {
        (Some((set, index, _)), Some(hash)) => Some((set.finding_set_id.as_str(), *index, hash)),
        (None, None) => None,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "baseline comparison membership presence is inconsistent".to_owned(),
            ));
        }
    };
    let candidate_identity = match (candidate.as_ref(), candidate_member_hash.as_deref()) {
        (Some((set, index, _)), Some(hash)) => Some((set.finding_set_id.as_str(), *index, hash)),
        (None, None) => None,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "revised comparison membership presence is inconsistent".to_owned(),
            ));
        }
    };
    let item_value = comparison_item_value(
        comparison_id,
        category,
        fingerprint,
        category_occurrence_index,
        baseline_identity,
        candidate_identity,
        explanation_code,
    );
    let item_json = canonical_json_string(&item_value)?;
    let item_hash = sha256_hex(item_json.as_bytes());
    transaction.execute(
        "INSERT INTO engagement_validation_comparison_items (
             comparison_id, category, finding_fingerprint,
             category_occurrence_index, baseline_finding_set_id,
             baseline_occurrence_index, baseline_member_hash,
             revised_finding_set_id, revised_occurrence_index,
             revised_member_hash, explanation_code,
             canonical_item_json, item_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            comparison_id,
            category,
            fingerprint,
            sqlite_tour_integer(
                category_occurrence_index,
                "comparison category occurrence index"
            )?,
            baseline.map(|(set, _, _)| set.finding_set_id.as_str()),
            baseline
                .map(|(_, index, _)| {
                    sqlite_tour_integer(index, "baseline comparison occurrence index")
                })
                .transpose()?,
            baseline_member_hash,
            candidate.map(|(set, _, _)| set.finding_set_id.as_str()),
            candidate
                .map(|(_, index, _)| {
                    sqlite_tour_integer(index, "revised comparison occurrence index")
                })
                .transpose()?,
            candidate_member_hash,
            explanation_code,
            item_json,
            item_hash,
        ],
    )?;
    Ok(())
}

fn tour_summary(tour: &RegisteredTourRow) -> TourSummaryView {
    let evidence = &tour.evidence;
    TourSummaryView {
        tour_id: evidence.tour_id.clone(),
        tour_number: evidence.tour_number,
        predecessor_tour_id: evidence.predecessor_tour_id.clone(),
        source_hash: evidence.source_hash.clone(),
        manifest_hash: evidence.manifest_hash.clone(),
        profiling_run_id: evidence.profiling_run_id.clone(),
        profile_id: evidence.profile_id.clone(),
        profile_version: evidence.profile_version.clone(),
        profile_hash: evidence.profile_hash.clone(),
        mapping_id: evidence.mapping_id.clone(),
        mapping_version: evidence.mapping_version.clone(),
        mapping_hash: evidence.mapping_hash.clone(),
        mapping_authority_id: tour.mapping_authority_id.clone(),
        mapping_authority_hash: tour.mapping_authority_hash.clone(),
        validation_run_id: evidence.validation_run_id.clone(),
        validation_result_hash: evidence.validation_result_hash.clone(),
        tour_hash: tour.tour_hash.clone(),
        completed_at: evidence.registered_at.clone(),
    }
}

fn load_tour_chain(
    connection: &Connection,
    category: Option<ComparedFindingCategory>,
) -> Result<TourChainView> {
    let engagement_id = require_engagement_id(connection)?;
    let head = load_tour_head(connection, &engagement_id)?;
    let Some(head) = head else {
        let generation: i64 = connection.query_row(
            "SELECT generation FROM engagement_active_context WHERE engagement_id = ?1",
            [&engagement_id],
            |row| row.get(0),
        )?;
        return Ok(TourChainView {
            schema_version: TOUR_CHAIN_SCHEMA.to_owned(),
            engagement_generation: u64::try_from(generation).unwrap_or(u64::MAX),
            active_tour: None,
            pending_attempt: None,
            latest_comparison: None,
            tours: Vec::new(),
            comparisons: Vec::new(),
        });
    };
    let active_tour = head
        .active_tour_id
        .as_deref()
        .map(|tour_id| load_registered_tour(connection, tour_id))
        .transpose()?;
    let pending_attempt = head
        .pending_tour_attempt_id
        .as_deref()
        .map(|attempt_id| pending_attempt_view(connection, &head, attempt_id))
        .transpose()?;
    let tour_rows = connection
        .prepare(
            "SELECT tour_id FROM engagement_tours
             WHERE engagement_id = ?1 ORDER BY tour_number, tour_id
             LIMIT ?2",
        )?
        .query_map(
            params![
                engagement_id,
                sqlite_tour_integer(MAXIMUM_TOURS_PER_ENGAGEMENT, "maximum tour count")?
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tours = Vec::with_capacity(tour_rows.len());
    let mut comparisons = Vec::with_capacity(tour_rows.len().saturating_sub(1));
    for tour_id in tour_rows {
        let tour = load_registered_tour(connection, &tour_id)?;
        if tour.evidence.tour_number > 1 {
            comparisons.push(load_comparison_view(
                connection,
                &tour.evidence.tour_id,
                category,
            )?);
        }
        tours.push(tour_summary(&tour));
    }
    let latest_comparison = comparisons.last().cloned();
    Ok(TourChainView {
        schema_version: TOUR_CHAIN_SCHEMA.to_owned(),
        engagement_generation: head.generation,
        active_tour: active_tour.as_ref().map(tour_summary),
        pending_attempt,
        latest_comparison,
        tours,
        comparisons,
    })
}

fn pending_attempt_view(
    connection: &Connection,
    head: &TourHeadRow,
    tour_attempt_id: &str,
) -> Result<RevisedTourAttemptView> {
    let attempt = load_tour_attempt(connection, tour_attempt_id)?;
    let reserve = load_tour_command_by_kind(connection, tour_attempt_id, "RESERVE_SOURCE")?;
    let accept = load_tour_command_by_kind(connection, tour_attempt_id, "ACCEPT_SOURCE")?;
    let launch = load_tour_command_by_kind(connection, tour_attempt_id, "LAUNCH_VALIDATION")?;
    let mut state = RevisedTourAttemptState::Started;
    let mut preflight_id = None;
    let mut source_hash = None;
    let mut manifest_hash = None;
    let mut job_id = None;
    let mut profiling_run_id = None;
    let mut structure_hash = None;
    let mut reuse_authority_id = None;
    let mut reuse_authority_hash = None;
    let mut validation_run_id = None;
    let mut error_code = None;
    let mut error_message = None;
    let mut updated_at = attempt.evidence.created_at.clone();

    if let Some(reserve) = reserve {
        preflight_id = Some(reserve.result_reference.clone());
        let preflight = load_preflight(connection, &reserve.result_reference)?;
        updated_at.clone_from(&preflight.updated_at);
        match preflight.state.as_str() {
            "RECEIVING" => state = RevisedTourAttemptState::PreflightReceiving,
            "READY" => {
                state = RevisedTourAttemptState::PreflightReady;
                source_hash = preflight.source_hash;
                manifest_hash = preflight.manifest_hash;
            }
            "REJECTED" | "INTERRUPTED" => {
                state = RevisedTourAttemptState::Failed;
                error_code = preflight.error_code;
                error_message = preflight.error_message;
            }
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "pending tour preflight has an unknown state".to_owned(),
                ));
            }
        }
    }
    if let Some(accept) = accept {
        if accept.result_reference == "MAPPING_REQUIRED" {
            state = RevisedTourAttemptState::Incompatible;
            error_code = Some("MAPPING_REQUIRED".to_owned());
            error_message = Some(
                "The refreshed source structure differs from the approved file/header/entity routing; create and approve a new mapping before validation."
                    .to_owned(),
            );
            let accepted_candidates = connection
                .prepare(
                    "SELECT a.job_id, a.profiling_run_id,
                            a.accepted_source_hash, a.accepted_manifest_hash,
                            p.result_hash
                     FROM engagement_acceptance_attempts a
                     JOIN profiling_runs p ON p.id = a.profiling_run_id
                     WHERE a.engagement_id = ?1 AND a.preflight_id = ?2
                       AND a.job_id IS NOT NULL AND a.profiling_run_id IS NOT NULL
                       AND p.state = 'COMPLETE'
                     ORDER BY a.attempt_id",
                )?
                .query_map(
                    params![attempt.evidence.engagement_id, preflight_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if accepted_candidates.len() != 1 {
                return Err(KernelError::IntegrityFailed(
                    "mapping-required revision has no unique accepted candidate evidence"
                        .to_owned(),
                ));
            }
            let candidate = &accepted_candidates[0];
            require_sample_free_profiling(
                connection,
                &candidate.0,
                &candidate.1,
                &candidate.2,
                &candidate.4,
            )?;
            let predecessor =
                load_registered_tour(connection, &attempt.evidence.predecessor_tour_id)?;
            let mapping = load_stored_mapping(
                connection,
                &predecessor.evidence.mapping_id,
                &predecessor.evidence.mapping_version,
            )?;
            let candidate_structure =
                source_structure(connection, &candidate.0, &mapping.document)?;
            job_id = Some(candidate.0.clone());
            profiling_run_id = Some(candidate.1.clone());
            source_hash = Some(candidate.2.clone());
            manifest_hash = Some(candidate.3.clone());
            structure_hash = Some(canonical_hash(&serde_json::to_value(candidate_structure)?)?);
            updated_at = connection.query_row(
                "SELECT updated_at FROM engagement_acceptance_attempts
                 WHERE engagement_id = ?1 AND preflight_id = ?2 AND job_id = ?3",
                params![attempt.evidence.engagement_id, preflight_id, candidate.0],
                |row| row.get(0),
            )?;
        } else {
            let reuse = load_reuse_authority(connection, &accept.result_reference, true)?;
            state = RevisedTourAttemptState::ReadyToValidate;
            preflight_id = Some(reuse.evidence.candidate.preflight_id.clone());
            source_hash = Some(reuse.evidence.candidate.source_hash.clone());
            manifest_hash = Some(reuse.evidence.candidate.manifest_hash.clone());
            job_id = Some(reuse.evidence.candidate.job_id.clone());
            profiling_run_id = Some(reuse.evidence.candidate.profiling_run_id.clone());
            structure_hash = Some(reuse.evidence.candidate.structure_hash.clone());
            reuse_authority_id = Some(reuse.evidence.source_reuse_id.clone());
            reuse_authority_hash = Some(reuse.source_reuse_hash.clone());
            updated_at = reuse.evidence.verified_at;
        }
    }
    if let Some(launch) = launch {
        validation_run_id = Some(launch.result_reference.clone());
        let status: Option<(String, Option<String>, Option<String>, String)> = connection
            .query_row(
                "SELECT state, error_code, error_message, updated_at
                 FROM validation_runs WHERE id = ?1",
                [&launch.result_reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((run_state, run_error_code, run_error_message, run_updated_at)) = status {
            updated_at = run_updated_at;
            match run_state.as_str() {
                "RUNNING" | "COMPLETE" => state = RevisedTourAttemptState::Validating,
                "FAILED" | "INTERRUPTED" | "BOUND_EXCEEDED" => {
                    state = RevisedTourAttemptState::Failed;
                    error_code = run_error_code;
                    error_message = run_error_message;
                }
                _ => {
                    return Err(KernelError::IntegrityFailed(
                        "pending revised validation has an unknown state".to_owned(),
                    ));
                }
            }
        } else {
            state = RevisedTourAttemptState::Validating;
        }
    }
    Ok(RevisedTourAttemptView {
        tour_attempt_id: attempt.evidence.tour_attempt_id,
        tour_number: attempt.evidence.tour_number,
        baseline_tour_id: attempt.evidence.predecessor_tour_id,
        state,
        generation: head.generation,
        preflight_id,
        source_hash,
        manifest_hash,
        job_id,
        profiling_run_id,
        structure_hash,
        reuse_authority_id,
        reuse_authority_hash,
        validation_run_id,
        error_code,
        error_message,
        actor: attempt.evidence.actor,
        reason: attempt.evidence.reason,
        created_at: attempt.evidence.created_at,
        updated_at,
    })
}

fn load_completed_launch_result(
    connection: &Connection,
    tour_attempt_id: &str,
) -> Result<Option<(TourSummaryView, ComparedValidationView)>> {
    let tour_id: Option<String> = connection
        .query_row(
            "SELECT tour_id FROM engagement_tours WHERE tour_attempt_id = ?1",
            [tour_attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(tour_id) = tour_id else {
        return Ok(None);
    };
    let tour = load_registered_tour(connection, &tour_id)?;
    let comparison = load_comparison_view(connection, &tour_id, None)?;
    Ok(Some((tour_summary(&tour), comparison)))
}

#[allow(clippy::too_many_lines)]
fn load_comparison_view(
    connection: &Connection,
    revised_tour_id: &str,
    category: Option<ComparedFindingCategory>,
) -> Result<ComparedValidationView> {
    let mut integrity_issues = Vec::new();
    verify_integrity(connection, &mut integrity_issues)?;
    if let Some(issue) = integrity_issues.first() {
        return Err(KernelError::IntegrityFailed(format!(
            "compared-validation read failed global replay: {issue}"
        )));
    }
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT comparison_id, baseline_tour_id,
                canonical_comparison_json, comparison_hash,
                algorithm_version, created_at,
                baseline_finding_set_id, revised_finding_set_id
         FROM engagement_validation_comparisons
         WHERE revised_tour_id = ?1",
        [revised_tour_id],
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
    )?;
    let value: Value = serde_json::from_str(&row.2).map_err(|error| {
        KernelError::IntegrityFailed(format!("comparison JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != row.2 {
        return Err(KernelError::IntegrityFailed(
            "comparison JSON is not canonical".to_owned(),
        ));
    }
    let comparison: ComparedValidation = serde_json::from_value(value).map_err(|error| {
        KernelError::IntegrityFailed(format!("comparison contract is invalid: {error}"))
    })?;
    if comparison.canonical_hash != row.3
        || comparison.prior.tour_id != row.1
        || comparison.current.tour_id != revised_tour_id
        || row.4 != COMPARISON_FINGERPRINT_ALGORITHM
    {
        return Err(KernelError::IntegrityFailed(
            "comparison canonical evidence differs from immutable linkage".to_owned(),
        ));
    }
    let category_sql = category.map(|value| match value {
        ComparedFindingCategory::Closed => "CLOSED",
        ComparedFindingCategory::Remaining => "REMAINING",
        ComparedFindingCategory::New => "NEW",
    });
    let mut statement = connection.prepare(
        "SELECT i.item_hash, i.category, i.finding_fingerprint,
                i.category_occurrence_index,
                COALESCE(r.subject_algorithm, b.subject_algorithm),
                COALESCE(r.severity, b.severity),
                COALESCE(r.rule_id, b.rule_id),
                COALESCE(r.issue_code, b.issue_code),
                COALESCE(r.display_message, b.display_message),
                COALESCE(r.entity_id, b.entity_id),
                COALESCE(r.field_id, b.field_id),
                COALESCE(r.relationship_id, b.relationship_id),
                COALESCE(r.relationship_role, b.relationship_role),
                b.issue_id, b.issue_hash, r.issue_id, r.issue_hash
         FROM engagement_validation_comparison_items i
         LEFT JOIN engagement_tour_finding_members b
           ON b.finding_set_id = i.baseline_finding_set_id
          AND b.finding_fingerprint = i.finding_fingerprint
          AND b.occurrence_index = i.baseline_occurrence_index
         LEFT JOIN engagement_tour_finding_members r
           ON r.finding_set_id = i.revised_finding_set_id
          AND r.finding_fingerprint = i.finding_fingerprint
          AND r.occurrence_index = i.revised_occurrence_index
         WHERE i.comparison_id = ?1
           AND (?2 IS NULL OR i.category = ?2)
         ORDER BY CASE i.category
                    WHEN 'CLOSED' THEN 1
                    WHEN 'REMAINING' THEN 2
                    ELSE 3
                  END,
                  i.finding_fingerprint, i.category_occurrence_index
         LIMIT ?3",
    )?;
    let findings = statement
        .query_map(
            params![
                row.0,
                category_sql,
                i64::try_from(MAXIMUM_COMPARISON_PAGE_SIZE).unwrap_or(i64::MAX)
            ],
            |row| {
                let category = match row.get::<_, String>(1)?.as_str() {
                    "CLOSED" => ComparedFindingCategory::Closed,
                    "REMAINING" => ComparedFindingCategory::Remaining,
                    "NEW" => ComparedFindingCategory::New,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                let occurrence = row.get::<_, i64>(3)?;
                let subject_algorithm_value: String = row.get(4)?;
                let subject_algorithm = parse_subject_algorithm(&subject_algorithm_value)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let identity_basis = subject_algorithm.identity_basis();
                let baseline_issue_id: Option<String> = row.get(13)?;
                let baseline_issue_hash: Option<String> = row.get(14)?;
                let candidate_issue_id: Option<String> = row.get(15)?;
                let candidate_issue_hash: Option<String> = row.get(16)?;
                Ok(ComparedFindingView {
                    finding_id: row.get(0)?,
                    category,
                    fingerprint_hash: row.get(2)?,
                    occurrence: u64::try_from(occurrence).unwrap_or(u64::MAX),
                    identity_basis,
                    identity_basis_explanation: compared_identity_basis_explanation(identity_basis)
                        .to_owned(),
                    severity: row.get(5)?,
                    rule_id: row.get(6)?,
                    issue_code: row.get(7)?,
                    display_message: row.get(8)?,
                    entity_id: row.get(9)?,
                    field_id: row.get(10)?,
                    relationship_id: row.get(11)?,
                    relationship_role: row.get(12)?,
                    baseline_issue_ids: baseline_issue_id.into_iter().collect(),
                    baseline_issue_hashes: baseline_issue_hash.into_iter().collect(),
                    candidate_issue_ids: candidate_issue_id.into_iter().collect(),
                    candidate_issue_hashes: candidate_issue_hash.into_iter().collect(),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ComparedValidationView {
        schema_version: COMPARED_VALIDATION_SCHEMA.to_owned(),
        comparison_id: row.0,
        baseline_tour_id: comparison.prior.tour_id,
        candidate_tour_id: comparison.current.tour_id,
        baseline_validation_run_id: comparison.prior.validation_run_id,
        candidate_validation_run_id: comparison.current.validation_run_id,
        baseline_result_hash: comparison.prior.validation_result_hash,
        candidate_result_hash: comparison.current.validation_result_hash,
        algorithm_version: row.4,
        closed_count: comparison.counts.closed,
        remaining_count: comparison.counts.remaining,
        new_count: comparison.counts.new,
        input_hash: comparison.input_hash,
        comparison_hash: comparison.canonical_hash,
        actor: comparison.attribution.actor,
        reason: comparison.attribution.reason,
        created_at: row.5,
        findings,
    })
}

/// Replay the complete repeatable source-revision authority chain for the
/// global workspace
/// integrity report. Any semantic mismatch is appended without exposing the
/// engagement-scoped HMAC secret or canonical received values.
pub(crate) fn verify_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<TourIntegritySummary> {
    let mut summary = TourIntegritySummary::default();
    let engagement_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM engagement_records", [], |row| {
            row.get(0)
        })?;
    if engagement_count == 0 {
        return Ok(summary);
    }
    let engagement_id = require_engagement_id(connection)?;
    let head_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_tour_heads WHERE engagement_id = ?1",
        [&engagement_id],
        |row| row.get(0),
    )?;
    summary.heads = usize::try_from(head_count).unwrap_or(usize::MAX);
    let attempt_ids = connection
        .prepare(
            "SELECT tour_attempt_id FROM engagement_tour_attempts
             WHERE engagement_id = ?1 ORDER BY created_at, tour_attempt_id",
        )?
        .query_map([&engagement_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    summary.attempts = attempt_ids.len();
    for attempt_id in &attempt_ids {
        if let Err(error) = load_tour_attempt(connection, attempt_id) {
            issues.push(format!("engagement tour attempt {attempt_id}: {error}"));
        }
    }

    let commands = connection
        .prepare(
            "SELECT attempt_id, tour_attempt_id, command_kind,
                    idempotency_key, request_hash, canonical_request_json,
                    expected_generation, result_generation,
                    result_reference, result_hash
             FROM engagement_tour_command_attempts
             WHERE engagement_id = ?1
             ORDER BY expected_generation, result_generation, attempt_id",
        )?
        .query_map([&engagement_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    summary.commands = commands.len();
    if head_count > 0 && commands.is_empty() {
        issues.push("engagement tour head exists without its immutable command history".to_owned());
    }
    if commands
        .first()
        .is_some_and(|command| command.2 != "START_TOUR")
    {
        issues.push("engagement tour command history does not begin with START_TOUR".to_owned());
    }
    let mut start_anchors = BTreeMap::<String, usize>::new();
    for command in &commands {
        if command.2 == "START_TOUR" {
            let count = start_anchors.entry(command.8.clone()).or_default();
            *count = count.saturating_add(1);
        }
    }
    for attempt_id in &attempt_ids {
        let count = start_anchors.remove(attempt_id).unwrap_or_default();
        if count != 1 {
            issues.push(format!(
                "engagement tour attempt {attempt_id} has {count} START_TOUR command anchors; expected one"
            ));
        }
    }
    for (attempt_id, count) in start_anchors {
        issues.push(format!(
            "START_TOUR command history has {count} anchors for unknown tour attempt {attempt_id}"
        ));
    }
    let mut previous_command: Option<(&str, Option<&str>, u64)> = None;
    for command in &commands {
        let expected_generation = u64::try_from(command.6).unwrap_or(u64::MAX);
        let result_generation = u64::try_from(command.7).unwrap_or(u64::MAX);
        let value: Value = match serde_json::from_str(&command.5) {
            Ok(value) => value,
            Err(error) => {
                issues.push(format!(
                    "engagement tour command {} JSON is invalid: {error}",
                    command.0
                ));
                continue;
            }
        };
        let generation_follows =
            previous_command.is_none_or(|(previous_kind, previous_attempt_id, previous_result)| {
                if expected_generation == previous_result {
                    return true;
                }
                if command.2 != "START_TOUR"
                    || previous_kind != "LAUNCH_VALIDATION"
                    || expected_generation != previous_result.saturating_add(1)
                {
                    return false;
                }
                previous_attempt_id.is_some_and(|attempt_id| {
                    connection
                        .query_row(
                            "SELECT EXISTS(
                                 SELECT 1 FROM engagement_tours
                                 WHERE tour_attempt_id = ?1
                             )",
                            [attempt_id],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
                })
            });
        if canonical_json_string(&value)? != command.5
            || sha256_hex(command.5.as_bytes()) != command.4
            || result_generation != expected_generation.saturating_add(1)
            || !generation_follows
        {
            issues.push(format!(
                "engagement tour command {} canonical/generation chain failed replay",
                command.0
            ));
        }
        previous_command = Some((command.2.as_str(), command.1.as_deref(), result_generation));
        if let Err(error) = verify_tour_command_result(
            connection,
            &command.2,
            command.1.as_deref(),
            &command.3,
            &value,
            expected_generation,
            result_generation,
            &command.8,
            &command.9,
        ) {
            issues.push(format!(
                "engagement tour command {} result replay failed: {error}",
                command.0
            ));
        }
    }

    let namespace_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_comparison_namespaces
         WHERE engagement_id = ?1",
        [&engagement_id],
        |row| row.get(0),
    )?;
    summary.namespaces = usize::try_from(namespace_count).unwrap_or(usize::MAX);
    let namespace = match load_namespace_optional(connection, &engagement_id) {
        Ok(value) => value,
        Err(error) => {
            issues.push(format!("comparison namespace replay failed: {error}"));
            None
        }
    };

    let reuse_ids = connection
        .prepare(
            "SELECT source_reuse_id
             FROM engagement_mapping_reuse_authorities
             WHERE engagement_id = ?1 ORDER BY verified_at, source_reuse_id",
        )?
        .query_map([&engagement_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    summary.reuse_authorities = reuse_ids.len();
    for reuse_id in &reuse_ids {
        if let Err(error) = load_reuse_authority(connection, reuse_id, true) {
            issues.push(format!("mapping reuse authority {reuse_id}: {error}"));
        }
    }

    let tour_ids = connection
        .prepare(
            "SELECT tour_id FROM engagement_tours
             WHERE engagement_id = ?1 ORDER BY tour_number, tour_id",
        )?
        .query_map([&engagement_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut verified_tours = BTreeMap::new();
    let mut previous_tour: Option<RegisteredTourRow> = None;
    for tour_id in &tour_ids {
        match load_registered_tour(connection, tour_id) {
            Ok(tour) => {
                let expected_number = previous_tour.as_ref().map_or(1, |previous| {
                    previous.evidence.tour_number.saturating_add(1)
                });
                let expected_predecessor = previous_tour
                    .as_ref()
                    .map(|previous| previous.evidence.tour_id.as_str());
                if tour.evidence.tour_number != expected_number
                    || tour.evidence.predecessor_tour_id.as_deref() != expected_predecessor
                {
                    issues.push(format!(
                        "registered tour {} is not the next immutable adjacent revision",
                        tour.evidence.tour_id
                    ));
                }
                previous_tour = Some(tour.clone());
                verified_tours.insert(tour_id.clone(), tour);
            }
            Err(error) => issues.push(format!("registered tour {tour_id}: {error}")),
        }
    }
    if u64::try_from(tour_ids.len()).unwrap_or(u64::MAX) > MAXIMUM_TOURS_PER_ENGAGEMENT {
        issues.push("registered tour history exceeds its bounded revision limit".to_owned());
    }

    let finding_set_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_tour_finding_sets",
        [],
        |row| row.get(0),
    )?;
    summary.finding_sets = usize::try_from(finding_set_count).unwrap_or(usize::MAX);
    let member_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_tour_finding_members",
        [],
        |row| row.get(0),
    )?;
    summary.finding_members = usize::try_from(member_count).unwrap_or(usize::MAX);
    if let Some(namespace) = namespace.as_ref() {
        for tour in verified_tours.values() {
            let result = (|| {
                let stored = load_finding_set_for_tour(connection, &tour.evidence.tour_id)?;
                let input = compared_finding_set_input(
                    connection,
                    &tour.evidence.validation_run_id,
                    &tour.evidence.tour_id,
                )?;
                verify_compared_finding_set_replay(&namespace.key, &input, &stored)?;
                load_finding_set_persistence(connection, namespace, &tour.evidence.tour_id, &stored)
                    .map(|_| ())
            })();
            if let Err(error) = result {
                issues.push(format!(
                    "tour {} finding-set replay failed: {error}",
                    tour.evidence.tour_id
                ));
            }
        }
    } else if !tour_ids.is_empty() {
        issues.push("registered tours have no comparison namespace".to_owned());
    }

    let comparison_rows = connection
        .prepare(
            "SELECT comparison_id, baseline_tour_id, revised_tour_id,
                    baseline_finding_set_id, baseline_finding_set_hash,
                    baseline_membership_hash, revised_finding_set_id,
                    revised_finding_set_hash, revised_membership_hash,
                    namespace_id, namespace_secret_hash, algorithm_version,
                    profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    rule_set_hash, validation_engine_version,
                    validation_policy_hash, closed_count, remaining_count,
                    new_count, canonical_comparison_json, comparison_hash,
                    created_at
             FROM engagement_validation_comparisons
             WHERE engagement_id = ?1 ORDER BY created_at, comparison_id",
        )?
        .query_map([&engagement_id], |row| {
            Ok(StoredComparisonIntegrityRow {
                comparison_id: row.get(0)?,
                baseline_tour_id: row.get(1)?,
                revised_tour_id: row.get(2)?,
                baseline_finding_set_id: row.get(3)?,
                baseline_finding_set_hash: row.get(4)?,
                baseline_membership_hash: row.get(5)?,
                revised_finding_set_id: row.get(6)?,
                revised_finding_set_hash: row.get(7)?,
                revised_membership_hash: row.get(8)?,
                namespace_id: row.get(9)?,
                namespace_secret_hash: row.get(10)?,
                algorithm_version: row.get(11)?,
                profile_id: row.get(12)?,
                profile_version: row.get(13)?,
                profile_hash: row.get(14)?,
                mapping_id: row.get(15)?,
                mapping_version: row.get(16)?,
                mapping_hash: row.get(17)?,
                rule_set_hash: row.get(18)?,
                validation_engine_version: row.get(19)?,
                validation_policy_hash: row.get(20)?,
                closed_count: row.get(21)?,
                remaining_count: row.get(22)?,
                new_count: row.get(23)?,
                canonical_comparison_json: row.get(24)?,
                comparison_hash: row.get(25)?,
                created_at: row.get(26)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    summary.comparisons = comparison_rows.len();
    let mut expected_comparison_pairs = tour_ids
        .windows(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect::<Vec<_>>();
    expected_comparison_pairs.sort();
    let mut observed_comparison_pairs = comparison_rows
        .iter()
        .map(|row| (row.baseline_tour_id.clone(), row.revised_tour_id.clone()))
        .collect::<Vec<_>>();
    observed_comparison_pairs.sort();
    if comparison_rows.len() != tour_ids.len().saturating_sub(1)
        || observed_comparison_pairs != expected_comparison_pairs
    {
        issues.push(
            "validation comparison history does not cover every immutable adjacent tour pair"
                .to_owned(),
        );
    }
    let comparison_item_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_validation_comparison_items",
        [],
        |row| row.get(0),
    )?;
    summary.comparison_items = usize::try_from(comparison_item_count).unwrap_or(usize::MAX);
    for row in comparison_rows {
        let result = (|| {
            let value: Value =
                serde_json::from_str(&row.canonical_comparison_json).map_err(|error| {
                    KernelError::IntegrityFailed(format!("comparison JSON is invalid: {error}"))
                })?;
            if canonical_json_string(&value)? != row.canonical_comparison_json {
                return Err(KernelError::IntegrityFailed(
                    "comparison JSON is not canonical".to_owned(),
                ));
            }
            let stored: ComparedValidation = serde_json::from_value(value).map_err(|error| {
                KernelError::IntegrityFailed(format!("comparison contract is invalid: {error}"))
            })?;
            let namespace = namespace.as_ref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "comparison has no independently replayed pseudonym namespace".to_owned(),
                )
            })?;
            let baseline_set = load_finding_set_for_tour(connection, &row.baseline_tour_id)?;
            let revised_set = load_finding_set_for_tour(connection, &row.revised_tour_id)?;
            let baseline_tour = load_registered_tour(connection, &row.baseline_tour_id)?;
            let revised_tour = load_registered_tour(connection, &row.revised_tour_id)?;
            if revised_tour.evidence.predecessor_tour_id.as_deref()
                != Some(baseline_tour.evidence.tour_id.as_str())
                || revised_tour.evidence.tour_number
                    != baseline_tour.evidence.tour_number.saturating_add(1)
            {
                return Err(KernelError::IntegrityFailed(
                    "comparison does not bind adjacent immutable revisions".to_owned(),
                ));
            }
            let baseline_persistence = load_finding_set_persistence(
                connection,
                namespace,
                &row.baseline_tour_id,
                &baseline_set,
            )?;
            let revised_persistence = load_finding_set_persistence(
                connection,
                namespace,
                &row.revised_tour_id,
                &revised_set,
            )?;
            let persisted_counts = ComparedValidationCounts {
                closed: nonnegative_i64(row.closed_count, "comparison closed count")?,
                remaining: nonnegative_i64(row.remaining_count, "comparison remaining count")?,
                new: nonnegative_i64(row.new_count, "comparison new count")?,
            };
            if stored.canonical_hash != row.comparison_hash
                || stored.prior.tour_id != row.baseline_tour_id
                || stored.current.tour_id != row.revised_tour_id
                || row.baseline_finding_set_id != baseline_persistence.finding_set_id
                || row.baseline_finding_set_hash != stored.prior.finding_set_hash
                || row.baseline_membership_hash != baseline_persistence.membership_hash
                || row.revised_finding_set_id != revised_persistence.finding_set_id
                || row.revised_finding_set_hash != stored.current.finding_set_hash
                || row.revised_membership_hash != revised_persistence.membership_hash
                || row.namespace_id != namespace.namespace_id
                || row.namespace_secret_hash != namespace.secret_hash
                || row.algorithm_version != COMPARISON_FINGERPRINT_ALGORITHM
                || row.profile_id != stored.authority.profile_id
                || row.profile_version != stored.authority.profile_version
                || row.profile_hash != stored.authority.profile_hash
                || row.mapping_id != stored.authority.mapping_id
                || row.mapping_version != stored.authority.mapping_version
                || row.mapping_hash != stored.authority.mapping_hash
                || row.rule_set_hash != stored.authority.rule_set_hash
                || row.validation_engine_version != stored.authority.validation_engine_version
                || row.validation_policy_hash != stored.authority.validation_policy_hash
                || persisted_counts != stored.counts
                || row.created_at != revised_tour.evidence.registered_at
            {
                return Err(KernelError::IntegrityFailed(
                    "comparison persisted authority/count/linkage differs from canonical comparison"
                        .to_owned(),
                ));
            }
            let input = ComparedValidationInput {
                schema_version: COMPARED_VALIDATION_INPUT_SCHEMA.to_owned(),
                prior: baseline_set,
                current: revised_set,
                attribution: stored.attribution.clone(),
            };
            verify_compared_validation_replay(&input, &stored)?;
            verify_comparison_items(
                connection,
                &row.comparison_id,
                &stored,
                &baseline_persistence,
                &revised_persistence,
            )
        })();
        if let Err(error) = result {
            issues.push(format!(
                "validation comparison {}: {error}",
                row.comparison_id
            ));
        }
    }

    if let Some(head) = load_tour_head(connection, &engagement_id)? {
        if let Some(terminal_tour) = previous_tour.as_ref()
            && head.active_tour_id.as_deref() != Some(terminal_tour.evidence.tour_id.as_str())
        {
            issues.push(
                "tour head does not reference the terminal immutable registered tour".to_owned(),
            );
        }
        if let Some(active_id) = head.active_tour_id.as_deref() {
            if let Some(active_tour) = verified_tours.get(active_id) {
                let context: (
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    i64,
                ) = connection.query_row(
                    "SELECT current_preflight_id, active_job_id,
                            active_profiling_run_id, selected_profile_hash,
                            active_mapping_hash, active_validation_run_id,
                            validation_stage, generation
                     FROM engagement_active_context WHERE engagement_id = ?1",
                    [&engagement_id],
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
                )?;
                if context.0.as_deref() != Some(active_tour.evidence.preflight_id.as_str())
                    || context.1.as_deref() != Some(active_tour.evidence.job_id.as_str())
                    || context.2.as_deref() != Some(active_tour.evidence.profiling_run_id.as_str())
                    || context.3.as_deref() != Some(active_tour.evidence.profile_hash.as_str())
                    || context.4.as_deref() != Some(active_tour.evidence.mapping_hash.as_str())
                    || context.5.as_deref() != Some(active_tour.evidence.validation_run_id.as_str())
                    || context.6.as_deref() != Some("COMPLETE")
                {
                    issues.push(
                        "active engagement context differs from immutable active tour".to_owned(),
                    );
                }
                if let Some(first_command) = commands.first()
                    && first_command.2 == "START_TOUR"
                {
                    let initial_generation = u64::try_from(first_command.6).ok();
                    let expected_context_generation = initial_generation.and_then(|generation| {
                        generation.checked_add(active_tour.evidence.tour_number.saturating_sub(1))
                    });
                    if expected_context_generation != u64::try_from(context.7).ok() {
                        issues.push(
                            "engagement tour initial START_TOUR generation differs from active-context authority"
                                .to_owned(),
                        );
                    }
                }
            } else {
                issues.push("tour head references an unverified active tour".to_owned());
            }
        }
        if let Some(pending_id) = head.pending_tour_attempt_id.as_deref()
            && !attempt_ids.iter().any(|attempt| attempt == pending_id)
        {
            issues.push("tour head references an unknown pending attempt".to_owned());
        }
        if let Some((last_kind, last_attempt_id, last_generation)) = previous_command {
            let complete_transition = last_kind == "LAUNCH_VALIDATION"
                && last_attempt_id.is_some_and(|attempt_id| {
                    verified_tours
                        .values()
                        .any(|tour| tour.evidence.tour_attempt_id.as_deref() == Some(attempt_id))
                });
            let expected_head = if complete_transition {
                last_generation.saturating_add(1)
            } else {
                last_generation
            };
            if head.generation != expected_head {
                issues.push(format!(
                    "tour head generation {} differs from replayed command/final transition {expected_head}",
                    head.generation
                ));
            }
        }
    } else if !attempt_ids.is_empty() || !tour_ids.is_empty() {
        issues.push("tour evidence exists without a tour head".to_owned());
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn verify_tour_command_result(
    connection: &Connection,
    command_kind: &str,
    tour_attempt_id: Option<&str>,
    idempotency_key: &str,
    value: &Value,
    expected_generation: u64,
    result_generation: u64,
    result_reference: &str,
    result_hash: &str,
) -> Result<()> {
    match command_kind {
        "START_TOUR" => {
            if tour_attempt_id.is_some() {
                return Err(KernelError::IntegrityFailed(
                    "START_TOUR command unexpectedly stores a direct attempt FK".to_owned(),
                ));
            }
            let command: StartRevisedTourCommand = serde_json::from_value(value.clone())?;
            if command.schema_version != START_REVISED_TOUR_COMMAND_SCHEMA
                || command.idempotency_key != idempotency_key
                || command.expected_engagement_generation != expected_generation
            {
                return Err(KernelError::IntegrityFailed(
                    "START_TOUR typed request differs".to_owned(),
                ));
            }
            let attempt = load_tour_attempt(connection, result_reference)?;
            if attempt.evidence.tour_attempt_id != result_reference {
                return Err(KernelError::IntegrityFailed(
                    "START_TOUR result attempt differs".to_owned(),
                ));
            }
            let canonical = canonical_json_string(&serde_json::to_value(&attempt.evidence)?)?;
            if sha256_hex(canonical.as_bytes()) != result_hash {
                return Err(KernelError::IntegrityFailed(
                    "START_TOUR result hash differs from exact attempt".to_owned(),
                ));
            }
        }
        "RESERVE_SOURCE" => {
            let command: ReserveRevisedPreflightCommand = serde_json::from_value(value.clone())?;
            if command.schema_version != RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA
                || command.idempotency_key != idempotency_key
                || command.expected_engagement_generation != expected_generation
                || tour_attempt_id != Some(command.tour_attempt_id.as_str())
            {
                return Err(KernelError::IntegrityFailed(
                    "RESERVE_SOURCE typed request differs".to_owned(),
                ));
            }
            let preflight = load_preflight(connection, result_reference)?;
            if preflight.source_name != command.source_name {
                return Err(KernelError::IntegrityFailed(
                    "RESERVE_SOURCE result preflight differs".to_owned(),
                ));
            }
            let expected_hash = canonical_hash(&json!({
                "schemaVersion": TOUR_COMMAND_RESULT_SCHEMA,
                "commandKind": "RESERVE_SOURCE",
                "tourAttemptId": command.tour_attempt_id,
                "preflightId": result_reference,
                "generation": result_generation,
            }))?;
            if expected_hash != result_hash {
                return Err(KernelError::IntegrityFailed(
                    "RESERVE_SOURCE result hash differs".to_owned(),
                ));
            }
        }
        "ACCEPT_SOURCE" => {
            let command: AcceptRevisedPreflightCommand = serde_json::from_value(value.clone())?;
            if command.schema_version != ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA
                || command.idempotency_key != idempotency_key
                || command.expected_engagement_generation != expected_generation
                || tour_attempt_id != Some(command.tour_attempt_id.as_str())
            {
                return Err(KernelError::IntegrityFailed(
                    "ACCEPT_SOURCE typed request differs".to_owned(),
                ));
            }
            if result_reference == "MAPPING_REQUIRED" {
                let attempt = load_tour_attempt(connection, &command.tour_attempt_id)?;
                let baseline =
                    load_registered_tour(connection, &attempt.evidence.predecessor_tour_id)?;
                let mapping = load_stored_mapping(
                    connection,
                    &baseline.evidence.mapping_id,
                    &baseline.evidence.mapping_version,
                )?;
                if mapping.mapping_hash != baseline.evidence.mapping_hash {
                    return Err(KernelError::IntegrityFailed(
                        "MAPPING_REQUIRED predecessor mapping differs from immutable revision"
                            .to_owned(),
                    ));
                }
                let candidate_job_id: Option<String> = connection
                    .query_row(
                        "SELECT job_id FROM engagement_acceptance_attempts
                         WHERE engagement_id = ?1 AND preflight_id = ?2
                           AND idempotency_key = ?3
                           AND accepted_source_hash = ?4
                           AND accepted_manifest_hash = ?5",
                        params![
                            attempt.evidence.engagement_id,
                            command.preflight_id,
                            command.idempotency_key,
                            command.expected_source_hash,
                            command.expected_manifest_hash,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                let candidate_job_id = candidate_job_id.ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "MAPPING_REQUIRED result has no exact accepted candidate job".to_owned(),
                    )
                })?;
                let baseline_structure =
                    source_structure(connection, &baseline.evidence.job_id, &mapping.document)?;
                let candidate_structure =
                    source_structure(connection, &candidate_job_id, &mapping.document)?;
                if baseline_structure == candidate_structure {
                    return Err(KernelError::IntegrityFailed(
                        "MAPPING_REQUIRED result names structurally compatible evidence".to_owned(),
                    ));
                }
                let baseline_hash = canonical_hash(&serde_json::to_value(&baseline_structure)?)?;
                let candidate_hash = canonical_hash(&serde_json::to_value(&candidate_structure)?)?;
                let expected_hash = mapping_required_result_hash(
                    &command.tour_attempt_id,
                    &baseline_hash,
                    &candidate_hash,
                )?;
                if expected_hash != result_hash {
                    return Err(KernelError::IntegrityFailed(
                        "MAPPING_REQUIRED result hash differs from exact structures".to_owned(),
                    ));
                }
            } else {
                let reuse = load_reuse_authority(connection, result_reference, true)?;
                if reuse.source_reuse_hash != result_hash
                    || reuse.evidence.tour_attempt_id != command.tour_attempt_id
                    || reuse.evidence.candidate.preflight_id != command.preflight_id
                    || reuse.evidence.candidate.source_hash != command.expected_source_hash
                    || reuse.evidence.candidate.manifest_hash != command.expected_manifest_hash
                {
                    return Err(KernelError::IntegrityFailed(
                        "ACCEPT_SOURCE result differs from exact reuse authority".to_owned(),
                    ));
                }
            }
        }
        "LAUNCH_VALIDATION" => {
            let command: LaunchRevisedValidationCommand = serde_json::from_value(value.clone())?;
            if command.schema_version != LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA
                || command.idempotency_key != idempotency_key
                || command.expected_engagement_generation != expected_generation
                || tour_attempt_id != Some(command.tour_attempt_id.as_str())
            {
                return Err(KernelError::IntegrityFailed(
                    "LAUNCH_VALIDATION typed request differs".to_owned(),
                ));
            }
            let reuse =
                load_reuse_authority(connection, &command.expected_reuse_authority_id, true)?;
            if reuse.source_reuse_hash != command.expected_reuse_authority_hash {
                return Err(KernelError::IntegrityFailed(
                    "LAUNCH_VALIDATION reuse authority differs".to_owned(),
                ));
            }
            let expected_hash = canonical_hash(&json!({
                "schemaVersion": TOUR_COMMAND_RESULT_SCHEMA,
                "commandKind": "LAUNCH_VALIDATION",
                "tourAttemptId": command.tour_attempt_id,
                "runId": result_reference,
                "sourceReuseId": reuse.evidence.source_reuse_id,
                "sourceReuseHash": reuse.source_reuse_hash,
                "generation": result_generation,
            }))?;
            if expected_hash != result_hash {
                return Err(KernelError::IntegrityFailed(
                    "LAUNCH_VALIDATION plan hash differs".to_owned(),
                ));
            }
        }
        _ => {
            return Err(KernelError::IntegrityFailed(format!(
                "unknown tour command kind {command_kind:?}"
            )));
        }
    }
    Ok(())
}

fn mapping_required_result_hash(
    tour_attempt_id: &str,
    baseline_structure_hash: &str,
    candidate_structure_hash: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "schemaVersion": TOUR_COMMAND_RESULT_SCHEMA,
        "commandKind": "ACCEPT_SOURCE",
        "tourAttemptId": tour_attempt_id,
        "result": "MAPPING_REQUIRED",
        "baselineStructureHash": baseline_structure_hash,
        "candidateStructureHash": candidate_structure_hash,
    }))
}

fn verify_comparison_items(
    connection: &Connection,
    comparison_id: &str,
    comparison: &ComparedValidation,
    baseline: &FindingSetPersistence,
    revised: &FindingSetPersistence,
) -> Result<()> {
    let mut expected = BTreeMap::<(String, String, u64), (String, String)>::new();
    let mut add_expected = |category: &str,
                            fingerprint: &str,
                            category_index: u64,
                            baseline_index: Option<u64>,
                            revised_index: Option<u64>,
                            explanation_code: &str|
     -> Result<()> {
        let baseline_member = baseline_index
            .map(|index| {
                baseline
                    .member_hashes
                    .get(&(fingerprint.to_owned(), index))
                    .map(|hash| (baseline.finding_set_id.as_str(), index, hash.as_str()))
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(format!(
                            "canonical comparison references absent baseline member {fingerprint}/{index}"
                        ))
                    })
            })
            .transpose()?;
        let revised_member = revised_index
            .map(|index| {
                revised
                    .member_hashes
                    .get(&(fingerprint.to_owned(), index))
                    .map(|hash| (revised.finding_set_id.as_str(), index, hash.as_str()))
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(format!(
                            "canonical comparison references absent revised member {fingerprint}/{index}"
                        ))
                    })
            })
            .transpose()?;
        let value = comparison_item_value(
            comparison_id,
            category,
            fingerprint,
            category_index,
            baseline_member,
            revised_member,
            explanation_code,
        );
        let canonical = canonical_json_string(&value)?;
        let hash = sha256_hex(canonical.as_bytes());
        if expected
            .insert(
                (category.to_owned(), fingerprint.to_owned(), category_index),
                (canonical, hash),
            )
            .is_some()
        {
            return Err(KernelError::IntegrityFailed(
                "canonical comparison repeats an item identity".to_owned(),
            ));
        }
        Ok(())
    };
    for delta in &comparison.findings {
        for index in 1..=delta.remaining_count {
            add_expected(
                "REMAINING",
                &delta.fingerprint,
                index,
                Some(index),
                Some(index),
                "MULTISET_REMAINING",
            )?;
        }
        for index in 1..=delta.closed_count {
            add_expected(
                "CLOSED",
                &delta.fingerprint,
                index,
                Some(delta.remaining_count.checked_add(index).ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "canonical closed occurrence index overflow".to_owned(),
                    )
                })?),
                None,
                "MULTISET_CLOSED",
            )?;
        }
        for index in 1..=delta.new_count {
            add_expected(
                "NEW",
                &delta.fingerprint,
                index,
                None,
                Some(delta.remaining_count.checked_add(index).ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "canonical new occurrence index overflow".to_owned(),
                    )
                })?),
                "MULTISET_NEW",
            )?;
        }
    }

    let mut statement = connection.prepare(
        "SELECT category, finding_fingerprint, category_occurrence_index,
                baseline_finding_set_id, baseline_occurrence_index,
                baseline_member_hash, revised_finding_set_id,
                revised_occurrence_index, revised_member_hash,
                explanation_code, canonical_item_json, item_hash
         FROM engagement_validation_comparison_items
         WHERE comparison_id = ?1
         ORDER BY category, finding_fingerprint, category_occurrence_index",
    )?;
    let mut rows = statement.query([comparison_id])?;
    while let Some(row) = rows.next()? {
        let category: String = row.get(0)?;
        let fingerprint: String = row.get(1)?;
        let occurrence_index = nonnegative_i64(row.get(2)?, "comparison occurrence index")?;
        let baseline_set: Option<String> = row.get(3)?;
        let baseline_occurrence: Option<i64> = row.get(4)?;
        let baseline_hash: Option<String> = row.get(5)?;
        let revised_set: Option<String> = row.get(6)?;
        let revised_occurrence: Option<i64> = row.get(7)?;
        let revised_hash: Option<String> = row.get(8)?;
        let explanation_code: String = row.get(9)?;
        let canonical_item_json: String = row.get(10)?;
        let item_hash: String = row.get(11)?;
        let baseline_member = match (
            baseline_set.as_deref(),
            baseline_occurrence,
            baseline_hash.as_deref(),
        ) {
            (Some(set), Some(index), Some(hash)) => Some((
                set,
                nonnegative_i64(index, "baseline comparison occurrence index")?,
                hash,
            )),
            (None, None, None) => None,
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "comparison item has incomplete baseline membership".to_owned(),
                ));
            }
        };
        let revised_member = match (
            revised_set.as_deref(),
            revised_occurrence,
            revised_hash.as_deref(),
        ) {
            (Some(set), Some(index), Some(hash)) => Some((
                set,
                nonnegative_i64(index, "revised comparison occurrence index")?,
                hash,
            )),
            (None, None, None) => None,
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "comparison item has incomplete revised membership".to_owned(),
                ));
            }
        };
        let item_value = comparison_item_value(
            comparison_id,
            &category,
            &fingerprint,
            occurrence_index,
            baseline_member,
            revised_member,
            &explanation_code,
        );
        let actual_json = canonical_json_string(&item_value)?;
        let Some((expected_json, expected_hash)) =
            expected.remove(&(category.clone(), fingerprint.clone(), occurrence_index))
        else {
            return Err(KernelError::IntegrityFailed(format!(
                "comparison item {category}/{fingerprint}/{occurrence_index} is absent from canonical comparison"
            )));
        };
        if actual_json != canonical_item_json
            || sha256_hex(actual_json.as_bytes()) != item_hash
            || actual_json != expected_json
            || item_hash != expected_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "comparison item {category}/{fingerprint}/{occurrence_index} differs from canonical comparison"
            )));
        }
    }
    if !expected.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "comparison item storage omits canonical comparison occurrences".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_revisions_accept_both_completed_validation_phases_only() {
        assert!(phase_has_completed_validation(
            &EngagementPhase::ValidationComplete
        ));
        assert!(phase_has_completed_validation(
            &EngagementPhase::FirstIssuesReady
        ));
        assert!(!phase_has_completed_validation(
            &EngagementPhase::Validating
        ));
        assert!(!phase_has_completed_validation(
            &EngagementPhase::ReadyToValidate
        ));
    }
}

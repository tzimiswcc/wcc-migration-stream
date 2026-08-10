#![allow(clippy::missing_errors_doc)]

//! Durable one-workspace engagement setup.
//!
//! The service accepts browser-safe identifiers and bounded declarative
//! commands only. Filesystem paths, intake/profiling/validation policy, schema
//! paths, and reviewed object-pack documents are fixed by the server process.
//! Registering a reviewed profile never approves it; local mapping/validation
//! authority is granted only by [`EngagementService::approve_profile`].

mod remediation;
mod tours;

pub use remediation::{
    CREATE_REMEDIATION_PACK_COMMAND_SCHEMA, CreateRemediationPackCommand,
    CreateRemediationPackResult, LOCAL_REMEDIATION_DATA_EXPORT_ACKNOWLEDGEMENT,
    LOCAL_REMEDIATION_OFFENDING_VALUES_EXPORT_ACKNOWLEDGEMENT, RemediationKeySelection,
    RemediationPackHistoryView, RemediationPackOptionsView, RemediationPackView,
};
#[allow(unused_imports)]
pub(crate) use remediation::{
    verify_exact_mapping_authority, verify_integrity as verify_remediation_integrity,
};

pub use tours::{
    ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA, AcceptRevisedPreflightCommand,
    AcceptRevisedPreflightResult, COMPARED_VALIDATION_SCHEMA, ComparedFindingCategory,
    ComparedFindingView, ComparedValidationView, LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA,
    LaunchRevisedValidationCommand, LaunchRevisedValidationResult, MappingReuseAuthorityView,
    RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA, REVISED_SOURCE_ACKNOWLEDGEMENT,
    ReserveRevisedPreflightCommand, RevisedTourAttemptState, RevisedTourAttemptView,
    START_REVISED_TOUR_COMMAND_SCHEMA, StartRevisedTourCommand, TourChainView, TourSummaryView,
};
pub(crate) use tours::{MAPPING_SOURCE_REUSE_SCHEMA, verify_integrity as verify_tour_integrity};

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json, sha256_hex},
    destination::{DestinationOrigin, DestinationService},
    intake::{ArchiveManifest, ingest_package, inspect_archive},
    journey::{
        DownstreamJourneyAuthority, JourneyBinding, JourneyDerivation, JourneySurface, JourneyView,
        derive_journey,
    },
    mapping::{
        MappingBinding, MappingDocument, MappingKind, MappingProfileIdentity,
        MappingRelationshipBinding, MappingSourceIdentity, MappingValidationReport, ReferenceTable,
        SourceEntityAssignment,
    },
    object_workspace::{
        EstablishObjectWorkspaceCommand, ObjectWorkspaceRecord, ObjectWorkspaceService,
        ObjectWorkspaceStatus, bind_workspace_engagement, verify_workspace_engagement_binding,
    },
    policy::IntakePolicy,
    profile_registry::{
        ProfileAuthority, ProfileLifecycleStatus, ProfileVersionView, RegistryLimits,
        approve_profile, inspect_profile, register_profile_document,
    },
    profiling::{ProfilingPolicy, run_profile},
    read_model::{MappingCoveragePage, PageRequest, ReadService},
    state::{
        DATABASE_FILENAME, JobState, RecoveryReport, WorkspaceLock, WorkspaceWriter, now_rfc3339,
        open_read_only, recover_workspace, retain_workspace_root, verify_retained_workspace_root,
        workspace_status,
    },
    validation::{
        ValidationPolicy, ValidationReport, ValidationStatus, run_validation_with_reserved_writer,
        validation_status,
    },
};

pub const ENGAGEMENT_RECORD_SCHEMA: &str = "migration-stream/engagement/record/v1";
pub const ENGAGEMENT_STATE_SCHEMA: &str = "migration-stream/engagement/state/v1";
pub const ENGAGEMENT_CONTEXT_SCHEMA: &str = "migration-stream/engagement/context/v1";
pub const ENGAGEMENT_DETAIL_SCHEMA: &str = "migration-stream/engagement/detail/v1";
pub const OBJECT_PACK_LIST_SCHEMA: &str = "migration-stream/engagement/object-packs/v1";
pub const PREFLIGHT_SCHEMA: &str = "migration-stream/engagement/source-preflight/v1";
pub const ACCEPT_RESULT_SCHEMA: &str = "migration-stream/engagement/source-acceptance-result/v1";
pub const SOURCE_PROFILE_RULES_SCHEMA: &str = "migration-stream/engagement/source-profile-rules/v1";
pub const MAPPING_WORKSPACE_SCHEMA: &str = "migration-stream/engagement/mapping-workspace/v1";
pub const MAPPING_CONTRACT_COVERAGE_SCHEMA: &str =
    "migration-stream/engagement/mapping-contract-coverage/v1";
pub const MAPPING_DRAFT_SCHEMA: &str = "migration-stream/engagement/mapping-draft/v1";
pub const SUBMITTED_MAPPING_SCHEMA: &str = "migration-stream/engagement/submitted-mapping/v1";
pub const MAPPING_APPROVAL_SCHEMA: &str = "migration-stream/engagement/mapping-approval/v1";
pub const PREPARED_MAPPING_SCHEMA: &str = "migration-stream/engagement/prepared-mapping/v1";
pub const VALIDATED_MAPPING_SCHEMA: &str = "migration-stream/engagement/validated-mapping/v1";
pub const VALIDATION_RESULT_SCHEMA: &str = "migration-stream/engagement/validation-result/v1";
const VALIDATION_ATTEMPT_AUTHORITY_SCHEMA: &str =
    "migration-stream/engagement/validation-attempt-authority/v1";
pub const RECOVERY_SCHEMA: &str = "migration-stream/engagement/recovery/v1";
pub const CREATE_ENGAGEMENT_COMMAND_SCHEMA: &str = "migration-stream/engagement/write/create/v1";
pub const RESERVE_PREFLIGHT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/reserve-source-preflight/v1";
pub const ACCEPT_SOURCE_COMMAND_SCHEMA: &str = "migration-stream/engagement/write/accept-source/v1";
pub const SELECT_OBJECT_PACK_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/select-object-pack/v1";
pub const APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/approve-object-pack-profile/v1";
pub const PREPARE_MAPPING_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/assisted-mapping-command/v1";
pub const SAVE_MAPPING_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/save-mapping-draft/v1";
pub const SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/submit-mapping-draft/v1";
pub const APPROVE_MAPPING_VERSION_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/approve-mapping-version/v1";
pub const LAUNCH_VALIDATION_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/launch-validation/v1";
pub const SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT: &str = "migration-stream/source-acceptance/v1";
pub const LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-mapping-validation-authority/v1";
pub const ASSISTED_MAPPING_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1";
pub const OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT: &str =
    "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1";
pub const OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-mapping-version-approval/v1";
pub const SYNTHETIC_REVIEWED_OBJECT_PACK_ID: &str = "synthetic.customer.readiness";
pub const SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID: &str = "synthetic.sales-document.readiness";

const MAXIMUM_COMMAND_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_IDENTIFIER_BYTES: usize = 256;
const MAXIMUM_DISPLAY_NAME_BYTES: usize = 256;
const MAXIMUM_REASON_BYTES: usize = 4_096;
const MAXIMUM_REFERENCE_TABLES: usize = 32;
const MAXIMUM_BINDINGS: usize = 512;
const MAXIMUM_SOURCE_FILES: usize = 32;
const MAXIMUM_RELATIONSHIPS: usize = 256;
const MAXIMUM_REFERENCE_KEY_BYTES: usize = 1_024;
const MAXIMUM_REFERENCE_VALUE_BYTES: usize = 4_096;
const MAXIMUM_MAPPING_DRAFT_REVISIONS: u64 = 1_000;
const MAXIMUM_MAPPING_APPROVAL_BYTES: usize = 64 * 1_024;
const MAXIMUM_MAPPING_AUTHORITY_ROWS: i64 = 100_000;
const MAXIMUM_MAPPING_AUTHORITY_METADATA_BYTES: i64 = 16 * 1_024;
const PREFLIGHT_FILE_INTENT_SCHEMA: &str = "migration-stream/engagement/preflight-file-intent/v1";
const LEGACY_PREFLIGHT_PUBLISH_INTENT_SCHEMA: &str =
    "migration-stream/engagement/preflight-publish-intent/v1";
const PREFLIGHT_PUBLISH_INTENT_SCHEMA: &str =
    "migration-stream/engagement/preflight-publish-intent/v2";
const PREFLIGHT_EVENTS_NO_UPDATE_TRIGGER_SQL: &str = concat!(
    "CREATE TRIGGER engagement_preflight_events_no_update ",
    "BEFORE UPDATE ON engagement_preflight_events ",
    "BEGIN SELECT RAISE(ABORT, 'engagement preflight history is immutable'); END"
);
const PREFLIGHT_EVENTS_NO_DELETE_TRIGGER_SQL: &str = concat!(
    "CREATE TRIGGER engagement_preflight_events_no_delete ",
    "BEFORE DELETE ON engagement_preflight_events ",
    "BEGIN SELECT RAISE(ABORT, 'engagement preflight history is immutable'); END"
);

/// How one reviewed pack permits a mapping to be governed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MappingMode {
    #[default]
    ControlledSuggestions,
    OperatorGoverned,
}

/// One server-controlled mapping suggestion carried by the reviewed proof pack.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectPackMappingSuggestion {
    pub suggestion_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_entity_id: Option<String>,
    pub target_field_id: String,
    pub source_file: Option<String>,
    pub source_column: Option<String>,
    pub source_column_ordinal: Option<usize>,
    pub kind: MappingKind,
    pub constant: Option<String>,
    pub lookup_table_id: Option<String>,
}

/// The only object-pack shape exposed by setup mode.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlledObjectPack {
    pub object_pack_id: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub scope: String,
    pub root_entity_id: String,
    pub mapping_mode: MappingMode,
    pub limitations: Vec<String>,
    pub mapping_suggestions: Vec<ObjectPackMappingSuggestion>,
    pub reference_tables: Vec<ReferenceTable>,
    #[serde(skip)]
    pub profile_document: Value,
}

/// Server-owned dependencies. No request may replace these paths or policies.
#[derive(Clone, Debug)]
pub struct EngagementServiceConfig {
    pub workspace: PathBuf,
    pub intake_policy: IntakePolicy,
    pub profiling_policy: ProfilingPolicy,
    pub profile_schema_path: PathBuf,
    pub mapping_schema_path: PathBuf,
    pub validation_policy: ValidationPolicy,
    pub object_packs: Vec<ControlledObjectPack>,
}

/// One configured, one-workspace engagement service.
#[derive(Clone, Debug)]
pub struct EngagementService {
    workspace: PathBuf,
    workspace_handle: Arc<File>,
    object_workspace: ObjectWorkspaceService,
    destination: DestinationService,
    intake_policy: IntakePolicy,
    safe_profiling_policy: ProfilingPolicy,
    profile_schema_path: PathBuf,
    mapping_schema_path: PathBuf,
    validation_policy: ValidationPolicy,
    object_packs: Vec<ControlledObjectPack>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOrReopenCommand {
    pub schema_version: String,
    pub display_name: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngagementRecord {
    pub schema_version: String,
    pub engagement_id: String,
    pub display_name: String,
    pub created_at: String,
    pub reopened: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EngagementPhase {
    AwaitingSource,
    PreflightReceiving,
    PreflightReady,
    Ingesting,
    Profiled,
    AwaitingObjectPack,
    AwaitingProfileApproval,
    AwaitingMapping,
    MappingPrepared,
    ReadyToValidate,
    Validating,
    ValidationComplete,
    FirstIssuesReady,
    Failed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveEngagementIds {
    pub preflight_id: Option<String>,
    pub job_id: Option<String>,
    pub profiling_run_id: Option<String>,
    pub object_pack_id: Option<String>,
    pub destination_origin: Option<String>,
    pub destination_authority_id: Option<String>,
    pub destination_origin_hash: Option<String>,
    pub profile_id: Option<String>,
    pub profile_version: Option<String>,
    pub profile_hash: Option<String>,
    pub mapping_id: Option<String>,
    pub mapping_version: Option<String>,
    pub mapping_hash: Option<String>,
    pub validation_run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngagementState {
    pub schema_version: String,
    pub engagement_id: String,
    pub display_name: String,
    pub phase: EngagementPhase,
    pub generation: u64,
    pub active: ActiveEngagementIds,
    pub profile_stage: String,
    pub mapping_stage: String,
    pub validation_stage: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngagementDetail {
    pub schema_version: String,
    pub state: EngagementState,
    pub preflights: Vec<SourcePreflightView>,
    pub available_object_pack_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct EngagementCapabilities {
    pub engagement_setup: bool,
    pub source_intake: bool,
    pub textual_profiling: bool,
    pub reviewed_object_pack_selection: bool,
    pub explicit_profile_approval: bool,
    pub mapping_preparation: bool,
    pub validation_launch: bool,
    pub governed_issue_decisions: bool,
    pub migration_result_approval: bool,
    pub target_write: bool,
    pub publication: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngagementContext {
    pub schema_version: String,
    pub workspace_label: String,
    pub engagement: Option<EngagementState>,
    pub supported_contracts: Vec<String>,
    pub capabilities: EngagementCapabilities,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectPackList {
    pub schema_version: String,
    pub items: Vec<ControlledObjectPack>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReservePreflightCommand {
    pub schema_version: String,
    pub source_name: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourcePreflightState {
    Receiving,
    Ready,
    Rejected,
    Interrupted,
}

impl SourcePreflightState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "RECEIVING" => Ok(Self::Receiving),
            "READY" => Ok(Self::Ready),
            "REJECTED" => Ok(Self::Rejected),
            "INTERRUPTED" => Ok(Self::Interrupted),
            other => Err(KernelError::IntegrityFailed(format!(
                "unknown engagement preflight state {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourcePreflightView {
    pub schema_version: String,
    pub preflight_id: String,
    pub state: SourcePreflightState,
    pub source_name: String,
    pub source_hash: Option<String>,
    pub source_size: Option<u64>,
    pub policy_hash: String,
    pub manifest_hash: Option<String>,
    pub manifest: Option<ArchiveManifest>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreflightReservation {
    pub preflight: SourcePreflightView,
    #[serde(skip_serializing, skip_deserializing)]
    pub controlled_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletePreflightCommand {
    pub preflight_id: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreflightTerminalCommand {
    pub preflight_id: String,
    pub error_code: String,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptPreflightCommand {
    pub schema_version: String,
    pub preflight_id: String,
    pub expected_source_hash: String,
    pub expected_manifest_hash: String,
    pub idempotency_key: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptPreflightResult {
    pub schema_version: String,
    pub engagement_id: String,
    pub preflight_id: String,
    pub job_id: String,
    pub profiling_run_id: String,
    pub source_hash: String,
    pub profiling_result_hash: String,
    pub reused_profile: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectObjectPackCommand {
    pub schema_version: String,
    pub object_pack_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedObjectPackResult {
    pub object_pack: ControlledObjectPack,
    pub profile: ProfileVersionView,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveProfileCommand {
    pub schema_version: String,
    pub actor: String,
    pub reason: String,
    pub authority_acknowledgement: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceColumnView {
    pub source_file: String,
    pub source_column: String,
    pub source_column_ordinal: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileFieldView {
    pub entity_id: String,
    pub field_id: String,
    pub label: String,
    pub logical_type: String,
    pub required: bool,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRuleView {
    pub rule_id: String,
    pub severity: String,
    pub kind: String,
    pub target: String,
    pub message: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceProfileRulesView {
    pub schema_version: String,
    pub job_id: String,
    pub profiling_run_id: String,
    pub stored_sample_values: bool,
    pub source_columns: Vec<SourceColumnView>,
    pub profile_fields: Vec<ProfileFieldView>,
    pub rules: Vec<ProfileRuleView>,
}

/// One canonical entity exposed to the operator-governed mapping workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingWorkspaceEntityView {
    pub entity_id: String,
    pub root: bool,
    pub key: Vec<String>,
    pub fields: Vec<ProfileFieldView>,
}

/// One accepted source file and its header-only, sample-free column identities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingWorkspaceSourceFileView {
    pub source_file: String,
    pub columns: Vec<SourceColumnView>,
}

/// One profile-authoritative relationship available for explicit confirmation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingWorkspaceRelationshipView {
    pub relationship_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    pub cardinality: String,
    pub key_pairs: Vec<crate::mapping::RelationshipFieldPair>,
}

/// One exact entity-scoped reference-table obligation declared by profile rules.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingReferenceTableObligationView {
    pub entity_id: String,
    pub table_id: String,
    pub table_version: String,
    pub rule_ids: Vec<String>,
    pub target_fields: Vec<String>,
}

/// Runtime bounds shared by the server validator and the narrow mapping editor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingWorkspaceLimitsView {
    pub maximum_source_files: usize,
    pub maximum_bindings: usize,
    pub maximum_relationships: usize,
    pub maximum_reference_tables: usize,
    pub maximum_reference_entries: usize,
    pub maximum_reference_bytes: usize,
    pub maximum_reference_key_bytes: usize,
    pub maximum_reference_value_bytes: usize,
}

/// Complete bounded operator-authored mapping draft bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingDraftSnapshot {
    pub source_entity_assignments: Vec<SourceEntityAssignment>,
    pub bindings: Vec<MappingBinding>,
    pub relationship_bindings: Vec<MappingRelationshipBinding>,
    pub reference_tables: Vec<ReferenceTable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// Durable head of an append-only mapping-draft revision chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingDraftView {
    pub schema_version: String,
    pub draft_id: String,
    pub draft_generation: u64,
    pub draft_hash: String,
    pub state: String,
    pub snapshot: MappingDraftSnapshot,
    pub submitted_mapping_id: Option<String>,
    pub submitted_mapping_version: Option<String>,
    pub submitted_mapping_hash: Option<String>,
    pub updated_at: String,
}

/// Value-free source-revision scope for a mapping draft created after a
/// structural compatibility failure. The prior active mapping remains
/// authoritative; this scope permits durable draft work only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingRevisionDraftScopeView {
    pub tour_attempt_id: String,
    pub tour_number: u64,
    pub baseline_tour_id: String,
    pub profiling_run_id: String,
    pub submission_available: bool,
    pub unavailable_reason: String,
}

/// Entity-qualified mapping workspace bound to exact active authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingWorkspaceView {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub job_id: String,
    pub source_hash: String,
    pub profiling_run_id: String,
    pub revision_draft_scope: Option<MappingRevisionDraftScopeView>,
    pub destination_origin: String,
    pub object_pack_id: String,
    pub object_pack_version: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_mode: MappingMode,
    pub source_files: Vec<MappingWorkspaceSourceFileView>,
    pub entities: Vec<MappingWorkspaceEntityView>,
    pub relationships: Vec<MappingWorkspaceRelationshipView>,
    pub allowed_operations: Vec<MappingKind>,
    pub controlled_reference_tables: Vec<ReferenceTable>,
    pub reference_table_obligations: Vec<MappingReferenceTableObligationView>,
    pub limits: MappingWorkspaceLimitsView,
    pub draft: Option<MappingDraftView>,
}

/// Honest status used by every part of value-free contract coverage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MappingContractCoverageStatus {
    Complete,
    Unresolved,
    Warning,
    Blocker,
}

/// Exact authorities that the derived mapping-contract coverage read replays.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractCoverageAuthority {
    pub workspace_id: String,
    pub workspace_descriptor_hash: String,
    pub engagement_id: String,
    pub engagement_generation: u64,
    pub job_id: String,
    pub source_hash: String,
    pub profiling_run_id: String,
    pub destination_origin: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub profile_approval_id: String,
    pub profile_approval_hash: String,
    pub draft_id: Option<String>,
    pub draft_generation: Option<u64>,
    pub draft_hash: Option<String>,
    pub draft_state: Option<String>,
    pub mapping_id: Option<String>,
    pub mapping_version: Option<String>,
    pub mapping_hash: Option<String>,
    pub mapping_approval_id: Option<String>,
    pub mapping_approval_hash: Option<String>,
}

/// One active destination slot and its value-free binding/rule status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractFieldCoverage {
    pub entity_id: String,
    pub field_id: String,
    pub binding_id: Option<String>,
    pub operation: Option<MappingKind>,
    pub source_file: Option<String>,
    pub source_column: Option<String>,
    pub source_column_ordinal: Option<usize>,
    pub fixed_value_configured: bool,
    pub lookup_table_id: Option<String>,
    pub lookup_table_version: Option<String>,
    pub rule_ids: Vec<String>,
    pub rule_kinds: Vec<String>,
    pub status: MappingContractCoverageStatus,
    pub reasons: Vec<String>,
}

/// One accepted header identity that is or is not used by the durable contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractSourceCoverage {
    pub source_file: String,
    pub source_column: String,
    pub source_column_ordinal: usize,
    pub status: MappingContractCoverageStatus,
    pub reason: String,
}

/// One profile-declared rule obligation without comparator or domain values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractRuleCoverage {
    pub rule_id: String,
    pub kind: String,
    pub entity_id: String,
    pub field_id: String,
    pub reference_table_id: Option<String>,
    pub reference_table_version: Option<String>,
    pub status: MappingContractCoverageStatus,
    pub reasons: Vec<String>,
}

/// One exact embedded table identity; entries remain private to the editor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractTableCoverage {
    pub entity_id: String,
    pub table_id: String,
    pub table_version: String,
    pub entry_count: usize,
    pub roles: Vec<String>,
    pub status: MappingContractCoverageStatus,
    pub reasons: Vec<String>,
}

/// One explicit relationship/key obligation, or none for a root-only profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractRelationshipCoverage {
    pub relationship_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    pub cardinality: String,
    pub key_pairs: Vec<crate::mapping::RelationshipFieldPair>,
    pub status: MappingContractCoverageStatus,
    pub reasons: Vec<String>,
}

/// Counted statuses for the complete derived contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractCoverageSummary {
    pub complete: usize,
    pub unresolved: usize,
    pub warnings: usize,
    pub blockers: usize,
}

/// Value-free, server-derived view of the exact draft-or-mapping contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingContractCoverageView {
    pub schema_version: String,
    pub authority: MappingContractCoverageAuthority,
    pub overall_status: MappingContractCoverageStatus,
    pub summary: MappingContractCoverageSummary,
    pub fields: Vec<MappingContractFieldCoverage>,
    pub source_columns: Vec<MappingContractSourceCoverage>,
    pub rules: Vec<MappingContractRuleCoverage>,
    pub tables: Vec<MappingContractTableCoverage>,
    pub relationships: Vec<MappingContractRelationshipCoverage>,
    pub coverage_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMappingDraftCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub expected_draft_id: Option<String>,
    pub expected_draft_generation: Option<u64>,
    pub expected_draft_hash: Option<String>,
    pub snapshot: MappingDraftSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveMappingDraftResult {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub draft: MappingDraftView,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitMappingDraftCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub draft_id: String,
    pub expected_draft_generation: u64,
    pub expected_draft_hash: String,
    pub mapping_id: String,
    pub version: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitMappingDraftResult {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub draft: MappingDraftView,
    pub preparation_id: String,
    pub mapping_hash: String,
    pub coverage: crate::mapping::MappingCoverage,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveMappingVersionCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub draft_id: String,
    pub expected_draft_generation: u64,
    pub expected_draft_hash: String,
    pub expected_mapping_id: String,
    pub expected_mapping_version: String,
    pub expected_mapping_hash: String,
    pub actor: String,
    pub reason: String,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingApprovalResult {
    pub schema_version: String,
    pub engagement_generation: u64,
    pub approval_id: String,
    pub approval_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub actor: String,
    pub reason: String,
    pub approved_at: String,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareMappingCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub mapping_id: String,
    pub version: String,
    pub selected_suggestion_ids: Vec<String>,
    pub author: Option<String>,
    pub reviewer: Option<String>,
    pub acknowledgement: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedMappingResult {
    pub schema_version: String,
    pub preparation_id: String,
    pub mapping_hash: String,
    pub document: MappingDocument,
    pub reused_immutable_preparation: bool,
    pub validation: Option<MappingValidationReport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateMappingCommand {
    pub preparation_id: String,
    pub expected_mapping_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidatedMappingResult {
    pub schema_version: String,
    pub preparation_id: String,
    pub report: MappingValidationReport,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchValidationCommand {
    pub schema_version: String,
    pub idempotency_key: String,
    pub expected_engagement_generation: u64,
    pub expected_job_id: String,
    pub expected_mapping_id: String,
    pub expected_mapping_version: String,
    pub expected_mapping_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchValidationResult {
    pub schema_version: String,
    pub status: ValidationStatus,
    pub report: Option<ValidationReport>,
    pub reused_attempt: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidationAttemptAuthority {
    schema_version: String,
    attempt_id: String,
    engagement_id: String,
    expected_engagement_generation: u64,
    planned_run_id: String,
    request_hash: String,
    job_id: String,
    source_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    mapping_mode: String,
    mapping_approval_id: Option<String>,
    mapping_approval_hash: Option<String>,
    policy_hash: String,
}

#[derive(Clone, Debug)]
struct ValidationAttemptRow {
    attempt_id: String,
    engagement_id: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
    job_id: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    policy_hash: String,
    run_id: Option<String>,
    authority_schema: String,
    expected_engagement_generation: Option<u64>,
    planned_run_id: Option<String>,
    source_hash: Option<String>,
    profile_id: Option<String>,
    profile_version: Option<String>,
    profile_hash: Option<String>,
    profile_approval_id: Option<String>,
    profile_approval_hash: Option<String>,
    mapping_mode: Option<String>,
    mapping_approval_id: Option<String>,
    mapping_approval_hash: Option<String>,
    canonical_authority_json: Option<String>,
    authority_hash: Option<String>,
    attempt_state: String,
    error_code: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngagementRecoveryResult {
    pub schema_version: String,
    pub workspace: RecoveryReport,
    pub state: EngagementState,
}

#[derive(Debug)]
struct PreflightRow {
    preflight_id: String,
    state: String,
    source_name: String,
    partial_relative_path: String,
    retained_relative_path: Option<String>,
    source_hash: Option<String>,
    source_size: Option<i64>,
    policy_hash: String,
    manifest_json: Option<String>,
    manifest_hash: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlledFileIdentity {
    schema_version: String,
    platform: String,
    primary: u64,
    secondary: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreflightFileIntent {
    schema_version: String,
    relative_path: String,
    file_identity: ControlledFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreflightPublishIntent {
    schema_version: String,
    retained_relative_path: String,
    source_hash: String,
    source_size: u64,
    manifest_json: String,
    manifest_hash: String,
    partial_file_identity: ControlledFileIdentity,
    retained_file_identity: ControlledFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyPreflightPublishIntent {
    schema_version: String,
    retained_relative_path: String,
    source_hash: String,
    source_size: u64,
    manifest_json: String,
    manifest_hash: String,
    partial_file_identity: ControlledFileIdentity,
}

#[derive(Debug)]
struct ControlledDirectory {
    path: PathBuf,
    handle: File,
}

#[derive(Clone, Debug)]
struct AcceptedPreflightEvidence {
    result: AcceptPreflightResult,
    acceptance_attempt_id: String,
}

#[derive(Clone, Debug)]
struct MappingDraftHeadRow {
    draft_id: String,
    generation: u64,
    state: String,
    head_revision_hash: String,
    submitted_mapping_id: Option<String>,
    submitted_mapping_version: Option<String>,
    submitted_mapping_hash: Option<String>,
    updated_at: String,
}

#[derive(Clone, Debug)]
struct MappingWorkspaceAttemptRow {
    attempt_id: String,
    command_kind: String,
    request_hash: String,
    draft_id: String,
    result_generation: u64,
    result_state: String,
    result_reference: String,
    result_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MappingApprovalEvidence {
    schema_version: String,
    approval_id: String,
    draft_id: String,
    draft_generation: u64,
    draft_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    actor: String,
    reason: String,
    acknowledgement: String,
    approved_at: String,
}

#[derive(Clone, Debug)]
struct MappingAuthorityHeadRow {
    draft_id: String,
    engagement_id: String,
    source_job_id: String,
    source_hash: String,
    object_pack_id: String,
    object_pack_version: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    generation: u64,
    state: String,
    head_revision_hash: String,
    submitted_mapping_id: Option<String>,
    submitted_mapping_version: Option<String>,
    submitted_mapping_hash: Option<String>,
    submitted_at: Option<String>,
    approved_at: Option<String>,
}

#[derive(Clone, Debug)]
struct MappingIntegrityAttemptRow {
    attempt_id: String,
    engagement_id: String,
    draft_id: String,
    command_kind: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
    expected_generation: u64,
    expected_state: String,
    result_generation: u64,
    result_state: String,
    result_reference: String,
    result_hash: String,
    created_at: String,
}

#[derive(Clone, Debug)]
struct MappingApprovalRow {
    approval_id: String,
    attempt_id: String,
    engagement_id: String,
    draft_id: String,
    draft_generation: u64,
    draft_revision_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    approver: String,
    reason: String,
    acknowledgement: String,
    canonical_approval_json: String,
    approval_hash: String,
    approved_at: String,
}

#[derive(Clone, Debug)]
struct MappingActiveAuthorityRow {
    job_id: Option<String>,
    object_pack_id: Option<String>,
    profile_id: Option<String>,
    profile_version: Option<String>,
    profile_hash: Option<String>,
    mapping_id: Option<String>,
    mapping_version: Option<String>,
    mapping_hash: Option<String>,
    mapping_stage: String,
}

#[derive(Clone, Debug)]
struct MappingWorkspaceProfileModel {
    entities: Vec<MappingWorkspaceEntityView>,
    relationships: Vec<MappingWorkspaceRelationshipView>,
    fields: HashMap<(String, String), ProfileFieldView>,
    rules: Vec<ProfileRuleView>,
    reference_table_obligations: Vec<MappingReferenceTableObligationView>,
}

impl EngagementService {
    /// Construct a service after validating every server-owned dependency.
    pub fn new(config: EngagementServiceConfig) -> Result<Self> {
        IntakePolicy::validate(&config.intake_policy)?;
        config.profiling_policy.validate()?;
        if config.object_packs.is_empty() {
            return Err(KernelError::PolicyInvalid(
                "engagement setup requires one controlled reviewed object pack".to_owned(),
            ));
        }
        let mut object_pack_ids = HashSet::new();
        for pack in &config.object_packs {
            validate_identifier(&pack.object_pack_id, "object pack ID")?;
            validate_identifier(&pack.version, "object pack version")?;
            validate_identifier(&pack.root_entity_id, "object pack root entity")?;
            if !matches!(
                pack.object_pack_id.as_str(),
                SYNTHETIC_REVIEWED_OBJECT_PACK_ID | SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID
            ) {
                return Err(KernelError::PolicyInvalid(format!(
                    "unsupported setup object pack {:?}; only explicitly enabled synthetic reviewed proof packs are accepted",
                    pack.object_pack_id
                )));
            }
            if pack.mapping_mode == MappingMode::ControlledSuggestions
                && pack.mapping_suggestions.is_empty()
            {
                return Err(KernelError::PolicyInvalid(
                    "controlled-suggestion object packs require at least one suggestion".to_owned(),
                ));
            }
            if pack.mapping_mode == MappingMode::OperatorGoverned
                && !pack.mapping_suggestions.is_empty()
            {
                return Err(KernelError::PolicyInvalid(
                    "operator-governed object packs must not embed mandatory source answers"
                        .to_owned(),
                ));
            }
            if !object_pack_ids.insert(pack.object_pack_id.as_str()) {
                return Err(KernelError::PolicyInvalid(
                    "object pack IDs must be unique".to_owned(),
                ));
            }
            let validated_profile = crate::profile::validate_profile_document(
                &config.profile_schema_path,
                &pack.profile_document,
            )?;
            let profile_summary = validated_profile.summary();
            if profile_summary.status != "REVIEWED_DRAFT"
                || profile_summary.profile_id != pack.object_pack_id
                || profile_summary.version != pack.version
            {
                return Err(KernelError::PolicyInvalid(
                    "controlled object pack must pin its exact REVIEWED_DRAFT profile document"
                        .to_owned(),
                ));
            }
        }

        // Textual profiling is useful for source shape only. Setup mode stores
        // no source sample values and enables no inferred duplicate semantics.
        let mut safe_profiling_policy = config.profiling_policy;
        safe_profiling_policy.maximum_samples_per_column = 0;
        safe_profiling_policy.maximum_sample_value_bytes = 0;
        safe_profiling_policy.duplicate_keys.clear();
        safe_profiling_policy.validate()?;

        let object_workspace = ObjectWorkspaceService::new(&config.workspace)?;
        let workspace = object_workspace.root().to_path_buf();
        let destination = DestinationService::new(&workspace, &config.profile_schema_path);
        let workspace_handle = retain_workspace_root(&workspace)?;
        verify_retained_workspace_root(&workspace, &workspace_handle)?;
        Ok(Self {
            workspace,
            workspace_handle: Arc::new(workspace_handle),
            object_workspace,
            destination,
            intake_policy: config.intake_policy,
            safe_profiling_policy,
            profile_schema_path: config.profile_schema_path,
            mapping_schema_path: config.mapping_schema_path,
            validation_policy: config.validation_policy,
            object_packs: config.object_packs,
        })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn object_workspace_status(&self) -> Result<ObjectWorkspaceStatus> {
        self.object_workspace.inspect()
    }

    /// Workspace-local destination contract authority used by the bounded
    /// engagement HTTP surface. Requests never provide a server path.
    #[must_use]
    pub fn destination_service(&self) -> &DestinationService {
        &self.destination
    }

    pub(crate) fn acquire_object_workspace_mutation_lock(&self) -> Result<WorkspaceLock> {
        self.verify_workspace_identity()?;
        self.object_workspace.require_active()?;
        let lock = WorkspaceLock::acquire(&self.workspace)?;
        self.verify_workspace_identity()?;
        self.object_workspace.require_active()?;
        lock.verify()?;
        Ok(lock)
    }

    pub fn initialize_object_workspace(
        &self,
        command: &EstablishObjectWorkspaceCommand,
    ) -> Result<ObjectWorkspaceRecord> {
        self.object_workspace.initialize(command)
    }

    pub fn adopt_object_workspace(
        &self,
        command: &EstablishObjectWorkspaceCommand,
    ) -> Result<ObjectWorkspaceRecord> {
        self.object_workspace.adopt(command)
    }

    /// Create the workspace's sole engagement or reopen the same immutable identity.
    pub fn create_or_reopen(&self, command: &CreateOrReopenCommand) -> Result<EngagementRecord> {
        validate_display_name(&command.display_name)?;
        validate_idempotency_key(&command.idempotency_key)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let object_workspace = self.object_workspace.require_active()?;
        let writer = self.open_writer()?;
        let existing: Option<(String, String, String)> = writer
            .connection()
            .query_row(
                "SELECT engagement_id, display_name, created_at
                 FROM engagement_records WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((engagement_id, display_name, created_at)) = existing {
            let bound_engagement_id = verify_workspace_engagement_binding(
                writer.connection(),
                &object_workspace.workspace_id,
            )?
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "active engagement has no object workspace binding".to_owned(),
                )
            })?;
            if bound_engagement_id != engagement_id {
                return Err(KernelError::IntegrityFailed(
                    "object workspace is bound to a different active engagement".to_owned(),
                ));
            }
            if display_name != command.display_name {
                return Err(KernelError::Conflict(
                    "this setup workspace is already bound to a different engagement name"
                        .to_owned(),
                ));
            }
            return Ok(EngagementRecord {
                schema_version: ENGAGEMENT_RECORD_SCHEMA.to_owned(),
                engagement_id,
                display_name,
                created_at,
                reopened: true,
            });
        }

        let engagement_id = Uuid::new_v4().to_string();
        let created_at = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO engagement_records (
                 singleton, engagement_id, schema_version, display_name,
                 idempotency_key, request_hash, canonical_request_json, created_at
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                engagement_id,
                ENGAGEMENT_RECORD_SCHEMA,
                command.display_name,
                command.idempotency_key,
                request_hash,
                canonical,
                created_at,
            ],
        )?;
        transaction.execute(
            "INSERT INTO engagement_active_context (
                 engagement_id, updated_at
             ) VALUES (?1, ?2)",
            params![engagement_id, created_at],
        )?;
        bind_workspace_engagement(
            &transaction,
            &object_workspace.workspace_id,
            &engagement_id,
            &created_at,
        )?;
        transaction.commit()?;
        Ok(EngagementRecord {
            schema_version: ENGAGEMENT_RECORD_SCHEMA.to_owned(),
            engagement_id,
            display_name: command.display_name.clone(),
            created_at,
            reopened: false,
        })
    }

    /// Read the durable active setup context without changing it.
    pub fn state(&self) -> Result<EngagementState> {
        let connection = self.open_read()?;
        load_engagement_state(&connection)
    }

    /// Return bounded immutable coverage for the exact active validated mapping.
    ///
    /// Unlike the existing practitioner read, this setup read does not depend
    /// on a selected validation run. It derives every identity from durable
    /// engagement state and verifies the replayed mapping still belongs to the
    /// active source and approved profile.
    pub fn active_mapping_coverage(&self, page: &PageRequest) -> Result<MappingCoveragePage> {
        let state = self.state()?;
        if !matches!(state.mapping_stage.as_str(), "PREPARED" | "VALIDATED") {
            return Err(KernelError::NotFound(
                "submitted engagement mapping coverage is not available".to_owned(),
            ));
        }
        if state.profile_stage != "APPROVED" {
            return Err(KernelError::IntegrityFailed(
                "validated engagement mapping has no approved profile authority".to_owned(),
            ));
        }
        let job_id = state.active.job_id.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active source job".to_owned(),
            )
        })?;
        let profile_id = state.active.profile_id.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active profile ID".to_owned(),
            )
        })?;
        let profile_version = state.active.profile_version.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active profile version".to_owned(),
            )
        })?;
        let profile_hash = state.active.profile_hash.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active profile hash".to_owned(),
            )
        })?;
        let mapping_id = state.active.mapping_id.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active mapping ID".to_owned(),
            )
        })?;
        let mapping_version = state.active.mapping_version.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active mapping version".to_owned(),
            )
        })?;
        let mapping_hash = state.active.mapping_hash.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "validated engagement mapping has no active mapping hash".to_owned(),
            )
        })?;
        let source_hash: String = self.open_read()?.query_row(
            "SELECT source_hash FROM jobs WHERE id = ?1 AND state = 'COMPLETE'",
            [job_id],
            |row| row.get(0),
        )?;

        let coverage = ReadService::new(&self.workspace).mapping_coverage(
            mapping_id,
            mapping_version,
            mapping_hash,
            page,
        )?;
        if coverage.mapping.validated_job_id != job_id
            || coverage.mapping.source_hash != source_hash
            || coverage.mapping.profile_id != profile_id
            || coverage.mapping.profile_version != profile_version
            || coverage.mapping.profile_hash != profile_hash
        {
            return Err(KernelError::IntegrityFailed(
                "validated mapping coverage differs from the active engagement authority"
                    .to_owned(),
            ));
        }
        Ok(coverage)
    }

    /// Return setup state plus bounded preflight history.
    pub fn detail(&self) -> Result<EngagementDetail> {
        let connection = self.open_read()?;
        let state = load_engagement_state(&connection)?;
        let mut statement = connection.prepare(
            "SELECT preflight_id, state, source_name, partial_relative_path,
                    retained_relative_path, source_hash, source_size, policy_hash,
                    manifest_json, manifest_hash, error_code, error_message,
                    created_at, updated_at, completed_at
             FROM engagement_source_preflights
             WHERE engagement_id = ?1
             ORDER BY created_at, preflight_id",
        )?;
        let rows = statement.query_map([&state.engagement_id], preflight_row)?;
        let preflights = rows
            .map(|row| preflight_view(row?))
            .collect::<Result<Vec<_>>>()?;
        Ok(EngagementDetail {
            schema_version: ENGAGEMENT_DETAIL_SCHEMA.to_owned(),
            state,
            preflights,
            available_object_pack_count: self.object_packs.len(),
        })
    }

    pub fn available_object_packs(&self) -> Result<ObjectPackList> {
        self.object_workspace.require_active()?;
        Ok(ObjectPackList {
            schema_version: OBJECT_PACK_LIST_SCHEMA.to_owned(),
            items: self.object_packs.clone(),
        })
    }

    /// Reserve a server-controlled `.part` path for a streamed request body.
    pub fn reserve_preflight(
        &self,
        command: &ReservePreflightCommand,
    ) -> Result<PreflightReservation> {
        validate_source_name(&command.source_name)?;
        validate_idempotency_key(&command.idempotency_key)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let policy_value = serde_json::to_value(&self.intake_policy)?;
        let policy_json = canonical_json_string(&policy_value)?;
        let policy_hash = canonical_hash(&policy_value)?;
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        let existing = load_preflight_by_idempotency(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )?;
        let row = if let Some((row, stored_request_hash)) = existing {
            if stored_request_hash != request_hash {
                return Err(KernelError::Conflict(
                    "preflight idempotency key was used for a different request".to_owned(),
                ));
            }
            row
        } else {
            require_original_source_intake_open(writer.connection(), &engagement_id)?;
            let preflight_id = Uuid::new_v4().to_string();
            let relative_path = format!("engagement/preflights/{preflight_id}.part");
            let now = now_rfc3339();
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
                    canonical,
                    relative_path,
                    policy_json,
                    policy_hash,
                    now,
                ],
            )?;
            insert_preflight_event(&transaction, &preflight_id, "RECEIVING", None, None, &now)?;
            update_context_preflight(&transaction, &engagement_id, &preflight_id, &now)?;
            transaction.commit()?;
            load_preflight(writer.connection(), &preflight_id)?
        };
        drop(writer);

        let controlled_path = if row.state == "RECEIVING" {
            match self.ensure_reserved_preflight_file(&row) {
                Ok(path) => path,
                Err(error) => {
                    let _ = self.terminate_preflight_with_file(
                        &PreflightTerminalCommand {
                            preflight_id: row.preflight_id.clone(),
                            error_code: "PREFLIGHT_RESERVATION_FAILED".to_owned(),
                            detail:
                                "the controlled local source reservation could not be established"
                                    .to_owned(),
                        },
                        "REJECTED",
                        None,
                    );
                    return Err(error);
                }
            }
        } else {
            controlled_preflight_path(&self.workspace, &row)?
        };
        Ok(PreflightReservation {
            preflight: preflight_view(row)?,
            controlled_path,
        })
    }

    fn ensure_reserved_preflight_file(&self, row: &PreflightRow) -> Result<PathBuf> {
        require_partial_preflight_path(row)?;
        self.verify_workspace_identity()?;
        let directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights"),
            true,
        )?;
        let name = preflight_partial_name(row);
        let existing_intent = {
            let connection = self.open_read()?;
            load_preflight_file_intent(&connection, &row.preflight_id)?
        };
        let file = if let Some(intent) = existing_intent {
            if intent.relative_path != row.partial_relative_path {
                return Err(KernelError::IntegrityFailed(
                    "preflight file intent differs from its controlled path".to_owned(),
                ));
            }
            let file = open_controlled_file(&directory, &name, true)?;
            require_controlled_file_identity(&file, &intent.file_identity)?;
            file
        } else {
            let file = create_new_controlled_file(&directory, &name)?;
            let identity = controlled_file_identity(&file)?;
            let intent = PreflightFileIntent {
                schema_version: PREFLIGHT_FILE_INTENT_SCHEMA.to_owned(),
                relative_path: row.partial_relative_path.clone(),
                file_identity: identity.clone(),
            };
            if let Err(error) = self.persist_preflight_file_intent(&row.preflight_id, &intent) {
                let _ = remove_controlled_file_if_owned(&directory, &name, &identity);
                return Err(error);
            }
            file
        };
        require_unaliased_controlled_file(&file)?;
        let metadata = file
            .metadata()
            .map_err(|error| KernelError::io(directory.path.join(&name), error))?;
        if metadata.len() != 0 {
            return Err(KernelError::Conflict(
                "reserved source preflight already contains bytes".to_owned(),
            ));
        }
        file.sync_all()
            .map_err(|error| KernelError::io(directory.path.join(&name), error))?;
        sync_controlled_directory(&directory)?;
        self.verify_workspace_identity()?;
        Ok(directory.path.join(name))
    }

    fn persist_preflight_file_intent(
        &self,
        preflight_id: &str,
        intent: &PreflightFileIntent,
    ) -> Result<()> {
        let detail = canonical_json_string(&serde_json::to_value(intent)?)?;
        let writer = self.open_writer()?;
        let row = load_preflight(writer.connection(), preflight_id)?;
        if row.state != "RECEIVING" {
            return Err(KernelError::Conflict(
                "preflight file intent cannot be recorded after reception ended".to_owned(),
            ));
        }
        if let Some(existing) = load_preflight_file_intent(writer.connection(), preflight_id)? {
            if existing == *intent {
                return Ok(());
            }
            return Err(KernelError::IntegrityFailed(
                "preflight has conflicting immutable file intents".to_owned(),
            ));
        }
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        insert_preflight_event(
            &transaction,
            preflight_id,
            "RECEIVING",
            None,
            Some(&detail),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Require an upload to reuse the exact retry identity that created its reservation.
    pub(crate) fn require_preflight_upload_identity(
        &self,
        preflight_id: &str,
        idempotency_key: &str,
    ) -> Result<()> {
        validate_identifier(preflight_id, "preflight ID")?;
        validate_idempotency_key(idempotency_key)?;
        let connection = self.open_read()?;
        let engagement_id = require_engagement_id(&connection)?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT state, idempotency_key
                 FROM engagement_source_preflights
                 WHERE engagement_id = ?1 AND preflight_id = ?2",
                params![engagement_id, preflight_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, reserved_idempotency_key)) = row else {
            return Err(KernelError::NotFound(format!(
                "source preflight {preflight_id}"
            )));
        };
        if state != "RECEIVING" {
            return Err(KernelError::Conflict(
                "source preflight is not available for a new byte stream".to_owned(),
            ));
        }
        if reserved_idempotency_key != idempotency_key {
            return Err(KernelError::Conflict(
                "source upload retry identity differs from its exact reservation".to_owned(),
            ));
        }
        require_source_preflight_mutation_open(&connection, &engagement_id, preflight_id)?;
        Ok(())
    }

    /// Open the exact durable reservation without traversing request-controlled paths.
    pub(crate) fn open_preflight_upload(
        &self,
        preflight_id: &str,
        idempotency_key: &str,
    ) -> Result<File> {
        self.require_preflight_upload_identity(preflight_id, idempotency_key)?;
        let connection = self.open_read()?;
        let row = load_preflight(&connection, preflight_id)?;
        require_partial_preflight_path(&row)?;
        let intent = load_preflight_file_intent(&connection, preflight_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "source preflight has no durable reserved-file identity".to_owned(),
            )
        })?;
        drop(connection);
        let directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights"),
            false,
        )?;
        let file = open_controlled_file(&directory, &preflight_partial_name(&row), true)?;
        require_controlled_file_identity(&file, &intent.file_identity)?;
        require_unaliased_controlled_file(&file)?;
        if file
            .metadata()
            .map_err(|error| KernelError::io(&directory.path, error))?
            .len()
            != 0
        {
            return Err(KernelError::Conflict(
                "source preflight reservation already contains bytes".to_owned(),
            ));
        }
        self.verify_workspace_identity()?;
        Ok(file)
    }

    /// Verify exact bytes and archive evidence, then retain the immutable preflight.
    #[allow(clippy::too_many_lines)]
    pub fn complete_preflight(
        &self,
        command: &CompletePreflightCommand,
    ) -> Result<SourcePreflightView> {
        self.complete_preflight_with_file(command, None)
    }

    pub(crate) fn complete_preflight_upload(
        &self,
        command: &CompletePreflightCommand,
        file: &File,
    ) -> Result<SourcePreflightView> {
        self.complete_preflight_with_file(command, Some(file))
    }

    #[allow(clippy::too_many_lines)]
    fn complete_preflight_with_file(
        &self,
        command: &CompletePreflightCommand,
        expected_file: Option<&File>,
    ) -> Result<SourcePreflightView> {
        validate_identifier(&command.preflight_id, "preflight ID")?;
        validate_sha256(&command.sha256, "preflight SHA-256")?;
        let writer = self.open_writer()?;
        let row = load_preflight(writer.connection(), &command.preflight_id)?;
        if row.state == "READY" {
            if row.source_hash.as_deref() == Some(command.sha256.as_str())
                && optional_nonnegative(row.source_size, "preflight source size")?
                    == Some(command.size_bytes)
            {
                let file_intent =
                    load_preflight_file_intent(writer.connection(), &command.preflight_id)?;
                drop(writer);
                self.verify_ready_preflight_file(&row)?;
                self.cleanup_preflight_partial(&row, file_intent.as_ref(), expected_file)?;
                return preflight_view(row);
            }
            return Err(KernelError::Conflict(
                "completed preflight differs from the supplied exact identity".to_owned(),
            ));
        }
        if row.state != "RECEIVING" {
            return Err(KernelError::Conflict(format!(
                "preflight {} is {}; only RECEIVING may complete",
                command.preflight_id, row.state
            )));
        }
        let engagement_id = require_engagement_id(writer.connection())?;
        require_source_preflight_mutation_open(
            writer.connection(),
            &engagement_id,
            &command.preflight_id,
        )?;
        if row.policy_hash != canonical_hash(&serde_json::to_value(&self.intake_policy)?)? {
            return Err(KernelError::Conflict(
                "preflight was reserved under a different intake policy".to_owned(),
            ));
        }
        require_partial_preflight_path(&row)?;
        let file_intent = load_preflight_file_intent(writer.connection(), &row.preflight_id)?
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "completing preflight has no durable reserved-file identity".to_owned(),
                )
            })?;
        if file_intent.relative_path != row.partial_relative_path {
            return Err(KernelError::IntegrityFailed(
                "completing preflight file intent differs from its controlled path".to_owned(),
            ));
        }
        let preflight_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights"),
            false,
        )?;
        let partial_name = preflight_partial_name(&row);
        let partial_file = open_controlled_file(&preflight_directory, &partial_name, false)?;
        require_controlled_file_identity(&partial_file, &file_intent.file_identity)?;
        if let Some(file) = expected_file {
            require_controlled_file_identity(file, &file_intent.file_identity)?;
        }
        require_unaliased_controlled_file(&partial_file)?;
        let (actual_hash, actual_size) =
            hash_open_regular_file(&partial_file, self.intake_policy.maximum_source_bytes)?;
        if actual_hash != command.sha256 || actual_size != command.size_bytes {
            return Err(KernelError::Conflict(format!(
                "streamed preflight identity differs: expected {}/{}, found {actual_hash}/{actual_size}",
                command.sha256, command.size_bytes
            )));
        }
        let partial = preflight_directory.path.join(&partial_name);
        let manifest = inspect_archive(&partial, &self.intake_policy)?;
        let replayed_partial = open_controlled_file(&preflight_directory, &partial_name, false)?;
        require_controlled_file_identity(&replayed_partial, &file_intent.file_identity)?;
        require_unaliased_controlled_file(&replayed_partial)?;
        let (replayed_hash, replayed_size) =
            hash_open_regular_file(&replayed_partial, self.intake_policy.maximum_source_bytes)?;
        if replayed_hash != actual_hash || replayed_size != actual_size {
            return Err(KernelError::IntegrityFailed(
                "source preflight changed during bounded archive inspection".to_owned(),
            ));
        }
        let manifest_value = serde_json::to_value(&manifest)?;
        let manifest_json = canonical_json_string(&manifest_value)?;
        let manifest_hash = canonical_hash(&manifest_value)?;
        let retained_relative_path =
            format!("engagement/preflights/retained/{}.zip", command.sha256);
        let retained_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights/retained"),
            true,
        )?;
        let retained_name = OsString::from(format!("{}.zip", command.sha256));
        let retained_file_identity = Self::select_retained_publication_identity(
            writer.connection(),
            &retained_directory,
            &retained_name,
            &retained_relative_path,
            &actual_hash,
            actual_size,
            &file_intent.file_identity,
        )?;
        let publish_intent = PreflightPublishIntent {
            schema_version: PREFLIGHT_PUBLISH_INTENT_SCHEMA.to_owned(),
            retained_relative_path: retained_relative_path.clone(),
            source_hash: actual_hash.clone(),
            source_size: actual_size,
            manifest_json: manifest_json.clone(),
            manifest_hash: manifest_hash.clone(),
            partial_file_identity: file_intent.file_identity.clone(),
            retained_file_identity,
        };
        Self::persist_preflight_publish_intent_locked(&writer, &row.preflight_id, &publish_intent)?;
        sync_read_only_controlled_file(
            &preflight_directory,
            &partial_name,
            &partial_file,
            expected_file,
        )?;
        let retained = publish_controlled_file(
            &preflight_directory,
            &partial_name,
            &retained_directory,
            &retained_name,
            &file_intent.file_identity,
            &publish_intent.retained_file_identity,
            &actual_hash,
            actual_size,
            self.intake_policy.maximum_source_bytes,
        )?;
        sync_read_only_controlled_file(&retained_directory, &retained_name, &retained, None)?;
        let expected_retained_links =
            self.require_declared_retained_aliases(&row, &publish_intent, &retained)?;
        install_retained_ready_commit_guard(
            &writer,
            &retained_directory,
            &retained_name,
            &retained,
            &publish_intent.retained_file_identity,
            expected_retained_links,
        )?;

        let now = now_rfc3339();
        let size = i64::try_from(actual_size).map_err(|_| {
            KernelError::PolicyInvalid("preflight size does not fit SQLite INTEGER".to_owned())
        })?;
        let transaction = writer.connection().unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE engagement_source_preflights
             SET state = 'READY', retained_relative_path = ?1,
                 source_hash = ?2, source_size = ?3,
                 manifest_json = ?4, manifest_hash = ?5,
                 updated_at = ?6, completed_at = ?6
             WHERE preflight_id = ?7 AND state = 'RECEIVING'",
            params![
                retained_relative_path,
                actual_hash,
                size,
                manifest_json,
                manifest_hash,
                now,
                command.preflight_id,
            ],
        )?;
        require_one(changed, "complete source preflight")?;
        insert_preflight_event(
            &transaction,
            &command.preflight_id,
            "READY",
            None,
            None,
            &now,
        )?;
        transaction.commit()?;
        let completed = load_preflight(writer.connection(), &command.preflight_id)?;
        drop(writer);
        self.verify_ready_preflight_file(&completed)?;
        self.cleanup_preflight_partial(&completed, Some(&file_intent), expected_file)?;
        preflight_view(completed)
    }

    fn persist_preflight_publish_intent_locked(
        writer: &WorkspaceWriter,
        preflight_id: &str,
        intent: &PreflightPublishIntent,
    ) -> Result<()> {
        require_supported_preflight_publish_intent(intent)?;
        if intent.schema_version != PREFLIGHT_PUBLISH_INTENT_SCHEMA {
            return Err(KernelError::IntegrityFailed(
                "new preflight publication authority must use the current identity contract"
                    .to_owned(),
            ));
        }
        if intent.manifest_json.len() > MAXIMUM_COMMAND_BYTES {
            return Err(KernelError::IntegrityFailed(
                "preflight publish intent exceeds its bounded manifest evidence".to_owned(),
            ));
        }
        let manifest = parse_unique_json(intent.manifest_json.as_bytes())?;
        if canonical_json_string(&manifest)? != intent.manifest_json
            || canonical_hash(&manifest)? != intent.manifest_hash
        {
            return Err(KernelError::IntegrityFailed(
                "preflight publish intent has invalid manifest evidence".to_owned(),
            ));
        }
        if let Some(existing) = load_preflight_publish_intent(writer.connection(), preflight_id)? {
            if existing == *intent {
                return Ok(());
            }
            return Err(KernelError::IntegrityFailed(
                "preflight has conflicting immutable publication intents".to_owned(),
            ));
        }
        let detail = canonical_json_string(&serde_json::to_value(intent)?)?;
        if detail.len() > MAXIMUM_COMMAND_BYTES {
            return Err(KernelError::IntegrityFailed(
                "preflight publish intent exceeds its durable evidence bound".to_owned(),
            ));
        }
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        insert_preflight_event(
            &transaction,
            preflight_id,
            "RECEIVING",
            None,
            Some(&detail),
            &now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn verify_ready_preflight_file(&self, row: &PreflightRow) -> Result<()> {
        let connection = self.open_read()?;
        verify_ready_preflight_authority(
            &self.workspace,
            self.workspace_handle.as_ref(),
            &connection,
            row,
        )?;
        self.verify_workspace_identity()
    }

    fn require_declared_retained_aliases(
        &self,
        row: &PreflightRow,
        intent: &PreflightPublishIntent,
        retained: &File,
    ) -> Result<u64> {
        let connection = self.open_read()?;
        require_declared_retained_aliases_with_connection(
            &self.workspace,
            self.workspace_handle.as_ref(),
            &connection,
            row,
            intent,
            retained,
            true,
        )
    }

    fn require_shared_ready_retained_authorities(
        connection: &Connection,
        retained_relative_path: &str,
        source_hash: &str,
        source_size: u64,
        retained_identity: &ControlledFileIdentity,
    ) -> Result<usize> {
        let rows = {
            let mut statement = connection.prepare(
                "SELECT preflight_id, source_hash, source_size, manifest_json, manifest_hash
                 FROM engagement_source_preflights
                 WHERE state = 'READY' AND retained_relative_path = ?1
                 ORDER BY preflight_id",
            )?;
            statement
                .query_map([retained_relative_path], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut current_authorities = 0_usize;
        for (preflight_id, row_hash, row_size, manifest_json, manifest_hash) in rows {
            let row_size = u64::try_from(row_size).map_err(|_| {
                KernelError::IntegrityFailed(
                    "shared retained preflight has a negative source size".to_owned(),
                )
            })?;
            let publish_intent = load_preflight_publish_intent(connection, &preflight_id)?
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "shared READY preflight has no publication authority".to_owned(),
                    )
                })?;
            let file_intent =
                load_preflight_file_intent(connection, &preflight_id)?.ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "shared READY preflight has no partial-file authority".to_owned(),
                    )
                })?;
            require_supported_preflight_publish_intent(&publish_intent)?;
            if row_hash != source_hash
                || row_size != source_size
                || publish_intent.retained_relative_path != retained_relative_path
                || publish_intent.source_hash != source_hash
                || publish_intent.source_size != source_size
                || publish_intent.manifest_json != manifest_json
                || publish_intent.manifest_hash != manifest_hash
                || publish_intent.partial_file_identity != file_intent.file_identity
                || publish_intent.retained_file_identity != *retained_identity
            {
                return Err(KernelError::IntegrityFailed(
                    "shared READY preflight differs from its retained-file authority".to_owned(),
                ));
            }
            if publish_intent.schema_version == PREFLIGHT_PUBLISH_INTENT_SCHEMA {
                current_authorities = current_authorities.checked_add(1).ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "shared retained authority count overflowed".to_owned(),
                    )
                })?;
            }
        }
        Ok(current_authorities)
    }

    #[allow(clippy::too_many_arguments)]
    fn select_retained_publication_identity(
        connection: &Connection,
        retained_directory: &ControlledDirectory,
        retained_name: &OsStr,
        retained_relative_path: &str,
        source_hash: &str,
        source_size: u64,
        partial_identity: &ControlledFileIdentity,
    ) -> Result<ControlledFileIdentity> {
        let retained = match open_controlled_file(retained_directory, retained_name, false) {
            Ok(file) => file,
            Err(KernelError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(partial_identity.clone());
            }
            Err(error) => return Err(error),
        };
        let retained_identity = controlled_file_identity(&retained)?;
        let current_authorities = Self::require_shared_ready_retained_authorities(
            connection,
            retained_relative_path,
            source_hash,
            source_size,
            &retained_identity,
        )?;
        if current_authorities == 0 {
            Ok(partial_identity.clone())
        } else {
            Ok(retained_identity)
        }
    }

    #[allow(clippy::too_many_lines)]
    fn reconcile_published_preflight(
        &self,
        row: &PreflightRow,
        intent: &PreflightPublishIntent,
    ) -> Result<SourcePreflightView> {
        if row.state == "READY" {
            self.verify_ready_preflight_file(row)?;
            return preflight_view(load_preflight(&self.open_read()?, &row.preflight_id)?);
        }
        if row.state != "RECEIVING" {
            return Err(KernelError::Conflict(
                "terminal preflight cannot resume immutable publication".to_owned(),
            ));
        }
        validate_sha256(&intent.source_hash, "preflight publish source hash")?;
        validate_sha256(&intent.manifest_hash, "preflight publish manifest hash")?;
        let expected_retained =
            format!("engagement/preflights/retained/{}.zip", intent.source_hash);
        require_supported_preflight_publish_intent(intent)?;
        if intent.retained_relative_path != expected_retained {
            return Err(KernelError::IntegrityFailed(
                "preflight publication intent has an unsafe retained identity".to_owned(),
            ));
        }
        let manifest = parse_unique_json(intent.manifest_json.as_bytes())?;
        if canonical_json_string(&manifest)? != intent.manifest_json
            || canonical_hash(&manifest)? != intent.manifest_hash
        {
            return Err(KernelError::IntegrityFailed(
                "preflight publication intent has invalid manifest evidence".to_owned(),
            ));
        }
        require_partial_preflight_path(row)?;
        let connection = self.open_read()?;
        let file_intent =
            load_preflight_file_intent(&connection, &row.preflight_id)?.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "published preflight has no durable reserved-file identity".to_owned(),
                )
            })?;
        drop(connection);
        if file_intent.file_identity != intent.partial_file_identity {
            return Err(KernelError::IntegrityFailed(
                "preflight publication intent differs from its reserved file".to_owned(),
            ));
        }
        let preflight_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights"),
            false,
        )?;
        let retained_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights/retained"),
            true,
        )?;
        let partial_name = preflight_partial_name(row);
        let retained_name = OsString::from(format!("{}.zip", intent.source_hash));
        let partial = match open_controlled_file(&preflight_directory, &partial_name, false) {
            Ok(file) => {
                require_controlled_file_identity(&file, &intent.partial_file_identity)?;
                let (hash, size) =
                    hash_open_regular_file(&file, self.intake_policy.maximum_source_bytes)?;
                if hash != intent.source_hash || size != intent.source_size {
                    return Err(KernelError::IntegrityFailed(
                        "published preflight partial differs from its immutable intent".to_owned(),
                    ));
                }
                sync_read_only_controlled_file(&preflight_directory, &partial_name, &file, None)?;
                Some(file)
            }
            Err(KernelError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let retained = match open_controlled_file(&retained_directory, &retained_name, false) {
            Ok(file) => file,
            Err(KernelError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                partial.as_ref().ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "preflight publication lost both partial and retained evidence".to_owned(),
                    )
                })?;
                publish_controlled_file(
                    &preflight_directory,
                    &partial_name,
                    &retained_directory,
                    &retained_name,
                    &intent.partial_file_identity,
                    &intent.retained_file_identity,
                    &intent.source_hash,
                    intent.source_size,
                    self.intake_policy.maximum_source_bytes,
                )?;
                open_controlled_file(&retained_directory, &retained_name, false)?
            }
            Err(error) => return Err(error),
        };
        require_controlled_file_identity(&retained, &intent.retained_file_identity)?;
        let expected_retained_links =
            self.require_declared_retained_aliases(row, intent, &retained)?;
        let retained_path = retained_directory.path.join(&retained_name);
        sync_read_only_controlled_file(&retained_directory, &retained_name, &retained, None)?;
        let (retained_hash, retained_size) =
            hash_open_regular_file(&retained, self.intake_policy.maximum_source_bytes)?;
        if retained_hash != intent.source_hash || retained_size != intent.source_size {
            return Err(KernelError::IntegrityFailed(
                "retained preflight differs from its immutable publish intent".to_owned(),
            ));
        }
        let replayed_manifest = inspect_archive(&retained_path, &self.intake_policy)?;
        if canonical_json_string(&serde_json::to_value(replayed_manifest)?)? != intent.manifest_json
        {
            return Err(KernelError::IntegrityFailed(
                "retained preflight manifest differs from its immutable publish intent".to_owned(),
            ));
        }
        let replayed_retained = open_controlled_file(&retained_directory, &retained_name, false)?;
        require_controlled_file_identity(&replayed_retained, &intent.retained_file_identity)?;
        require_exact_link_count(&replayed_retained, expected_retained_links)?;
        require_read_only_controlled_file(&replayed_retained, &retained_path)?;
        let (replayed_hash, replayed_size) =
            hash_open_regular_file(&replayed_retained, self.intake_policy.maximum_source_bytes)?;
        if replayed_hash != intent.source_hash || replayed_size != intent.source_size {
            return Err(KernelError::IntegrityFailed(
                "retained preflight changed during manifest replay".to_owned(),
            ));
        }

        let writer = self.open_writer()?;
        install_retained_ready_commit_guard(
            &writer,
            &retained_directory,
            &retained_name,
            &replayed_retained,
            &intent.retained_file_identity,
            expected_retained_links,
        )?;
        let now = now_rfc3339();
        let source_size = i64::try_from(intent.source_size).map_err(|_| {
            KernelError::IntegrityFailed(
                "preflight publication size does not fit durable state".to_owned(),
            )
        })?;
        let transaction = writer.connection().unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE engagement_source_preflights
             SET state = 'READY', retained_relative_path = ?1,
                 source_hash = ?2, source_size = ?3,
                 manifest_json = ?4, manifest_hash = ?5,
                 updated_at = ?6, completed_at = ?6
             WHERE preflight_id = ?7 AND state = 'RECEIVING'",
            params![
                intent.retained_relative_path,
                intent.source_hash,
                source_size,
                intent.manifest_json,
                intent.manifest_hash,
                now,
                row.preflight_id,
            ],
        )?;
        require_one(changed, "reconcile published source preflight")?;
        insert_preflight_event(&transaction, &row.preflight_id, "READY", None, None, &now)?;
        transaction.commit()?;
        let completed = load_preflight(writer.connection(), &row.preflight_id)?;
        drop(writer);
        self.verify_ready_preflight_file(&completed)?;
        self.cleanup_preflight_partial(&completed, Some(&file_intent), None)?;
        preflight_view(completed)
    }

    pub fn interrupt_preflight(
        &self,
        command: &PreflightTerminalCommand,
    ) -> Result<SourcePreflightView> {
        self.terminate_preflight_with_file(command, "INTERRUPTED", None)
    }

    pub fn fail_preflight(
        &self,
        command: &PreflightTerminalCommand,
    ) -> Result<SourcePreflightView> {
        self.terminate_preflight_with_file(command, "REJECTED", None)
    }

    pub(crate) fn fail_preflight_upload(
        &self,
        command: &PreflightTerminalCommand,
        file: &File,
    ) -> Result<SourcePreflightView> {
        self.terminate_preflight_with_file(command, "REJECTED", Some(file))
    }

    /// Return browser-safe setup authority and negative capabilities.
    pub fn context(&self) -> Result<EngagementContext> {
        let object_workspace = self.object_workspace.inspect()?;
        let supported_contracts = vec![
            ENGAGEMENT_CONTEXT_SCHEMA.to_owned(),
            ENGAGEMENT_STATE_SCHEMA.to_owned(),
            PREFLIGHT_SCHEMA.to_owned(),
            CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            PREPARE_MAPPING_COMMAND_SCHEMA.to_owned(),
            MAPPING_WORKSPACE_SCHEMA.to_owned(),
            MAPPING_CONTRACT_COVERAGE_SCHEMA.to_owned(),
            SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            START_REVISED_TOUR_COMMAND_SCHEMA.to_owned(),
            RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
            LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA.to_owned(),
            crate::object_workspace::OBJECT_WORKSPACE_STATUS_SCHEMA.to_owned(),
            crate::object_workspace::INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
            crate::object_workspace::ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
        ];
        let Some(workspace_record) = object_workspace.workspace else {
            return Ok(EngagementContext {
                schema_version: ENGAGEMENT_CONTEXT_SCHEMA.to_owned(),
                workspace_label: "Object workspace identity pending".to_owned(),
                engagement: None,
                supported_contracts,
                capabilities: EngagementCapabilities {
                    engagement_setup: false,
                    source_intake: false,
                    textual_profiling: false,
                    reviewed_object_pack_selection: false,
                    explicit_profile_approval: false,
                    mapping_preparation: false,
                    validation_launch: false,
                    governed_issue_decisions: false,
                    migration_result_approval: false,
                    target_write: false,
                    publication: false,
                },
            });
        };
        let connection = self.open_read()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM engagement_records WHERE singleton = 1)",
            [],
            |row| row.get(0),
        )?;
        let engagement = if exists {
            Some(load_engagement_state(&connection)?)
        } else {
            None
        };
        let downstream_gated = engagement
            .as_ref()
            .map(|state| self.downstream_authority_gated_for_state(state))
            .transpose()?
            .unwrap_or(false);
        let governed_issue_decisions = engagement.as_ref().is_some_and(|state| {
            state.phase == EngagementPhase::FirstIssuesReady && !downstream_gated
        });
        let source_intake = engagement
            .as_ref()
            .is_none_or(|state| state.active.job_id.is_none());
        Ok(EngagementContext {
            schema_version: ENGAGEMENT_CONTEXT_SCHEMA.to_owned(),
            workspace_label: workspace_record.display_name,
            engagement,
            supported_contracts,
            capabilities: EngagementCapabilities {
                engagement_setup: true,
                source_intake,
                textual_profiling: true,
                reviewed_object_pack_selection: true,
                explicit_profile_approval: true,
                mapping_preparation: true,
                validation_launch: true,
                governed_issue_decisions,
                migration_result_approval: false,
                target_write: false,
                publication: false,
            },
        })
    }

    /// Accept exact preflight evidence, ingest it once, then run value-redacted profiling.
    pub fn accept_preflight(
        &self,
        command: &AcceptPreflightCommand,
    ) -> Result<AcceptPreflightResult> {
        Ok(self.accept_preflight_evidence(command, true)?.result)
    }

    #[allow(clippy::too_many_lines)]
    fn accept_preflight_evidence(
        &self,
        command: &AcceptPreflightCommand,
        activate_source: bool,
    ) -> Result<AcceptedPreflightEvidence> {
        require_schema(&command.schema_version, ACCEPT_SOURCE_COMMAND_SCHEMA)?;
        validate_identifier(&command.preflight_id, "preflight ID")?;
        validate_sha256(&command.expected_source_hash, "accepted source hash")?;
        validate_sha256(&command.expected_manifest_hash, "accepted manifest hash")?;
        validate_idempotency_key(&command.idempotency_key)?;
        if command.acknowledgement != SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT {
            return Err(KernelError::PolicyInvalid(format!(
                "source acceptance requires acknowledgement {SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT:?}"
            )));
        }
        let policy_value = serde_json::to_value(&self.safe_profiling_policy)?;
        let profiling_policy_hash = canonical_hash(&policy_value)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());

        let (engagement_id, attempt_id, controlled_source_relative_path, expected_size) = {
            let writer = self.open_writer()?;
            let engagement_id = require_engagement_id(writer.connection())?;
            let preflight = load_preflight(writer.connection(), &command.preflight_id)?;
            if preflight.state != "READY"
                || preflight.source_hash.as_deref() != Some(command.expected_source_hash.as_str())
                || preflight.manifest_hash.as_deref()
                    != Some(command.expected_manifest_hash.as_str())
            {
                return Err(KernelError::Conflict(
                    "source acceptance does not match one exact READY preflight".to_owned(),
                ));
            }
            let expected_size =
                optional_nonnegative(preflight.source_size, "accepted preflight size")?
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "READY preflight has no source size".to_owned(),
                        )
                    })?;
            let existing: Option<(String, String, String)> = writer
                .connection()
                .query_row(
                    "SELECT attempt_id, request_hash, controlled_source_relative_path
                     FROM engagement_acceptance_attempts
                     WHERE engagement_id = ?1 AND idempotency_key = ?2",
                    params![engagement_id, command.idempotency_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((attempt_id, stored_hash, relative_path)) = existing {
                if stored_hash != request_hash {
                    return Err(KernelError::Conflict(
                        "source acceptance idempotency key was used for a different command"
                            .to_owned(),
                    ));
                }
                (engagement_id, attempt_id, relative_path, expected_size)
            } else {
                if activate_source {
                    require_original_source_intake_open(writer.connection(), &engagement_id)?;
                }
                let attempt_id = Uuid::new_v4().to_string();
                let relative_path = format!("engagement/acceptances/{attempt_id}.zip");
                let now = now_rfc3339();
                writer.connection().execute(
                    "INSERT INTO engagement_acceptance_attempts (
                         attempt_id, engagement_id, preflight_id, idempotency_key,
                         request_hash, canonical_request_json, accepted_source_hash,
                         accepted_manifest_hash, profiling_policy_hash, acknowledgement,
                         controlled_source_relative_path, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                    params![
                        attempt_id,
                        engagement_id,
                        command.preflight_id,
                        command.idempotency_key,
                        request_hash,
                        canonical,
                        command.expected_source_hash,
                        command.expected_manifest_hash,
                        profiling_policy_hash,
                        command.acknowledgement,
                        relative_path,
                        now,
                    ],
                )?;
                (engagement_id, attempt_id, relative_path, expected_size)
            }
        };
        let controlled_source = self.ensure_acceptance_source(
            &command.preflight_id,
            &attempt_id,
            &controlled_source_relative_path,
            &command.expected_source_hash,
            expected_size,
        )?;

        let mut job_id = reconcile_acceptance_jobs(
            &self.workspace,
            &attempt_id,
            &controlled_source,
            &command.expected_source_hash,
            expected_size,
        )?;
        if job_id.is_none() {
            let report = ingest_package(&controlled_source, &self.workspace, &self.intake_policy)?;
            job_id = Some(report.job_id);
        }
        let job_id = job_id.ok_or_else(|| {
            KernelError::IntegrityFailed("source acceptance produced no intake job".to_owned())
        })?;
        bind_completed_acceptance_job(
            &self.workspace,
            &attempt_id,
            &engagement_id,
            &job_id,
            &controlled_source,
            &command.expected_source_hash,
            expected_size,
        )?;

        let status = workspace_status(&self.workspace)?;
        let job = status
            .jobs
            .iter()
            .find(|candidate| candidate.id == job_id)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(format!("missing accepted job {job_id}"))
            })?;
        if job.state != JobState::Complete {
            return Err(KernelError::Conflict(format!(
                "idempotent source acceptance is bound to job {job_id} in terminal state {:?}; use a new idempotency key to start another attempt",
                job.state
            )));
        }
        if job.source_hash.as_deref() != Some(command.expected_source_hash.as_str())
            || job.source_size != Some(expected_size)
        {
            return Err(KernelError::IntegrityFailed(
                "accepted job differs from preflight source identity".to_owned(),
            ));
        }

        let profiling = run_profile(&self.workspace, &job_id, &self.safe_profiling_policy)?;
        let writer = self.open_writer()?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        require_exact_completed_acceptance_job(
            &transaction,
            &attempt_id,
            &engagement_id,
            &job_id,
            &controlled_source,
            &command.expected_source_hash,
            expected_size,
            true,
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_acceptance_attempts
             SET profiling_run_id = ?1, updated_at = ?2
             WHERE attempt_id = ?3 AND job_id = ?4
               AND (profiling_run_id IS NULL OR profiling_run_id = ?1)",
            params![profiling.run_id, now, attempt_id, job_id],
        )?;
        require_one(changed, "bind source acceptance profiling run")?;
        if activate_source {
            let current_job: Option<String> = transaction.query_row(
                "SELECT active_job_id FROM engagement_active_context
                 WHERE engagement_id = ?1",
                [&engagement_id],
                |row| row.get(0),
            )?;
            if current_job.as_deref() == Some(job_id.as_str()) {
                let changed = transaction.execute(
                    "UPDATE engagement_active_context
                     SET active_profiling_run_id = ?1, updated_at = ?2
                     WHERE engagement_id = ?3",
                    params![profiling.run_id, now, engagement_id],
                )?;
                require_one(changed, "refresh accepted source profiling authority")?;
            } else if current_job.is_none() {
                let changed = transaction.execute(
                    "UPDATE engagement_active_context
                     SET active_job_id = ?1, active_profiling_run_id = ?2,
                         generation = generation + 1, updated_at = ?3,
                         active_mapping_id = NULL, active_mapping_version = NULL,
                         active_mapping_hash = NULL, mapping_stage = 'NOT_PREPARED',
                         active_validation_run_id = NULL,
                         validation_stage = 'NOT_LAUNCHED'
                     WHERE engagement_id = ?4",
                    params![job_id, profiling.run_id, now, engagement_id],
                )?;
                require_one(changed, "activate accepted source evidence")?;
            } else {
                return Err(KernelError::Conflict(
                    "another accepted source became active before this acceptance completed"
                        .to_owned(),
                ));
            }
        }
        transaction.commit()?;
        Ok(AcceptedPreflightEvidence {
            result: AcceptPreflightResult {
                schema_version: ACCEPT_RESULT_SCHEMA.to_owned(),
                engagement_id,
                preflight_id: command.preflight_id.clone(),
                job_id,
                profiling_run_id: profiling.run_id,
                source_hash: command.expected_source_hash.clone(),
                profiling_result_hash: profiling.result_hash,
                reused_profile: profiling.reused_complete,
            },
            acceptance_attempt_id: attempt_id,
        })
    }

    fn ensure_acceptance_source(
        &self,
        preflight_id: &str,
        attempt_id: &str,
        relative_path: &str,
        expected_hash: &str,
        expected_size: u64,
    ) -> Result<PathBuf> {
        let expected_relative = format!("engagement/acceptances/{attempt_id}.zip");
        if relative_path != expected_relative {
            return Err(KernelError::IntegrityFailed(
                "acceptance source path differs from its durable attempt identity".to_owned(),
            ));
        }
        self.verify_workspace_identity()?;
        let connection = self.open_read()?;
        let preflight = load_preflight(&connection, preflight_id)?;
        if preflight.state != "READY"
            || preflight.source_hash.as_deref() != Some(expected_hash)
            || optional_nonnegative(preflight.source_size, "acceptance source size")?
                != Some(expected_size)
        {
            return Err(KernelError::IntegrityFailed(
                "acceptance intent differs from its exact READY preflight".to_owned(),
            ));
        }
        drop(connection);
        self.verify_ready_preflight_file(&preflight)?;
        let retained_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights/retained"),
            false,
        )?;
        let retained_name = OsString::from(format!("{expected_hash}.zip"));
        let retained = open_controlled_file(&retained_directory, &retained_name, false)?;
        let retained_identity = controlled_file_identity(&retained)?;

        let acceptance_directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/acceptances"),
            true,
        )?;
        let acceptance_name = OsString::from(format!("{attempt_id}.zip"));
        let acceptance = match open_controlled_file(&acceptance_directory, &acceptance_name, false)
        {
            Ok(file) => file,
            Err(KernelError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                match publish_controlled_file_platform(
                    &retained_directory,
                    &retained_name,
                    &acceptance_directory,
                    &acceptance_name,
                ) {
                    Ok(()) => {
                        sync_controlled_directory(&acceptance_directory)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => self.copy_acceptance_source(
                        attempt_id,
                        &retained,
                        &acceptance_directory,
                        &acceptance_name,
                        expected_hash,
                        expected_size,
                    )?,
                }
                open_controlled_file(&acceptance_directory, &acceptance_name, false)?
            }
            Err(error) => return Err(error),
        };
        let (actual_hash, actual_size) =
            hash_open_regular_file(&acceptance, self.intake_policy.maximum_source_bytes)?;
        if actual_hash != expected_hash || actual_size != expected_size {
            return Err(KernelError::IntegrityFailed(
                "controlled acceptance source differs from its exact preflight evidence".to_owned(),
            ));
        }
        sync_read_only_controlled_file(&acceptance_directory, &acceptance_name, &acceptance, None)?;
        let acceptance_identity = controlled_file_identity(&acceptance)?;
        if acceptance_identity != retained_identity {
            require_unaliased_controlled_file(&acceptance)?;
        }
        self.verify_ready_preflight_file(&preflight)?;
        self.verify_workspace_identity()?;
        Ok(acceptance_directory.path.join(acceptance_name))
    }

    fn copy_acceptance_source(
        &self,
        attempt_id: &str,
        retained: &File,
        acceptance_directory: &ControlledDirectory,
        acceptance_name: &OsStr,
        expected_hash: &str,
        expected_size: u64,
    ) -> Result<()> {
        let copy_name = OsString::from(format!("{attempt_id}.copy-part"));
        let copy = match open_controlled_file(acceptance_directory, &copy_name, false) {
            Ok(file) => file,
            Err(KernelError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let mut file = create_new_controlled_file(acceptance_directory, &copy_name)?;
                let identity = controlled_file_identity(&file)?;
                let mut reader = retained
                    .try_clone()
                    .map_err(|error| KernelError::io(&acceptance_directory.path, error))?;
                reader
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| KernelError::io(&acceptance_directory.path, error))?;
                if let Err(error) =
                    std::io::copy(&mut reader.take(expected_size.saturating_add(1)), &mut file)
                {
                    let _ = remove_controlled_file_if_owned(
                        acceptance_directory,
                        &copy_name,
                        &identity,
                    );
                    return Err(KernelError::io(&acceptance_directory.path, error));
                }
                file.sync_all()
                    .map_err(|error| KernelError::io(&acceptance_directory.path, error))?;
                file
            }
            Err(error) => return Err(error),
        };
        require_unaliased_controlled_file(&copy)?;
        let copy_identity = controlled_file_identity(&copy)?;
        let (copy_hash, copy_size) =
            hash_open_regular_file(&copy, self.intake_policy.maximum_source_bytes)?;
        if copy_hash != expected_hash || copy_size != expected_size {
            return Err(KernelError::IntegrityFailed(
                "acceptance copy partial differs from its durable source intent".to_owned(),
            ));
        }
        sync_read_only_controlled_file(acceptance_directory, &copy_name, &copy, None)?;
        match publish_controlled_file_platform(
            acceptance_directory,
            &copy_name,
            acceptance_directory,
            acceptance_name,
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(KernelError::io(&acceptance_directory.path, error)),
        }
        let published = open_controlled_file(acceptance_directory, acceptance_name, false)?;
        let (published_hash, published_size) =
            hash_open_regular_file(&published, self.intake_policy.maximum_source_bytes)?;
        if published_hash != expected_hash || published_size != expected_size {
            return Err(KernelError::IntegrityFailed(
                "published acceptance copy differs from its durable source intent".to_owned(),
            ));
        }
        remove_controlled_file_if_owned(acceptance_directory, &copy_name, &copy_identity)?;
        sync_controlled_directory(acceptance_directory)
    }

    /// Register the exact controlled `REVIEWED_DRAFT` profile without approving it.
    #[allow(clippy::too_many_lines)]
    pub fn select_object_pack(
        &self,
        command: &SelectObjectPackCommand,
    ) -> Result<SelectedObjectPackResult> {
        require_schema(&command.schema_version, SELECT_OBJECT_PACK_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let pack = self
            .object_packs
            .iter()
            .find(|candidate| candidate.object_pack_id == command.object_pack_id)
            .cloned()
            .ok_or_else(|| {
                KernelError::NotFound(format!("controlled object pack {}", command.object_pack_id))
            })?;
        let state = self.state()?;
        {
            let connection = self.open_read()?;
            if let Some(reference) = load_command_attempt(
                &connection,
                &state.engagement_id,
                "SELECT_OBJECT_PACK",
                &command.idempotency_key,
                &request_hash,
            )? {
                if reference != pack.object_pack_id
                    || state.active.object_pack_id.as_deref() != Some(pack.object_pack_id.as_str())
                {
                    return Err(KernelError::Conflict(
                        "object-pack selection replay differs from the active pack".to_owned(),
                    ));
                }
                let (profile_id, profile_version, _) = selected_profile_identity(&state)?;
                return Ok(SelectedObjectPackResult {
                    object_pack: pack,
                    profile: inspect_profile(&self.workspace, profile_id, profile_version)?,
                });
            }
        }
        let profile = register_profile_document(
            &self.workspace,
            &self.profile_schema_path,
            &pack.profile_document,
            &RegistryLimits::default(),
        )?;
        if !matches!(
            profile.lifecycle_status,
            ProfileLifecycleStatus::ReviewedDraft | ProfileLifecycleStatus::Approved
        ) {
            return Err(KernelError::ProfileNotApproved(
                "controlled setup pack must register as REVIEWED_DRAFT".to_owned(),
            ));
        }
        let lifecycle_stage = if profile.lifecycle_status == ProfileLifecycleStatus::Approved {
            "APPROVED"
        } else {
            "REVIEWED"
        };
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        insert_command_attempt(
            &transaction,
            &engagement_id,
            "SELECT_OBJECT_PACK",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            Some(&pack.object_pack_id),
            &now,
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_active_context
             SET selected_object_pack_id = ?1, selected_profile_id = ?2,
                 selected_profile_version = ?3, selected_profile_hash = ?4,
                 profile_stage = ?5,
                 destination_origin = 'CONTROLLED_PACK',
                 destination_authority_id = ?1,
                 destination_origin_hash = ?4,
                 active_mapping_id = NULL,
                 active_mapping_version = NULL, active_mapping_hash = NULL,
                 mapping_stage = 'NOT_PREPARED', active_validation_run_id = NULL,
                 validation_stage = 'NOT_LAUNCHED',
                 generation = generation + 1, updated_at = ?6
             WHERE engagement_id = ?7 AND generation = ?8",
            params![
                pack.object_pack_id,
                profile.profile_id,
                profile.profile_version,
                profile.profile_hash,
                lifecycle_stage,
                now,
                engagement_id,
                i64::try_from(state.generation).unwrap_or(i64::MAX),
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "controlled destination selection lost its engagement compare-and-swap".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(SelectedObjectPackResult {
            object_pack: pack,
            profile,
        })
    }

    /// Explicitly grant only local mapping/validation authority.
    pub fn approve_profile(&self, command: &ApproveProfileCommand) -> Result<ProfileVersionView> {
        require_schema(
            &command.schema_version,
            APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA,
        )?;
        validate_idempotency_key(&command.idempotency_key)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        validate_bounded_text(&command.actor, "approval actor", MAXIMUM_IDENTIFIER_BYTES)?;
        validate_bounded_text(&command.reason, "approval reason", MAXIMUM_REASON_BYTES)?;
        if command.authority_acknowledgement != LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT {
            return Err(KernelError::PolicyInvalid(format!(
                "profile approval requires acknowledgement {LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT:?}"
            )));
        }
        let state = self.state()?;
        let (profile_id, version, hash) = selected_profile_identity(&state)?;
        {
            let connection = self.open_read()?;
            if load_command_attempt(
                &connection,
                &state.engagement_id,
                "APPROVE_PROFILE",
                &command.idempotency_key,
                &request_hash,
            )?
            .is_some()
            {
                return inspect_profile(&self.workspace, profile_id, version);
            }
        }
        let approved = approve_profile(
            &self.workspace,
            profile_id,
            version,
            hash,
            &command.actor,
            &command.reason,
            true,
            &RegistryLimits::default(),
        )?;
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        insert_command_attempt(
            &transaction,
            &engagement_id,
            "APPROVE_PROFILE",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            approved
                .approval
                .as_ref()
                .map(|approval| approval.approval_id.as_str()),
            &now,
        )?;
        transaction.execute(
            "UPDATE engagement_active_context
             SET profile_stage = 'APPROVED', generation = generation + 1,
                 updated_at = ?1
             WHERE engagement_id = ?2
               AND selected_profile_id = ?3
               AND selected_profile_version = ?4
               AND selected_profile_hash = ?5",
            params![now, engagement_id, profile_id, version, hash],
        )?;
        transaction.commit()?;
        Ok(approved)
    }

    /// Return source-column identities and selected profile fields/rules only.
    pub fn source_profile_rules(&self) -> Result<SourceProfileRulesView> {
        let state = self.state()?;
        let job_id = state.active.job_id.as_deref().ok_or_else(|| {
            KernelError::Conflict("source column view requires active intake".to_owned())
        })?;
        let profiling_run_id = state.active.profiling_run_id.as_deref().ok_or_else(|| {
            KernelError::Conflict("source column view requires safe profiling".to_owned())
        })?;
        let (profile_id, profile_version, profile_hash) = selected_profile_identity(&state)?;
        let connection = self.open_read()?;
        let mut statement = connection.prepare(
            "SELECT path, header_json FROM csv_files
             WHERE job_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([job_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut source_columns = Vec::new();
        for row in rows {
            let (source_file, header_json) = row?;
            let header_json = header_json.ok_or_else(|| {
                KernelError::IntegrityFailed(format!("accepted CSV {source_file:?} has no header"))
            })?;
            let headers: Vec<String> = serde_json::from_str(&header_json)?;
            for (source_column_ordinal, source_column) in headers.into_iter().enumerate() {
                source_columns.push(SourceColumnView {
                    source_file: source_file.clone(),
                    source_column,
                    source_column_ordinal,
                });
            }
        }
        let profile = crate::read_model::ReadService::new(&self.workspace).profile_view(
            profile_id,
            profile_version,
            profile_hash,
        )?;
        let (profile_fields, rules) = profile_fields_and_rules(&profile.data.document)?;
        Ok(SourceProfileRulesView {
            schema_version: SOURCE_PROFILE_RULES_SCHEMA.to_owned(),
            job_id: job_id.to_owned(),
            profiling_run_id: profiling_run_id.to_owned(),
            stored_sample_values: false,
            source_columns,
            profile_fields,
            rules,
        })
    }

    /// Return the exact header-only operator mapping workspace and its durable draft.
    #[allow(clippy::too_many_lines)]
    pub fn mapping_workspace(&self) -> Result<MappingWorkspaceView> {
        let state = self.state()?;
        if state.profile_stage != "APPROVED" {
            return Err(KernelError::ProfileNotApproved(
                "mapping workspace requires explicit local profile approval".to_owned(),
            ));
        }
        let pack = self.operator_pack_for_state(&state)?;
        let destination_origin = state
            .active
            .destination_origin
            .as_deref()
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "mapping workspace has no exact destination origin".to_owned(),
                )
            })?
            .to_owned();
        let tour_chain = self.tour_chain()?;
        let revision_draft_scope = tour_chain
            .pending_attempt
            .as_ref()
            .filter(|attempt| {
                attempt.state == RevisedTourAttemptState::Incompatible
                    && attempt.error_code.as_deref() == Some("MAPPING_REQUIRED")
            })
            .map(|attempt| {
                let job_id = attempt.job_id.clone().ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "mapping-required source revision has no accepted candidate job"
                            .to_owned(),
                    )
                })?;
                let source_hash = attempt.source_hash.clone().ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "mapping-required source revision has no accepted source hash".to_owned(),
                    )
                })?;
                let profiling_run_id = attempt.profiling_run_id.clone().ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "mapping-required source revision has no sample-free profiling run"
                            .to_owned(),
                    )
                })?;
                Ok::<_, KernelError>((
                    job_id,
                    source_hash,
                    profiling_run_id.clone(),
                    MappingRevisionDraftScopeView {
                        tour_attempt_id: attempt.tour_attempt_id.clone(),
                        tour_number: attempt.tour_number,
                        baseline_tour_id: attempt.baseline_tour_id.clone(),
                        profiling_run_id,
                        submission_available: false,
                        unavailable_reason: "This structurally changed source revision can be mapped and saved as a durable draft, but it cannot replace the prior approved mapping or active revision until a separate exact mapping-authority transition is implemented."
                            .to_owned(),
                    },
                ))
            })
            .transpose()?;
        let (job_id, expected_source_hash, profiling_run_id, revision_draft_scope) =
            if let Some((job_id, source_hash, profiling_run_id, scope)) = revision_draft_scope {
                (job_id, source_hash, profiling_run_id, Some(scope))
            } else {
                (
                    state.active.job_id.clone().ok_or_else(|| {
                        KernelError::Conflict("mapping workspace requires active intake".to_owned())
                    })?,
                    String::new(),
                    state.active.profiling_run_id.clone().ok_or_else(|| {
                        KernelError::Conflict(
                            "mapping workspace requires active sample-free profiling".to_owned(),
                        )
                    })?,
                    None,
                )
            };
        let (profile_id, profile_version, profile_hash) = selected_profile_identity(&state)?;
        let connection = self.open_read()?;
        let source_hash: String = connection.query_row(
            "SELECT source_hash FROM jobs WHERE id = ?1 AND state = 'COMPLETE'",
            [&job_id],
            |row| row.get(0),
        )?;
        if !expected_source_hash.is_empty() && source_hash != expected_source_hash {
            return Err(KernelError::IntegrityFailed(
                "mapping-required candidate job differs from immutable source-revision evidence"
                    .to_owned(),
            ));
        }
        let profiling_identity: (String, String) = connection.query_row(
            "SELECT job_id, source_hash
             FROM profiling_runs
             WHERE id = ?1 AND state = 'COMPLETE'",
            [&profiling_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if profiling_identity.0 != job_id || profiling_identity.1 != source_hash {
            return Err(KernelError::IntegrityFailed(
                "mapping workspace profiling identity is not the exact sample-free source"
                    .to_owned(),
            ));
        }
        let mut samples = connection.prepare(
            "SELECT samples_json FROM profiling_columns
             WHERE run_id = ?1 ORDER BY path, column_ordinal",
        )?;
        for sample_json in samples
            .query_map([&profiling_run_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            let values: Vec<Value> = serde_json::from_str(&sample_json)?;
            if !values.is_empty() {
                return Err(KernelError::IntegrityFailed(
                    "mapping workspace cannot expose a profiling run that retained samples"
                        .to_owned(),
                ));
            }
        }
        let source_files = mapping_workspace_source_files(&connection, &job_id)?;
        let profile = ReadService::new(&self.workspace).profile_view(
            profile_id,
            profile_version,
            profile_hash,
        )?;
        let profile_model = mapping_workspace_profile(&profile.data.document)?;
        let draft = load_active_mapping_draft(
            &connection,
            &state.engagement_id,
            &job_id,
            &source_hash,
            &pack.object_pack_id,
            &pack.version,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        Ok(MappingWorkspaceView {
            schema_version: MAPPING_WORKSPACE_SCHEMA.to_owned(),
            engagement_generation: state.generation,
            job_id,
            source_hash,
            profiling_run_id,
            revision_draft_scope,
            destination_origin,
            object_pack_id: pack.object_pack_id.clone(),
            object_pack_version: pack.version.clone(),
            profile_id: profile_id.to_owned(),
            profile_version: profile_version.to_owned(),
            profile_hash: profile_hash.to_owned(),
            mapping_mode: pack.mapping_mode,
            source_files,
            entities: profile_model.entities,
            relationships: profile_model.relationships,
            allowed_operations: vec![
                MappingKind::Direct,
                MappingKind::Trim,
                MappingKind::EmptyAsNull,
                MappingKind::UppercaseAscii,
                MappingKind::LowercaseAscii,
                MappingKind::Lookup,
                MappingKind::Constant,
            ],
            controlled_reference_tables: pack.reference_tables.clone(),
            reference_table_obligations: profile_model.reference_table_obligations,
            limits: MappingWorkspaceLimitsView {
                maximum_source_files: MAXIMUM_SOURCE_FILES,
                maximum_bindings: MAXIMUM_BINDINGS,
                maximum_relationships: MAXIMUM_RELATIONSHIPS,
                maximum_reference_tables: MAXIMUM_REFERENCE_TABLES,
                maximum_reference_entries: self.validation_policy.maximum_lookup_entries,
                maximum_reference_bytes: self.validation_policy.maximum_lookup_bytes,
                maximum_reference_key_bytes: MAXIMUM_REFERENCE_KEY_BYTES,
                maximum_reference_value_bytes: MAXIMUM_REFERENCE_VALUE_BYTES,
            },
            draft,
        })
    }

    /// Derive one value-free contract-coverage view from exact durable authority.
    #[allow(clippy::too_many_lines)]
    pub fn mapping_contract_coverage(&self) -> Result<MappingContractCoverageView> {
        let state = self.state()?;
        let workspace_record = self.object_workspace.require_active()?;
        let workspace = self.mapping_workspace()?;
        if workspace.mapping_mode != MappingMode::OperatorGoverned {
            return Err(KernelError::Conflict(
                "mapping contract coverage is available for the operator-governed workshop"
                    .to_owned(),
            ));
        }
        let profiling_run_id = workspace.profiling_run_id.clone();
        let connection = self.open_read()?;
        let profile_view = crate::profile_registry::inspect_profile_connection(
            &connection,
            &workspace.profile_id,
            &workspace.profile_version,
            &workspace.profile_hash,
        )?;
        if profile_view.lifecycle_status != ProfileLifecycleStatus::Approved
            || profile_view.authority != ProfileAuthority::LocalMappingValidation
            || !profile_view.can_start_new_runs
        {
            return Err(KernelError::ProfileNotApproved(
                "mapping contract coverage requires exact approved local profile authority"
                    .to_owned(),
            ));
        }
        let profile_approval = profile_view.approval.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "approved profile has no exact approval identity".to_owned(),
            )
        })?;
        let profile = ReadService::new(&self.workspace).profile_view(
            &workspace.profile_id,
            &workspace.profile_version,
            &workspace.profile_hash,
        )?;
        let profile_model = mapping_workspace_profile(&profile.data.document)?;
        let mapping_identity = workspace.draft.as_ref().and_then(|draft| {
            Some((
                draft.submitted_mapping_id.as_deref()?,
                draft.submitted_mapping_version.as_deref()?,
                draft.submitted_mapping_hash.as_deref()?,
            ))
        });
        let mapping_approval = match (workspace.draft.as_ref(), mapping_identity) {
            (Some(draft), Some((mapping_id, mapping_version, mapping_hash)))
                if draft.state == "APPROVED" =>
            {
                Some(
                    verified_operator_mapping_approval_identity(
                        &connection,
                        &state.engagement_id,
                        mapping_id,
                        mapping_version,
                        mapping_hash,
                    )?
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "approved mapping draft has no independently verified approval"
                                .to_owned(),
                        )
                    })?,
                )
            }
            _ => None,
        };
        let authority = MappingContractCoverageAuthority {
            workspace_id: workspace_record.workspace_id.clone(),
            workspace_descriptor_hash: workspace_record.descriptor_hash.clone(),
            engagement_id: state.engagement_id.clone(),
            engagement_generation: workspace.engagement_generation,
            job_id: workspace.job_id.clone(),
            source_hash: workspace.source_hash.clone(),
            profiling_run_id,
            destination_origin: workspace.destination_origin.clone(),
            profile_id: workspace.profile_id.clone(),
            profile_version: workspace.profile_version.clone(),
            profile_hash: workspace.profile_hash.clone(),
            profile_approval_id: profile_approval.approval_id,
            profile_approval_hash: profile_approval.approval_hash,
            draft_id: workspace.draft.as_ref().map(|draft| draft.draft_id.clone()),
            draft_generation: workspace.draft.as_ref().map(|draft| draft.draft_generation),
            draft_hash: workspace
                .draft
                .as_ref()
                .map(|draft| draft.draft_hash.clone()),
            draft_state: workspace.draft.as_ref().map(|draft| draft.state.clone()),
            mapping_id: mapping_identity.map(|identity| identity.0.to_owned()),
            mapping_version: mapping_identity.map(|identity| identity.1.to_owned()),
            mapping_hash: mapping_identity.map(|identity| identity.2.to_owned()),
            mapping_approval_id: mapping_approval
                .as_ref()
                .map(|identity: &(String, String)| identity.0.clone()),
            mapping_approval_hash: mapping_approval.map(|identity| identity.1),
        };
        let mut coverage = derive_mapping_contract_coverage(authority, &workspace, &profile_model)?;
        coverage.coverage_hash = mapping_contract_coverage_hash(&coverage)?;
        let final_state = self.state()?;
        let final_workspace = self.object_workspace.require_active()?;
        if final_state.generation != state.generation
            || final_state.engagement_id != state.engagement_id
            || final_state.active.job_id != state.active.job_id
            || final_state.active.profile_id != state.active.profile_id
            || final_state.active.profile_version != state.active.profile_version
            || final_state.active.profile_hash != state.active.profile_hash
            || final_state.active.mapping_id != state.active.mapping_id
            || final_state.active.mapping_version != state.active.mapping_version
            || final_state.active.mapping_hash != state.active.mapping_hash
            || final_workspace.workspace_id != workspace_record.workspace_id
            || final_workspace.descriptor_hash != workspace_record.descriptor_hash
        {
            return Err(KernelError::Conflict(
                "mapping contract authority changed while coverage was derived".to_owned(),
            ));
        }
        Ok(coverage)
    }

    /// Whether the active pack must stop at read-only First Issues.
    pub fn multi_entity_authority_gated(&self) -> Result<bool> {
        let state = self.state()?;
        self.downstream_authority_gated_for_state(&state)
    }

    #[allow(clippy::too_many_lines)]
    fn selected_pack_for_state(
        &self,
        state: &EngagementState,
    ) -> Result<Option<ControlledObjectPack>> {
        let Some(pack_id) = state.active.object_pack_id.as_deref() else {
            if state.active.profile_id.is_some()
                || state.active.profile_version.is_some()
                || state.active.profile_hash.is_some()
                || state.active.destination_origin.is_some()
                || state.active.destination_authority_id.is_some()
                || state.active.destination_origin_hash.is_some()
                || state.profile_stage != "NOT_SELECTED"
            {
                return Err(KernelError::IntegrityFailed(
                    "active destination authority is incomplete".to_owned(),
                ));
            }
            return Ok(None);
        };
        let (profile_id, profile_version, profile_hash) = selected_profile_identity(state)?;
        let destination_origin = state.active.destination_origin.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed("active destination has no origin".to_owned())
        })?;
        let destination_authority_id = state
            .active
            .destination_authority_id
            .as_deref()
            .ok_or_else(|| {
                KernelError::IntegrityFailed("active destination has no authority ID".to_owned())
            })?;
        let destination_origin_hash =
            state
                .active
                .destination_origin_hash
                .as_deref()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed("active destination has no origin hash".to_owned())
                })?;
        if destination_origin == DestinationOrigin::ControlledPack.as_str() {
            let pack = self
                .object_packs
                .iter()
                .find(|pack| pack.object_pack_id == pack_id)
                .cloned()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "selected object pack is absent from the configured catalogue".to_owned(),
                    )
                })?;
            let expected_profile_hash = canonical_hash(&pack.profile_document)?;
            if profile_id != pack.object_pack_id
                || profile_version != pack.version
                || profile_hash != expected_profile_hash
                || destination_authority_id != pack.object_pack_id
                || destination_origin_hash != expected_profile_hash
            {
                return Err(KernelError::IntegrityFailed(
                    "selected controlled pack differs from the active destination binding"
                        .to_owned(),
                ));
            }
            return Ok(Some(pack));
        }
        if destination_origin != DestinationOrigin::WorkspaceProfile.as_str()
            || pack_id != profile_id
        {
            return Err(KernelError::IntegrityFailed(
                "active destination origin is unsupported or inconsistent".to_owned(),
            ));
        }
        let context = match state.profile_stage.as_str() {
            "REVIEWED" => {
                let context = self
                    .destination
                    .verified_profile_context(
                        destination_origin,
                        destination_authority_id,
                        destination_origin_hash,
                        profile_id,
                        profile_version,
                        profile_hash,
                    )?
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "reviewed workspace destination authority did not resolve".to_owned(),
                        )
                    })?;
                let connection = self.open_read()?;
                let profile = crate::profile_registry::inspect_profile_connection(
                    &connection,
                    profile_id,
                    profile_version,
                    profile_hash,
                )?;
                if profile.lifecycle_status != ProfileLifecycleStatus::ReviewedDraft
                    || profile.authority != ProfileAuthority::None
                    || profile.approval.is_some()
                {
                    return Err(KernelError::IntegrityFailed(
                        "reviewed workspace destination lifecycle differs from its unapproved active stage"
                            .to_owned(),
                    ));
                }
                context
            }
            "APPROVED" => self
                .destination
                .mapping_context(
                    destination_origin,
                    destination_authority_id,
                    destination_origin_hash,
                    profile_id,
                    profile_version,
                    profile_hash,
                )?
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "approved workspace destination authority did not resolve".to_owned(),
                    )
                })?,
            other => {
                return Err(KernelError::IntegrityFailed(format!(
                    "workspace destination has unsupported profile stage {other:?}"
                )));
            }
        };
        let connection = self.open_read()?;
        let profile_document = crate::profile_registry::load_verified_profile_document(
            &connection,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        let title = profile_document
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(profile_id)
            .to_owned();
        Ok(Some(ControlledObjectPack {
            object_pack_id: profile_id.to_owned(),
            version: profile_version.to_owned(),
            title,
            description:
                "Workspace-reviewed destination profile with no embedded source mapping answers."
                    .to_owned(),
            scope: "Workspace-local, target-neutral mapping and validation authority.".to_owned(),
            root_entity_id: context.root_entity_id,
            mapping_mode: MappingMode::OperatorGoverned,
            limitations: vec![
                "No target compatibility, target write, publication, or external transmission authority."
                    .to_owned(),
            ],
            mapping_suggestions: Vec::new(),
            reference_tables: Vec::new(),
            profile_document,
        }))
    }

    fn downstream_authority_gated_for_state(&self, state: &EngagementState) -> Result<bool> {
        if self.selected_pack_for_state(state)?.is_none() {
            return Ok(false);
        }
        let entity_count = self.active_profile_entity_count(state)?;
        Ok(entity_count != Some(1))
    }

    fn active_profile_entity_count(&self, state: &EngagementState) -> Result<Option<u64>> {
        let identity = match (
            state.active.profile_id.as_deref(),
            state.active.profile_version.as_deref(),
            state.active.profile_hash.as_deref(),
        ) {
            (None, None, None) => return Ok(None),
            (Some(profile_id), Some(profile_version), Some(profile_hash)) => {
                (profile_id, profile_version, profile_hash)
            }
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "active profile identity is incomplete".to_owned(),
                ));
            }
        };
        let profile =
            ReadService::new(&self.workspace).profile_view(identity.0, identity.1, identity.2)?;
        let count = profile
            .data
            .document
            .get("entities")
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "active canonical profile has no entity scope".to_owned(),
                )
            })?;
        Ok(Some(u64::try_from(count).map_err(|_| {
            KernelError::IntegrityFailed(
                "active profile entity count exceeds its contract".to_owned(),
            )
        })?))
    }

    /// Derive the fail-closed navigation spine from exact persisted authority.
    ///
    /// This read does not grant authority. It verifies every identity needed by
    /// an emitted route and omits links whenever the corresponding engagement
    /// or downstream capability is unavailable.
    #[allow(clippy::too_many_lines)]
    pub fn journey(&self) -> Result<JourneyView> {
        let workspace = self.object_workspace.inspect()?;
        let context = self.context()?;
        let Some(state) = context.engagement else {
            return Ok(derive_journey(
                JourneyDerivation {
                    binding: JourneyBinding::default(),
                    surface: JourneySurface::Engagement,
                    engagement_exists: false,
                    source_complete: false,
                    destination_complete: false,
                    mapping_complete: false,
                    operator_governed_mapping: false,
                    validation_complete: false,
                    issues_available: false,
                    comparison_available: false,
                    comparison_pending: false,
                    comparison_complete: false,
                    downstream: DownstreamJourneyAuthority::Disabled,
                    decisions_available: false,
                    reconciliation_available: false,
                    package_available: false,
                },
                workspace,
            ));
        };
        let connection = self.open_read()?;
        let reads = ReadService::new(&self.workspace);
        let chain = self.tour_chain()?;
        let job_identity = state
            .active
            .job_id
            .as_deref()
            .map(|job_id| {
                connection.query_row(
                    "SELECT source_hash, state FROM jobs WHERE id = ?1",
                    [job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
            })
            .transpose()?;
        let profiling = state
            .active
            .profiling_run_id
            .as_deref()
            .map(|run_id| {
                reads
                    .profiling_summary(run_id, false, &PageRequest::default())
                    .map_err(|error| {
                        KernelError::IntegrityFailed(format!(
                            "journey profiling authority failed replay: {error}"
                        ))
                    })
            })
            .transpose()?;
        if let Some(summary) = profiling.as_ref() {
            let profiling_source_hash: String = connection.query_row(
                "SELECT source_hash FROM profiling_runs WHERE id = ?1",
                [&summary.scope.run_id],
                |row| row.get(0),
            )?;
            if state.active.job_id.as_deref() != Some(summary.scope.job_id.as_str())
                || job_identity
                    .as_ref()
                    .is_none_or(|(source_hash, _)| source_hash != &profiling_source_hash)
            {
                return Err(KernelError::IntegrityFailed(
                    "journey profiling run does not match the active source job".to_owned(),
                ));
            }
        }
        let source_complete = job_identity
            .as_ref()
            .is_some_and(|(_, job_state)| job_state == "COMPLETE")
            && profiling.is_some();
        let active_preflight = state
            .active
            .preflight_id
            .as_deref()
            .map(|preflight_id| load_preflight(&connection, preflight_id))
            .transpose()?;
        let manifest_hash = if let Some((source_hash, _)) = job_identity.as_ref() {
            let preflight = active_preflight.as_ref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "journey active source has no accepted preflight identity".to_owned(),
                )
            })?;
            if preflight.state != "READY"
                || preflight.source_hash.as_deref() != Some(source_hash.as_str())
            {
                return Err(KernelError::IntegrityFailed(
                    "journey active source differs from accepted preflight evidence".to_owned(),
                ));
            }
            let manifest_json = preflight.manifest_json.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "journey accepted preflight has no manifest document".to_owned(),
                )
            })?;
            let manifest_hash = preflight.manifest_hash.as_ref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "journey accepted preflight has no manifest hash".to_owned(),
                )
            })?;
            let manifest_value = parse_unique_json(manifest_json.as_bytes()).map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "journey accepted preflight manifest is invalid: {error}"
                ))
            })?;
            if canonical_hash(&manifest_value)? != *manifest_hash {
                return Err(KernelError::IntegrityFailed(
                    "journey accepted preflight manifest hash differs from its document".to_owned(),
                ));
            }
            Some(manifest_hash.clone())
        } else {
            None
        };
        let destination_complete = state.profile_stage == "APPROVED"
            && state.active.profile_id.is_some()
            && state.active.profile_version.is_some()
            && state.active.profile_hash.is_some();
        if state.profile_stage == "APPROVED" && !destination_complete {
            return Err(KernelError::IntegrityFailed(
                "journey approved profile identity is incomplete".to_owned(),
            ));
        }
        let mapping_complete = state.mapping_stage == "VALIDATED"
            && state.active.mapping_id.is_some()
            && state.active.mapping_version.is_some()
            && state.active.mapping_hash.is_some();
        if state.mapping_stage == "VALIDATED" && !mapping_complete {
            return Err(KernelError::IntegrityFailed(
                "journey validated mapping identity is incomplete".to_owned(),
            ));
        }
        if mapping_complete {
            let coverage = reads
                .mapping_coverage(
                    state.active.mapping_id.as_deref().ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "journey validated mapping has no ID".to_owned(),
                        )
                    })?,
                    state.active.mapping_version.as_deref().ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "journey validated mapping has no version".to_owned(),
                        )
                    })?,
                    state.active.mapping_hash.as_deref().ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "journey validated mapping has no hash".to_owned(),
                        )
                    })?,
                    &PageRequest::default(),
                )
                .map_err(|error| {
                    KernelError::IntegrityFailed(format!(
                        "journey mapping authority failed replay: {error}"
                    ))
                })?;
            let source_matches = job_identity
                .as_ref()
                .is_some_and(|(source_hash, _)| source_hash == &coverage.mapping.source_hash);
            let verified_tour_reuse = chain.active_tour.as_ref().is_some_and(|tour| {
                tour.tour_number > 1
                    && job_identity
                        .as_ref()
                        .is_some_and(|(source_hash, _)| source_hash == &tour.source_hash)
                    && tour.mapping_id == coverage.mapping.mapping_id
                    && tour.mapping_version == coverage.mapping.mapping_version
                    && tour.mapping_hash == coverage.mapping.mapping_hash
            });
            let exact_source_authority = (state.active.job_id.as_deref()
                == Some(coverage.mapping.validated_job_id.as_str())
                && source_matches)
                || verified_tour_reuse;
            let exact = exact_source_authority
                && state.active.profile_id.as_deref() == Some(coverage.mapping.profile_id.as_str())
                && state.active.profile_version.as_deref()
                    == Some(coverage.mapping.profile_version.as_str())
                && state.active.profile_hash.as_deref()
                    == Some(coverage.mapping.profile_hash.as_str());
            if !exact {
                return Err(KernelError::IntegrityFailed(
                    "journey mapping coverage differs from active source and profile authority"
                        .to_owned(),
                ));
            }
        }
        let validation = state
            .active
            .validation_run_id
            .as_deref()
            .map(|run_id| reads.validation_summary(run_id))
            .transpose()?;
        if let Some(summary) = validation.as_ref() {
            let data = &summary.data;
            let exact = state.active.job_id.as_deref() == Some(data.job_id.as_str())
                && job_identity
                    .as_ref()
                    .is_some_and(|(source_hash, _)| source_hash == &data.source_hash)
                && state.active.profile_id.as_deref() == Some(data.profile_id.as_str())
                && state.active.profile_version.as_deref() == Some(data.profile_version.as_str())
                && state.active.profile_hash.as_deref() == Some(data.profile_hash.as_str())
                && state.active.mapping_id.as_deref() == Some(data.mapping_id.as_str())
                && state.active.mapping_version.as_deref() == Some(data.mapping_version.as_str())
                && state.active.mapping_hash.as_deref() == Some(data.mapping_hash.as_str());
            if !exact {
                return Err(KernelError::IntegrityFailed(
                    "journey validation run does not match active source, profile, and mapping authority"
                        .to_owned(),
                ));
            }
        }
        let validation_identity = validation
            .as_ref()
            .map(|summary| (summary.data.state.clone(), summary.data.result_hash.clone()));
        let validation_complete = state.validation_stage == "COMPLETE"
            && validation_identity
                .as_ref()
                .is_some_and(|(run_state, result_hash)| {
                    run_state == "COMPLETE" && result_hash.is_some()
                });
        if state.validation_stage == "COMPLETE" && !validation_complete {
            return Err(KernelError::IntegrityFailed(
                "journey validation binding is incomplete".to_owned(),
            ));
        }

        let entity_count = self.active_profile_entity_count(&state)?;
        let operator_governed_mapping = self
            .selected_pack_for_state(&state)?
            .is_some_and(|pack| pack.mapping_mode == MappingMode::OperatorGoverned);
        let multi_entity_gated = self.downstream_authority_gated_for_state(&state)?;
        if let Some(tour) = chain.active_tour.as_ref() {
            require_journey_tour_binding(
                &state,
                job_identity.as_ref(),
                manifest_hash.as_ref(),
                validation_identity.as_ref(),
                tour,
            )?;
        }
        if let Some(comparison) = chain.latest_comparison.as_ref()
            && (chain
                .active_tour
                .as_ref()
                .is_none_or(|tour| tour.tour_id != comparison.candidate_tour_id)
                || state.active.validation_run_id.as_deref()
                    != Some(comparison.candidate_validation_run_id.as_str()))
        {
            return Err(KernelError::IntegrityFailed(
                "journey comparison does not bind the active tour and validation".to_owned(),
            ));
        }
        let downstream = if validation_complete && !multi_entity_gated {
            DownstreamJourneyAuthority::Enabled
        } else if validation_complete && multi_entity_gated {
            DownstreamJourneyAuthority::MultiEntityLocked
        } else {
            DownstreamJourneyAuthority::Disabled
        };
        let surface = if downstream == DownstreamJourneyAuthority::Enabled {
            JourneySurface::Workbench
        } else {
            JourneySurface::Engagement
        };
        let active_tour = chain.active_tour.as_ref();
        let comparison = chain.latest_comparison.as_ref();
        Ok(derive_journey(
            JourneyDerivation {
                binding: JourneyBinding {
                    engagement_id: Some(state.engagement_id),
                    engagement_generation: Some(state.generation),
                    job_id: state.active.job_id,
                    source_hash: job_identity.map(|identity| identity.0),
                    manifest_hash,
                    profiling_run_id: state.active.profiling_run_id,
                    object_pack_id: state.active.object_pack_id,
                    destination_origin: state.active.destination_origin,
                    destination_authority_id: state.active.destination_authority_id,
                    destination_origin_hash: state.active.destination_origin_hash,
                    profile_id: state.active.profile_id,
                    profile_version: state.active.profile_version,
                    profile_hash: state.active.profile_hash,
                    mapping_id: state.active.mapping_id,
                    mapping_version: state.active.mapping_version,
                    mapping_hash: state.active.mapping_hash,
                    validation_run_id: state.active.validation_run_id,
                    validation_result_hash: validation_identity.and_then(|identity| identity.1),
                    active_tour_id: active_tour.map(|tour| tour.tour_id.clone()),
                    active_tour_number: active_tour.map(|tour| tour.tour_number),
                    active_tour_hash: active_tour.map(|tour| tour.tour_hash.clone()),
                    comparison_id: comparison.map(|value| value.comparison_id.clone()),
                    comparison_hash: comparison.map(|value| value.comparison_hash.clone()),
                    entity_count,
                },
                surface,
                engagement_exists: true,
                source_complete,
                destination_complete,
                mapping_complete,
                operator_governed_mapping,
                validation_complete,
                issues_available: state.phase == EngagementPhase::FirstIssuesReady,
                comparison_available: validation_complete,
                comparison_pending: chain.pending_attempt.is_some(),
                comparison_complete: comparison.is_some(),
                downstream,
                decisions_available: downstream == DownstreamJourneyAuthority::Enabled
                    && state.phase == EngagementPhase::FirstIssuesReady,
                reconciliation_available: downstream == DownstreamJourneyAuthority::Enabled,
                package_available: downstream == DownstreamJourneyAuthority::Enabled,
            },
            workspace,
        ))
    }

    /// Append one validated operator-authored draft revision with optimistic concurrency.
    #[allow(clippy::too_many_lines)]
    pub fn save_mapping_draft(
        &self,
        command: &SaveMappingDraftCommand,
    ) -> Result<SaveMappingDraftResult> {
        require_schema(&command.schema_version, SAVE_MAPPING_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_optional_draft_expectations(command)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let state = self.state()?;
        let pack = self.operator_pack_for_state(&state)?;
        let workspace = self.mapping_workspace()?;
        let revision_scoped = workspace.revision_draft_scope.is_some();
        let result_engagement_generation = command
            .expected_engagement_generation
            .saturating_add(u64::from(!revision_scoped));
        if let Some(attempt) = load_mapping_workspace_attempt(
            &self.open_read()?,
            &state.engagement_id,
            &command.idempotency_key,
            &request_hash,
            "SAVE_DRAFT",
        )? {
            return replay_saved_mapping_draft(
                &self.open_read()?,
                &attempt,
                result_engagement_generation,
            );
        }
        if state.generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "mapping draft save used a stale engagement generation".to_owned(),
            ));
        }
        let profile_model = mapping_workspace_model_from_view(&workspace);
        validate_mapping_draft_snapshot(&command.snapshot, &workspace, &profile_model, &pack)?;
        let snapshot_value = serde_json::to_value(&command.snapshot)?;
        let canonical_snapshot = canonical_json_string(&snapshot_value)?;
        if canonical_snapshot.len() > MAXIMUM_COMMAND_BYTES {
            return Err(KernelError::MappingInvalid(
                "mapping draft exceeds its canonical byte bound".to_owned(),
            ));
        }
        let draft_hash = canonical_hash(&snapshot_value)?;
        let author = command
            .snapshot
            .author
            .as_deref()
            .filter(|author| !author.trim().is_empty())
            .ok_or_else(|| {
                KernelError::MappingInvalid(
                    "operator-governed mapping draft requires an attributable author".to_owned(),
                )
            })?;
        validate_bounded_text(author, "mapping draft author", MAXIMUM_IDENTIFIER_BYTES)?;

        let writer = self.open_writer()?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        let active_generation: i64 = transaction.query_row(
            "SELECT generation FROM engagement_active_context
             WHERE engagement_id = ?1",
            [&state.engagement_id],
            |row| row.get(0),
        )?;
        if u64::try_from(active_generation).unwrap_or(u64::MAX)
            != command.expected_engagement_generation
        {
            return Err(KernelError::Conflict(
                "mapping draft save lost its engagement-generation race".to_owned(),
            ));
        }
        let existing = load_active_mapping_draft_head(
            &transaction,
            &state.engagement_id,
            &workspace.job_id,
            &workspace.source_hash,
            &workspace.object_pack_id,
            &workspace.object_pack_version,
            &workspace.profile_id,
            &workspace.profile_version,
            &workspace.profile_hash,
        )?;
        let (draft_id, draft_generation, parent_hash, expected_state) = if let Some(head) = existing
        {
            if head.state != "EDITABLE"
                || command.expected_draft_id.as_deref() != Some(head.draft_id.as_str())
                || command.expected_draft_generation != Some(head.generation)
                || command.expected_draft_hash.as_deref() != Some(head.head_revision_hash.as_str())
            {
                return Err(KernelError::Conflict(
                    "mapping draft save used a stale or non-editable draft head".to_owned(),
                ));
            }
            if head.generation >= MAXIMUM_MAPPING_DRAFT_REVISIONS {
                return Err(KernelError::MappingInvalid(format!(
                    "mapping draft reached its {MAXIMUM_MAPPING_DRAFT_REVISIONS}-revision bound"
                )));
            }
            (
                head.draft_id,
                head.generation + 1,
                Some(head.head_revision_hash),
                "EDITABLE",
            )
        } else {
            if command.expected_draft_id.is_some()
                || command.expected_draft_generation.is_some()
                || command.expected_draft_hash.is_some()
            {
                return Err(KernelError::Conflict(
                    "mapping draft expectations name a head that does not exist".to_owned(),
                ));
            }
            (Uuid::new_v4().to_string(), 1, None, "ABSENT")
        };
        if draft_generation == 1 {
            transaction.execute(
                "INSERT INTO engagement_mapping_draft_heads (
                     draft_id, engagement_id, source_job_id, source_hash,
                     object_pack_id, object_pack_version,
                     profile_id, profile_version, profile_hash,
                     generation, state, head_revision_hash,
                     created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                           1, 'EDITABLE', ?10, ?11, ?11)",
                params![
                    draft_id,
                    state.engagement_id,
                    workspace.job_id,
                    workspace.source_hash,
                    workspace.object_pack_id,
                    workspace.object_pack_version,
                    workspace.profile_id,
                    workspace.profile_version,
                    workspace.profile_hash,
                    draft_hash,
                    now,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO engagement_mapping_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                draft_id,
                i64::try_from(draft_generation).unwrap_or(i64::MAX),
                draft_hash,
                parent_hash,
                canonical_snapshot,
                author,
                now,
            ],
        )?;
        if draft_generation > 1 {
            transaction.execute(
                "UPDATE engagement_mapping_draft_heads
                 SET generation = ?1, head_revision_hash = ?2, updated_at = ?3
                 WHERE draft_id = ?4 AND state = 'EDITABLE'
                   AND generation = ?5 AND head_revision_hash = ?6",
                params![
                    i64::try_from(draft_generation).unwrap_or(i64::MAX),
                    draft_hash,
                    now,
                    draft_id,
                    i64::try_from(draft_generation - 1).unwrap_or(i64::MAX),
                    parent_hash,
                ],
            )?;
        }
        if !revision_scoped {
            let changed = transaction.execute(
                "UPDATE engagement_active_context
                 SET generation = generation + 1, updated_at = ?1
                 WHERE engagement_id = ?2 AND generation = ?3",
                params![
                    now,
                    state.engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?;
            if changed != 1 {
                return Err(KernelError::Conflict(
                    "mapping draft save lost its engagement-generation compare-and-swap".to_owned(),
                ));
            }
        }
        insert_mapping_workspace_attempt(
            &transaction,
            &state.engagement_id,
            &draft_id,
            "SAVE_DRAFT",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_draft_generation.unwrap_or(0),
            expected_state,
            draft_generation,
            "EDITABLE",
            &draft_id,
            &draft_hash,
            &now,
        )?;
        transaction.commit()?;
        Ok(SaveMappingDraftResult {
            schema_version: MAPPING_DRAFT_SCHEMA.to_owned(),
            engagement_generation: result_engagement_generation,
            draft: MappingDraftView {
                schema_version: MAPPING_DRAFT_SCHEMA.to_owned(),
                draft_id,
                draft_generation,
                draft_hash,
                state: "EDITABLE".to_owned(),
                snapshot: command.snapshot.clone(),
                submitted_mapping_id: None,
                submitted_mapping_version: None,
                submitted_mapping_hash: None,
                updated_at: now,
            },
            idempotent_replay: false,
        })
    }

    /// Freeze one exact draft generation into immutable, validated mapping evidence.
    #[allow(clippy::too_many_lines)]
    pub fn submit_mapping_draft(
        &self,
        command: &SubmitMappingDraftCommand,
    ) -> Result<SubmitMappingDraftResult> {
        require_schema(&command.schema_version, SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.draft_id, "mapping draft ID")?;
        validate_mapping_document_id(&command.mapping_id, "mapping ID")?;
        validate_operator_mapping_version(&command.version)?;
        validate_sha256(&command.expected_draft_hash, "mapping draft hash")?;
        if command.acknowledgement != OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT {
            return Err(KernelError::MappingInvalid(format!(
                "mapping submission requires acknowledgement {OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT:?}"
            )));
        }
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let state = self.state()?;
        let pack = self.operator_pack_for_state(&state)?;
        if let Some(attempt) = load_mapping_workspace_attempt(
            &self.open_read()?,
            &state.engagement_id,
            &command.idempotency_key,
            &request_hash,
            "SUBMIT_MAPPING",
        )? {
            return replay_submitted_mapping(
                &self.workspace,
                &self.open_read()?,
                &attempt,
                command.expected_engagement_generation.saturating_add(1),
            );
        }
        if state.generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "mapping submission used a stale engagement generation".to_owned(),
            ));
        }
        let workspace = self.mapping_workspace()?;
        if let Some(scope) = workspace.revision_draft_scope.as_ref() {
            return Err(KernelError::Conflict(format!(
                "mapping draft submission is unavailable for pending source revision {}: {}",
                scope.tour_number, scope.unavailable_reason
            )));
        }
        let draft = workspace.draft.as_ref().ok_or_else(|| {
            KernelError::Conflict("mapping submission requires a saved draft".to_owned())
        })?;
        if draft.state != "EDITABLE"
            || draft.draft_id != command.draft_id
            || draft.draft_generation != command.expected_draft_generation
            || draft.draft_hash != command.expected_draft_hash
        {
            return Err(KernelError::Conflict(
                "mapping submission does not match the exact editable draft head".to_owned(),
            ));
        }
        let profile_model = mapping_workspace_model_from_view(&workspace);
        validate_mapping_draft_snapshot(&draft.snapshot, &workspace, &profile_model, &pack)?;
        let document = MappingDocument {
            schema_version: crate::mapping::MAPPING_SCHEMA_VERSION,
            mapping_id: command.mapping_id.clone(),
            version: command.version.clone(),
            profile: MappingProfileIdentity {
                profile_id: workspace.profile_id.clone(),
                version: workspace.profile_version.clone(),
                sha256: workspace.profile_hash.clone(),
            },
            source: MappingSourceIdentity {
                package_sha256: workspace.source_hash.clone(),
                source_files: workspace
                    .source_files
                    .iter()
                    .map(|file| file.source_file.clone())
                    .collect(),
            },
            root_entity_id: pack.root_entity_id.clone(),
            source_entity_assignments: draft.snapshot.source_entity_assignments.clone(),
            bindings: draft.snapshot.bindings.clone(),
            reference_tables: draft.snapshot.reference_tables.clone(),
            relationship_bindings: draft.snapshot.relationship_bindings.clone(),
            author: draft.snapshot.author.clone(),
            reviewer: None,
        };
        let document_value = serde_json::to_value(&document)?;
        let document_json = canonical_json_string(&document_value)?;
        if document_json.len() > MAXIMUM_COMMAND_BYTES {
            return Err(KernelError::MappingInvalid(
                "submitted mapping exceeds its canonical byte bound".to_owned(),
            ));
        }
        let mapping_hash = canonical_hash(&document_value)?;
        let relative_path = format!(
            "engagement/mappings/operator-{}-{}.json",
            command.draft_id, mapping_hash
        );
        let relative_directory = Path::new(&relative_path).parent().ok_or_else(|| {
            KernelError::IntegrityFailed("submitted mapping has no relative parent".to_owned())
        })?;
        let mapping_path = checked_workspace_path(&self.workspace, &relative_path)?;
        let writer = self.open_writer()?;
        if let Some(attempt) = load_mapping_workspace_attempt(
            writer.connection(),
            &state.engagement_id,
            &command.idempotency_key,
            &request_hash,
            "SUBMIT_MAPPING",
        )? {
            drop(writer);
            let connection = self.open_read()?;
            return replay_submitted_mapping(
                &self.workspace,
                &connection,
                &attempt,
                command.expected_engagement_generation.saturating_add(1),
            );
        }
        require_active_generation(
            writer.connection(),
            &state.engagement_id,
            command.expected_engagement_generation,
            "mapping submission",
        )?;
        let locked_head = load_active_mapping_draft_head(
            writer.connection(),
            &state.engagement_id,
            &workspace.job_id,
            &workspace.source_hash,
            &workspace.object_pack_id,
            &workspace.object_pack_version,
            &workspace.profile_id,
            &workspace.profile_version,
            &workspace.profile_hash,
        )?
        .ok_or_else(|| KernelError::Conflict("mapping draft head vanished".to_owned()))?;
        if locked_head.state != "EDITABLE"
            || locked_head.draft_id != command.draft_id
            || locked_head.generation != command.expected_draft_generation
            || locked_head.head_revision_hash != command.expected_draft_hash
        {
            return Err(KernelError::Conflict(
                "mapping submission lost its draft-head compare-and-swap".to_owned(),
            ));
        }
        ensure_workspace_directory(&self.workspace, relative_directory)?;
        let file_disposition = write_exact_file(&mapping_path, document_json.as_bytes())?;
        let commit_result = (|| {
            let prepared = crate::mapping::prepare_mapping_validation_locked(
                &writer,
                &workspace.job_id,
                &self.mapping_schema_path,
                &self.profile_schema_path,
                &mapping_path,
            )?;
            let report = prepared.report().clone();
            if report.mapping_id != command.mapping_id
                || report.mapping_version != command.version
                || report.mapping_hash != mapping_hash
            {
                return Err(KernelError::IntegrityFailed(
                    "mapping registration returned a different immutable identity".to_owned(),
                ));
            }
            let now = now_rfc3339();
            let transaction = writer.connection().unchecked_transaction()?;
            require_active_generation(
                &transaction,
                &state.engagement_id,
                command.expected_engagement_generation,
                "mapping submission",
            )?;
            let head = load_active_mapping_draft_head(
                &transaction,
                &state.engagement_id,
                &workspace.job_id,
                &workspace.source_hash,
                &workspace.object_pack_id,
                &workspace.object_pack_version,
                &workspace.profile_id,
                &workspace.profile_version,
                &workspace.profile_hash,
            )?
            .ok_or_else(|| KernelError::Conflict("mapping draft head vanished".to_owned()))?;
            if head.state != "EDITABLE"
                || head.draft_id != command.draft_id
                || head.generation != command.expected_draft_generation
                || head.head_revision_hash != command.expected_draft_hash
            {
                return Err(KernelError::Conflict(
                    "mapping submission lost its draft-head compare-and-swap".to_owned(),
                ));
            }
            crate::mapping::register_prepared_mapping_validation(&transaction, &prepared)?;
            let existing_preparation = transaction
                .query_row(
                    "SELECT preparation_id, mapping_hash, document_json, job_id,
                            source_hash, profile_id, profile_version, profile_hash
                     FROM engagement_mapping_preparations
                     WHERE engagement_id = ?1 AND mapping_id = ?2 AND mapping_version = ?3",
                    params![state.engagement_id, command.mapping_id, command.version,],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, String>(7)?,
                        ))
                    },
                )
                .optional()?;
            let preparation_id = if let Some(existing) = existing_preparation {
                if existing.1 != mapping_hash
                    || existing.2 != document_json
                    || existing.3 != workspace.job_id
                    || existing.4 != workspace.source_hash
                    || existing.5 != workspace.profile_id
                    || existing.6 != workspace.profile_version
                    || existing.7 != workspace.profile_hash
                {
                    return Err(KernelError::Conflict(
                        "existing mapping preparation differs from the exact submitted mapping authority"
                            .to_owned(),
                    ));
                }
                existing.0
            } else {
                let preparation_id = Uuid::new_v4().to_string();
                let changed = transaction.execute(
                    "INSERT INTO engagement_mapping_preparations (
                         preparation_id, engagement_id, mapping_id, mapping_version,
                         mapping_hash, document_json, job_id, source_hash,
                         profile_id, profile_version, profile_hash, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        preparation_id,
                        state.engagement_id,
                        command.mapping_id,
                        command.version,
                        mapping_hash,
                        document_json,
                        workspace.job_id,
                        workspace.source_hash,
                        workspace.profile_id,
                        workspace.profile_version,
                        workspace.profile_hash,
                        now,
                    ],
                )?;
                if changed != 1 {
                    return Err(KernelError::IntegrityFailed(
                        "mapping submission did not persist exactly one preparation".to_owned(),
                    ));
                }
                preparation_id
            };
            let changed = transaction.execute(
                "UPDATE engagement_mapping_draft_heads
                 SET state = 'SUBMITTED', submitted_mapping_id = ?1,
                     submitted_mapping_version = ?2, submitted_mapping_hash = ?3,
                     submitted_at = ?4, updated_at = ?4
                 WHERE draft_id = ?5 AND generation = ?6
                   AND head_revision_hash = ?7 AND state = 'EDITABLE'",
                params![
                    command.mapping_id,
                    command.version,
                    mapping_hash,
                    now,
                    command.draft_id,
                    i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                    command.expected_draft_hash,
                ],
            )?;
            if changed != 1 {
                return Err(KernelError::Conflict(
                    "mapping submission could not freeze the exact draft head".to_owned(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE engagement_active_context
                 SET active_mapping_id = ?1, active_mapping_version = ?2,
                     active_mapping_hash = ?3, mapping_stage = 'PREPARED',
                     active_validation_run_id = NULL,
                     validation_stage = 'NOT_LAUNCHED',
                     generation = generation + 1, updated_at = ?4
                 WHERE engagement_id = ?5 AND generation = ?6",
                params![
                    command.mapping_id,
                    command.version,
                    mapping_hash,
                    now,
                    state.engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?;
            if changed != 1 {
                return Err(KernelError::Conflict(
                    "mapping submission lost its engagement-generation compare-and-swap".to_owned(),
                ));
            }
            insert_mapping_workspace_attempt(
                &transaction,
                &state.engagement_id,
                &command.draft_id,
                "SUBMIT_MAPPING",
                &command.idempotency_key,
                &request_hash,
                &canonical,
                command.expected_draft_generation,
                "EDITABLE",
                command.expected_draft_generation,
                "SUBMITTED",
                &preparation_id,
                &mapping_hash,
                &now,
            )?;
            transaction.commit()?;
            Ok((preparation_id, now, report.coverage))
        })();
        let (preparation_id, now, coverage) = match commit_result {
            Ok(result) => result,
            Err(error) => {
                if file_disposition == ExactFileDisposition::Created {
                    match fs::remove_file(&mapping_path) {
                        Ok(()) => {}
                        Err(cleanup_error)
                            if cleanup_error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(cleanup_error) => {
                            return Err(KernelError::IntegrityFailed(format!(
                                "mapping submission failed ({error}); newly created mapping file {} could not be removed: {cleanup_error}",
                                mapping_path.display()
                            )));
                        }
                    }
                }
                return Err(error);
            }
        };
        let mut submitted = draft.clone();
        "SUBMITTED".clone_into(&mut submitted.state);
        submitted.submitted_mapping_id = Some(command.mapping_id.clone());
        submitted.submitted_mapping_version = Some(command.version.clone());
        submitted.submitted_mapping_hash = Some(mapping_hash.clone());
        submitted.updated_at = now;
        Ok(SubmitMappingDraftResult {
            schema_version: SUBMITTED_MAPPING_SCHEMA.to_owned(),
            engagement_generation: command.expected_engagement_generation + 1,
            draft: submitted,
            preparation_id,
            mapping_hash,
            coverage,
            idempotent_replay: false,
        })
    }

    /// Approve one exact immutable submitted mapping hash with attribution.
    #[allow(clippy::too_many_lines)]
    pub fn approve_mapping_version(
        &self,
        command: &ApproveMappingVersionCommand,
    ) -> Result<MappingApprovalResult> {
        require_schema(
            &command.schema_version,
            APPROVE_MAPPING_VERSION_COMMAND_SCHEMA,
        )?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.draft_id, "mapping draft ID")?;
        validate_identifier(&command.expected_mapping_id, "mapping ID")?;
        validate_identifier(&command.expected_mapping_version, "mapping version")?;
        validate_sha256(&command.expected_draft_hash, "mapping draft hash")?;
        validate_sha256(&command.expected_mapping_hash, "mapping hash")?;
        validate_bounded_text(&command.actor, "mapping approver", MAXIMUM_IDENTIFIER_BYTES)?;
        validate_bounded_text(
            &command.reason,
            "mapping approval reason",
            MAXIMUM_REASON_BYTES,
        )?;
        if command.acknowledgement != OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT {
            return Err(KernelError::MappingInvalid(format!(
                "mapping approval requires acknowledgement {OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT:?}"
            )));
        }
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let state = self.state()?;
        self.operator_pack_for_state(&state)?;
        if let Some(attempt) = load_mapping_workspace_attempt(
            &self.open_read()?,
            &state.engagement_id,
            &command.idempotency_key,
            &request_hash,
            "APPROVE_MAPPING",
        )? {
            return replay_mapping_approval(
                &self.open_read()?,
                &attempt,
                command.expected_engagement_generation.saturating_add(1),
            );
        }
        if state.generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "mapping approval used a stale engagement generation".to_owned(),
            ));
        }
        let workspace = self.mapping_workspace()?;
        let draft = workspace.draft.as_ref().ok_or_else(|| {
            KernelError::Conflict("mapping approval requires a submitted draft".to_owned())
        })?;
        if draft.state != "SUBMITTED"
            || draft.draft_id != command.draft_id
            || draft.draft_generation != command.expected_draft_generation
            || draft.draft_hash != command.expected_draft_hash
            || draft.submitted_mapping_id.as_deref() != Some(command.expected_mapping_id.as_str())
            || draft.submitted_mapping_version.as_deref()
                != Some(command.expected_mapping_version.as_str())
            || draft.submitted_mapping_hash.as_deref()
                != Some(command.expected_mapping_hash.as_str())
        {
            return Err(KernelError::Conflict(
                "mapping approval does not match the exact submitted draft and mapping hash"
                    .to_owned(),
            ));
        }
        let approval_id = Uuid::new_v4().to_string();
        let attempt_id = Uuid::new_v4().to_string();
        let approved_at = now_rfc3339();
        let approval_value = json!({
            "schemaVersion": MAPPING_APPROVAL_SCHEMA,
            "approvalId": approval_id,
            "draftId": command.draft_id,
            "draftGeneration": command.expected_draft_generation,
            "draftHash": command.expected_draft_hash,
            "mappingId": command.expected_mapping_id,
            "mappingVersion": command.expected_mapping_version,
            "mappingHash": command.expected_mapping_hash,
            "actor": command.actor,
            "reason": command.reason,
            "acknowledgement": command.acknowledgement,
            "approvedAt": approved_at
        });
        let canonical_approval = canonical_json_string(&approval_value)?;
        let approval_hash = canonical_hash(&approval_value)?;
        let writer = self.open_writer()?;
        let transaction = writer.connection().unchecked_transaction()?;
        require_active_generation(
            &transaction,
            &state.engagement_id,
            command.expected_engagement_generation,
            "mapping approval",
        )?;
        insert_mapping_workspace_attempt_with_id(
            &transaction,
            &attempt_id,
            &state.engagement_id,
            &command.draft_id,
            "APPROVE_MAPPING",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_draft_generation,
            "SUBMITTED",
            command.expected_draft_generation,
            "APPROVED",
            &approval_id,
            &approval_hash,
            &approved_at,
        )?;
        transaction.execute(
            "INSERT INTO engagement_mapping_approvals (
                 approval_id, attempt_id, engagement_id, draft_id,
                 draft_generation, draft_revision_hash,
                 mapping_id, mapping_version, mapping_hash,
                 approver, reason, acknowledgement,
                 canonical_approval_json, approval_hash, approved_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                approval_id,
                attempt_id,
                state.engagement_id,
                command.draft_id,
                i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                command.expected_draft_hash,
                command.expected_mapping_id,
                command.expected_mapping_version,
                command.expected_mapping_hash,
                command.actor,
                command.reason,
                command.acknowledgement,
                canonical_approval,
                approval_hash,
                approved_at,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_mapping_draft_heads
             SET state = 'APPROVED', approved_at = ?1, updated_at = ?1
             WHERE draft_id = ?2 AND state = 'SUBMITTED'
               AND generation = ?3 AND head_revision_hash = ?4
               AND submitted_mapping_id = ?5
               AND submitted_mapping_version = ?6
               AND submitted_mapping_hash = ?7",
            params![
                approved_at,
                command.draft_id,
                i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                command.expected_draft_hash,
                command.expected_mapping_id,
                command.expected_mapping_version,
                command.expected_mapping_hash,
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "mapping approval lost its submitted-draft compare-and-swap".to_owned(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE engagement_active_context
             SET mapping_stage = 'VALIDATED', generation = generation + 1,
                 updated_at = ?1
             WHERE engagement_id = ?2 AND mapping_stage = 'PREPARED'
               AND generation = ?3
               AND active_mapping_id = ?4
               AND active_mapping_version = ?5
               AND active_mapping_hash = ?6",
            params![
                approved_at,
                state.engagement_id,
                i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                command.expected_mapping_id,
                command.expected_mapping_version,
                command.expected_mapping_hash,
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "mapping approval lost its active-mapping compare-and-swap".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(MappingApprovalResult {
            schema_version: MAPPING_APPROVAL_SCHEMA.to_owned(),
            engagement_generation: command.expected_engagement_generation + 1,
            approval_id,
            approval_hash,
            mapping_id: command.expected_mapping_id.clone(),
            mapping_version: command.expected_mapping_version.clone(),
            mapping_hash: command.expected_mapping_hash.clone(),
            actor: command.actor.clone(),
            reason: command.reason.clone(),
            approved_at,
            idempotent_replay: false,
        })
    }

    fn operator_pack_for_state(&self, state: &EngagementState) -> Result<ControlledObjectPack> {
        if state.profile_stage != "APPROVED" {
            return Err(KernelError::ProfileNotApproved(
                "mapping workspace requires separate exact-hash destination approval".to_owned(),
            ));
        }
        let pack = self.selected_pack_for_state(state)?.ok_or_else(|| {
            KernelError::Conflict("mapping workspace requires an approved destination".to_owned())
        })?;
        if pack.mapping_mode != MappingMode::OperatorGoverned {
            return Err(KernelError::Conflict(
                "selected object pack uses controlled suggestions, not the operator workspace"
                    .to_owned(),
            ));
        }
        Ok(pack)
    }

    /// Prepare, immutably persist, and immediately validate/register one mapping.
    #[allow(clippy::too_many_lines)]
    pub fn prepare_mapping(
        &self,
        command: &PrepareMappingCommand,
    ) -> Result<PreparedMappingResult> {
        require_schema(&command.schema_version, PREPARE_MAPPING_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.mapping_id, "mapping ID")?;
        validate_identifier(&command.version, "mapping version")?;
        if command.acknowledgement != ASSISTED_MAPPING_ACKNOWLEDGEMENT {
            return Err(KernelError::PolicyInvalid(format!(
                "assisted mapping requires acknowledgement {ASSISTED_MAPPING_ACKNOWLEDGEMENT:?}"
            )));
        }
        if command.selected_suggestion_ids.is_empty()
            || command.selected_suggestion_ids.len() > MAXIMUM_BINDINGS
        {
            return Err(KernelError::MappingInvalid(format!(
                "assisted mapping must select 1..={MAXIMUM_BINDINGS} controlled suggestions"
            )));
        }
        let selected_ids = command
            .selected_suggestion_ids
            .iter()
            .collect::<HashSet<_>>();
        if selected_ids.len() != command.selected_suggestion_ids.len() {
            return Err(KernelError::MappingInvalid(
                "assisted mapping suggestion IDs must be unique".to_owned(),
            ));
        }
        let canonical = canonical_command(command)?;
        let command_request_hash = sha256_hex(canonical.as_bytes());
        if self
            .object_packs
            .iter()
            .any(|pack| pack.reference_tables.len() > MAXIMUM_REFERENCE_TABLES)
        {
            return Err(KernelError::MappingInvalid(format!(
                "controlled object pack exceeds {MAXIMUM_REFERENCE_TABLES} reference tables"
            )));
        }
        let state = self.state()?;
        if state.profile_stage != "APPROVED" {
            return Err(KernelError::ProfileNotApproved(
                "mapping preparation requires explicit local profile approval".to_owned(),
            ));
        }
        {
            let connection = self.open_read()?;
            if let Some(preparation_id) = load_command_attempt(
                &connection,
                &state.engagement_id,
                "PREPARE_MAPPING",
                &command.idempotency_key,
                &command_request_hash,
            )? {
                let (document_json, mapping_hash, job_id, profile_hash): (
                    String,
                    String,
                    String,
                    String,
                ) = connection.query_row(
                    "SELECT document_json, mapping_hash, job_id, profile_hash
                     FROM engagement_mapping_preparations
                     WHERE preparation_id = ?1",
                    [&preparation_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                if state.active.job_id.as_deref() != Some(job_id.as_str())
                    || state.active.profile_hash.as_deref() != Some(profile_hash.as_str())
                {
                    return Err(KernelError::Conflict(
                        "assisted-mapping replay is no longer bound to the active source/profile"
                            .to_owned(),
                    ));
                }
                let document: MappingDocument = serde_json::from_str(&document_json)?;
                drop(connection);
                let validation = self.validate_mapping(&ValidateMappingCommand {
                    preparation_id: preparation_id.clone(),
                    expected_mapping_hash: mapping_hash.clone(),
                })?;
                return Ok(PreparedMappingResult {
                    schema_version: PREPARED_MAPPING_SCHEMA.to_owned(),
                    preparation_id,
                    mapping_hash,
                    document,
                    reused_immutable_preparation: true,
                    validation: Some(validation.report),
                });
            }
        }
        let job_id = state.active.job_id.as_deref().ok_or_else(|| {
            KernelError::Conflict("mapping preparation requires active intake".to_owned())
        })?;
        let (profile_id, profile_version, profile_hash) = selected_profile_identity(&state)?;
        let object_pack_id = state.active.object_pack_id.as_deref().ok_or_else(|| {
            KernelError::Conflict("mapping preparation requires an object pack".to_owned())
        })?;
        let pack = self
            .object_packs
            .iter()
            .find(|candidate| candidate.object_pack_id == object_pack_id)
            .ok_or_else(|| {
                KernelError::IntegrityFailed("selected object pack vanished".to_owned())
            })?;
        if pack.mapping_mode != MappingMode::ControlledSuggestions {
            return Err(KernelError::Conflict(
                "operator-governed packs must use the durable mapping workspace".to_owned(),
            ));
        }
        let mut suggestions_by_id = pack
            .mapping_suggestions
            .iter()
            .map(|suggestion| (suggestion.suggestion_id.as_str(), suggestion))
            .collect::<BTreeMap<_, _>>();
        let mut bindings = Vec::with_capacity(command.selected_suggestion_ids.len());
        for suggestion_id in &command.selected_suggestion_ids {
            validate_identifier(suggestion_id, "mapping suggestion ID")?;
            let suggestion = suggestions_by_id
                .remove(suggestion_id.as_str())
                .ok_or_else(|| {
                    KernelError::MappingInvalid(format!(
                        "unknown controlled mapping suggestion {suggestion_id:?}"
                    ))
                })?;
            bindings.push(MappingBinding {
                binding_id: suggestion.suggestion_id.clone(),
                target_entity_id: suggestion.target_entity_id.clone(),
                source_file: suggestion.source_file.clone(),
                source_column: suggestion.source_column.clone(),
                source_column_ordinal: suggestion.source_column_ordinal,
                target_field_id: suggestion.target_field_id.clone(),
                kind: suggestion.kind,
                constant: suggestion.constant.clone(),
                lookup_table_id: suggestion.lookup_table_id.clone(),
            });
        }
        let reference_entries =
            pack.reference_tables
                .iter()
                .try_fold(0_usize, |total, table| {
                    total.checked_add(table.values.len()).ok_or_else(|| {
                        KernelError::MappingInvalid("reference table count overflow".to_owned())
                    })
                })?;
        if reference_entries > self.validation_policy.maximum_lookup_entries {
            return Err(KernelError::MappingInvalid(format!(
                "controlled mapping has {reference_entries} reference entries; maximum is {}",
                self.validation_policy.maximum_lookup_entries
            )));
        }
        let connection = self.open_read()?;
        let source_hash: String = connection.query_row(
            "SELECT source_hash FROM jobs WHERE id = ?1 AND state = 'COMPLETE'",
            [job_id],
            |row| row.get(0),
        )?;
        let mut source_files_statement =
            connection.prepare("SELECT path FROM csv_files WHERE job_id = ?1 ORDER BY path")?;
        let source_files = source_files_statement
            .query_map([job_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(source_files_statement);
        drop(connection);

        let document = MappingDocument {
            schema_version: crate::mapping::MAPPING_SCHEMA_VERSION,
            mapping_id: command.mapping_id.clone(),
            version: command.version.clone(),
            profile: MappingProfileIdentity {
                profile_id: profile_id.to_owned(),
                version: profile_version.to_owned(),
                sha256: profile_hash.to_owned(),
            },
            source: MappingSourceIdentity {
                package_sha256: source_hash.clone(),
                source_files,
            },
            root_entity_id: pack.root_entity_id.clone(),
            source_entity_assignments: Vec::new(),
            bindings,
            reference_tables: pack.reference_tables.clone(),
            relationship_bindings: Vec::new(),
            author: command.author.clone(),
            reviewer: command.reviewer.clone(),
        };
        let value = serde_json::to_value(&document)?;
        let document_json = canonical_json_string(&value)?;
        if document_json.len() > MAXIMUM_COMMAND_BYTES {
            return Err(KernelError::MappingInvalid(format!(
                "canonical mapping document exceeds {MAXIMUM_COMMAND_BYTES} bytes"
            )));
        }
        let mapping_hash = canonical_hash(&value)?;
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        let existing: Option<(String, String)> = writer
            .connection()
            .query_row(
                "SELECT preparation_id, mapping_hash
                 FROM engagement_mapping_preparations
                 WHERE engagement_id = ?1 AND mapping_id = ?2 AND mapping_version = ?3",
                params![engagement_id, command.mapping_id, command.version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (preparation_id, reused_immutable_preparation) =
            if let Some((preparation_id, existing_hash)) = existing {
                if existing_hash != mapping_hash {
                    return Err(KernelError::MappingConflict(
                        "mapping identity/version already has different prepared bytes".to_owned(),
                    ));
                }
                (preparation_id, true)
            } else {
                let preparation_id = Uuid::new_v4().to_string();
                let now = now_rfc3339();
                writer.connection().execute(
                    "INSERT INTO engagement_mapping_preparations (
                         preparation_id, engagement_id, mapping_id, mapping_version,
                         mapping_hash, document_json, job_id, source_hash,
                         profile_id, profile_version, profile_hash, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        preparation_id,
                        engagement_id,
                        command.mapping_id,
                        command.version,
                        mapping_hash,
                        document_json,
                        job_id,
                        source_hash,
                        profile_id,
                        profile_version,
                        profile_hash,
                        now,
                    ],
                )?;
                writer.connection().execute(
                    "UPDATE engagement_active_context
                     SET active_mapping_id = ?1, active_mapping_version = ?2,
                         active_mapping_hash = ?3, mapping_stage = 'PREPARED',
                         active_validation_run_id = NULL,
                         validation_stage = 'NOT_LAUNCHED',
                         generation = generation + 1, updated_at = ?4
                     WHERE engagement_id = ?5",
                    params![
                        command.mapping_id,
                        command.version,
                        mapping_hash,
                        now,
                        engagement_id,
                    ],
                )?;
                (preparation_id, false)
            };
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        insert_command_attempt(
            &transaction,
            &engagement_id,
            "PREPARE_MAPPING",
            &command.idempotency_key,
            &command_request_hash,
            &canonical,
            Some(&preparation_id),
            &now,
        )?;
        transaction.commit()?;
        drop(writer);
        let validation = self.validate_mapping(&ValidateMappingCommand {
            preparation_id: preparation_id.clone(),
            expected_mapping_hash: mapping_hash.clone(),
        })?;
        Ok(PreparedMappingResult {
            schema_version: PREPARED_MAPPING_SCHEMA.to_owned(),
            preparation_id,
            mapping_hash,
            document,
            reused_immutable_preparation,
            validation: Some(validation.report),
        })
    }

    /// Validate/register one exact persisted mapping preparation.
    #[allow(clippy::too_many_lines)]
    pub fn validate_mapping(
        &self,
        command: &ValidateMappingCommand,
    ) -> Result<ValidatedMappingResult> {
        validate_identifier(&command.preparation_id, "mapping preparation ID")?;
        validate_sha256(&command.expected_mapping_hash, "prepared mapping hash")?;
        let writer = self.open_writer()?;
        let connection = writer.connection();
        let (engagement_id, mapping_id, mapping_version, mapping_hash, document_json, job_id): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT engagement_id, mapping_id, mapping_version, mapping_hash,
                        document_json, job_id
                 FROM engagement_mapping_preparations WHERE preparation_id = ?1",
                [&command.preparation_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::NotFound(format!("mapping preparation {}", command.preparation_id))
            })?;
        if mapping_hash != command.expected_mapping_hash
            || canonical_hash(&serde_json::from_str::<Value>(&document_json)?)? != mapping_hash
        {
            return Err(KernelError::Conflict(
                "prepared mapping hash does not match immutable bytes".to_owned(),
            ));
        }
        let existing: Option<(String, String, String)> = connection
            .query_row(
                "SELECT v.mapping_hash, v.attempt_id, a.attempt_hash
                 FROM mapping_versions v
                 JOIN mapping_validation_attempts a ON a.id = v.attempt_id
                 WHERE v.mapping_id = ?1 AND v.mapping_version = ?2",
                params![mapping_id, mapping_version],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((existing_hash, attempt_id, attempt_hash)) = existing {
            if existing_hash != mapping_hash {
                return Err(KernelError::MappingConflict(
                    "registered mapping identity has different immutable bytes".to_owned(),
                ));
            }
            let view =
                crate::mapping::inspect_mapping(&self.workspace, &mapping_id, &mapping_version)?;
            let state = self.state()?;
            if state.mapping_stage != "VALIDATED"
                || state.active.mapping_hash.as_deref() != Some(mapping_hash.as_str())
            {
                let now = now_rfc3339();
                writer.connection().execute(
                    "UPDATE engagement_active_context
                     SET active_mapping_id = ?1, active_mapping_version = ?2,
                         active_mapping_hash = ?3, mapping_stage = 'VALIDATED',
                         active_validation_run_id = NULL,
                         validation_stage = 'NOT_LAUNCHED',
                         generation = generation + 1, updated_at = ?4
                     WHERE engagement_id = ?5",
                    params![
                        mapping_id,
                        mapping_version,
                        mapping_hash,
                        now,
                        engagement_id
                    ],
                )?;
            }
            return Ok(ValidatedMappingResult {
                schema_version: VALIDATED_MAPPING_SCHEMA.to_owned(),
                preparation_id: command.preparation_id.clone(),
                report: MappingValidationReport {
                    schema_version: crate::mapping::MAPPING_SCHEMA_VERSION,
                    attempt_id,
                    attempt_hash,
                    state: "VALID".to_owned(),
                    mapping_id,
                    mapping_version,
                    mapping_hash,
                    coverage: view.coverage,
                    reused_immutable_version: true,
                },
            });
        }
        let relative_path = format!(
            "engagement/mappings/{}-{}.json",
            command.preparation_id, mapping_hash
        );
        let relative_directory = Path::new(&relative_path).parent().ok_or_else(|| {
            KernelError::IntegrityFailed("prepared mapping has no relative parent".to_owned())
        })?;
        let mapping_path = checked_workspace_path(&self.workspace, &relative_path)?;
        ensure_workspace_directory(&self.workspace, relative_directory)?;
        let file_disposition = write_exact_file(&mapping_path, document_json.as_bytes())?;
        let prepared = match crate::mapping::prepare_mapping_validation_locked(
            &writer,
            &job_id,
            &self.mapping_schema_path,
            &self.profile_schema_path,
            &mapping_path,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                if file_disposition == ExactFileDisposition::Created {
                    let _ = fs::remove_file(&mapping_path);
                }
                return Err(error);
            }
        };
        if prepared.report().mapping_id != mapping_id
            || prepared.report().mapping_version != mapping_version
            || prepared.report().mapping_hash != mapping_hash
        {
            if file_disposition == ExactFileDisposition::Created {
                let _ = fs::remove_file(&mapping_path);
            }
            return Err(KernelError::IntegrityFailed(
                "mapping registration returned a different immutable identity".to_owned(),
            ));
        }
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        if let Err(error) =
            crate::mapping::register_prepared_mapping_validation(&transaction, &prepared)
        {
            if file_disposition == ExactFileDisposition::Created {
                let _ = fs::remove_file(&mapping_path);
            }
            return Err(error);
        }
        let update = transaction.execute(
            "UPDATE engagement_active_context
             SET active_mapping_id = ?1, active_mapping_version = ?2,
                 active_mapping_hash = ?3, mapping_stage = 'VALIDATED',
                 active_validation_run_id = NULL,
                 validation_stage = 'NOT_LAUNCHED',
                 generation = generation + 1, updated_at = ?4
             WHERE engagement_id = ?5",
            params![
                mapping_id,
                mapping_version,
                mapping_hash,
                now,
                engagement_id
            ],
        );
        if let Err(error) = update {
            if file_disposition == ExactFileDisposition::Created {
                let _ = fs::remove_file(&mapping_path);
            }
            return Err(error.into());
        }
        if let Err(error) = transaction.commit() {
            if file_disposition == ExactFileDisposition::Created {
                let _ = fs::remove_file(&mapping_path);
            }
            return Err(error.into());
        }
        let report = prepared.into_report();
        Ok(ValidatedMappingResult {
            schema_version: VALIDATED_MAPPING_SCHEMA.to_owned(),
            preparation_id: command.preparation_id.clone(),
            report,
        })
    }

    /// Launch fixed-policy validation against exact active source/profile/mapping identities.
    #[allow(clippy::too_many_lines)]
    pub fn launch_validation(
        &self,
        command: &LaunchValidationCommand,
    ) -> Result<LaunchValidationResult> {
        require_schema(&command.schema_version, LAUNCH_VALIDATION_COMMAND_SCHEMA)?;
        validate_idempotency_key(&command.idempotency_key)?;
        validate_identifier(&command.expected_job_id, "validation job ID")?;
        validate_identifier(&command.expected_mapping_id, "validation mapping ID")?;
        validate_identifier(
            &command.expected_mapping_version,
            "validation mapping version",
        )?;
        validate_sha256(&command.expected_mapping_hash, "validation mapping hash")?;
        let state = self.state()?;
        let pack = self.selected_pack_for_state(&state)?.ok_or_else(|| {
            KernelError::Conflict(
                "validation requires one exact active destination authority".to_owned(),
            )
        })?;
        let mapping_mode = match pack.mapping_mode {
            MappingMode::ControlledSuggestions => "CONTROLLED_SUGGESTIONS",
            MappingMode::OperatorGoverned => "OPERATOR_GOVERNED",
        };
        let policy_hash = canonical_hash(&serde_json::to_value(&self.validation_policy)?)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = self.open_writer()?;
        let engagement_id = require_engagement_id(writer.connection())?;
        let transaction = writer.connection().unchecked_transaction()?;
        if let Some(existing) = load_validation_attempt_by_idempotency(
            &transaction,
            &engagement_id,
            &command.idempotency_key,
        )? {
            if existing.request_hash != request_hash {
                return Err(KernelError::Conflict(
                    "validation idempotency key was used for a different command".to_owned(),
                ));
            }
            verify_validation_attempt_authority(&transaction, &existing, false)?;
            let attempt_id = existing.attempt_id.clone();
            transaction.commit()?;
            drop(writer);
            if (existing.authority_schema == "EXACT_V13" && existing.attempt_state == "RESERVED")
                || (existing.authority_schema == "LEGACY_V9" && existing.run_id.is_none())
            {
                self.reconcile_validation_attempt(&attempt_id)?;
            }
            let replay = self.open_read()?;
            let replay = load_validation_attempt(&replay, &attempt_id)?;
            if replay.attempt_state == "FAILED" {
                return Err(KernelError::Conflict(format!(
                    "validation attempt failed before a run could be registered: {}",
                    replay.error_code.as_deref().unwrap_or("UNKNOWN")
                )));
            }
            let run_id = replay.run_id.ok_or_else(|| {
                KernelError::Conflict(
                    "validation attempt is reserved and has not registered its run yet".to_owned(),
                )
            })?;
            let status = validation_status(&self.workspace, &run_id)?;
            return Ok(LaunchValidationResult {
                schema_version: VALIDATION_RESULT_SCHEMA.to_owned(),
                status,
                report: None,
                reused_attempt: true,
            });
        }

        let current = load_engagement_state(&transaction)?;
        if current.generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(format!(
                "validation expected engagement generation {}; current generation is {}",
                command.expected_engagement_generation, current.generation
            )));
        }
        if current.profile_stage != "APPROVED" || current.mapping_stage != "VALIDATED" {
            return Err(KernelError::Conflict(
                "validation requires approved profile authority and a validated mapping".to_owned(),
            ));
        }
        if current.active.job_id.as_deref() != Some(command.expected_job_id.as_str())
            || current.active.mapping_id.as_deref() != Some(command.expected_mapping_id.as_str())
            || current.active.mapping_version.as_deref()
                != Some(command.expected_mapping_version.as_str())
            || current.active.mapping_hash.as_deref()
                != Some(command.expected_mapping_hash.as_str())
            || current.active != state.active
        {
            return Err(KernelError::Conflict(
                "validation request differs from the exact active source/profile/mapping context"
                    .to_owned(),
            ));
        }
        let (profile_id, profile_version, profile_hash) = selected_profile_identity(&current)?;
        let profile_view = crate::profile_registry::inspect_profile_connection(
            &transaction,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        if profile_view.lifecycle_status != ProfileLifecycleStatus::Approved
            || profile_view.authority != ProfileAuthority::LocalMappingValidation
            || !profile_view.can_start_new_runs
        {
            return Err(KernelError::ProfileNotApproved(
                "exact active profile does not grant local mapping/validation authority".to_owned(),
            ));
        }
        let profile_approval = profile_view.approval.ok_or_else(|| {
            KernelError::ProfileNotApproved(
                "exact active profile has no independently verified approval".to_owned(),
            )
        })?;
        let mapping = crate::mapping::load_stored_mapping(
            &transaction,
            &command.expected_mapping_id,
            &command.expected_mapping_version,
        )?;
        let source_hash: String = transaction
            .query_row(
                "SELECT source_hash FROM jobs
                 WHERE id = ?1 AND state = 'COMPLETE'",
                [&command.expected_job_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                KernelError::Conflict(
                    "validation source job is absent, incomplete, or lacks immutable identity"
                        .to_owned(),
                )
            })?;
        if mapping.mapping_hash != command.expected_mapping_hash
            || mapping.document.source.package_sha256 != source_hash
            || mapping.document.profile.profile_id != profile_id
            || mapping.document.profile.version != profile_version
            || mapping.document.profile.sha256 != profile_hash
        {
            return Err(KernelError::IntegrityFailed(
                "registered mapping differs from the exact active source/profile authority"
                    .to_owned(),
            ));
        }
        let mapping_approval = match pack.mapping_mode {
            MappingMode::ControlledSuggestions => None,
            MappingMode::OperatorGoverned => Some(
                verified_operator_mapping_approval_identity(
                    &transaction,
                    &engagement_id,
                    &command.expected_mapping_id,
                    &command.expected_mapping_version,
                    &command.expected_mapping_hash,
                )?
                .ok_or_else(|| {
                    KernelError::Conflict(
                        "operator-governed validation requires one exact attributable mapping-hash approval"
                            .to_owned(),
                    )
                })?,
            ),
        };
        if current.validation_stage != "NOT_LAUNCHED" {
            return Err(KernelError::Conflict(
                "validation cannot create a competing run for the current engagement context"
                    .to_owned(),
            ));
        }
        let attempt_id = Uuid::new_v4().to_string();
        let planned_run_id = Uuid::new_v4().to_string();
        let authority = ValidationAttemptAuthority {
            schema_version: VALIDATION_ATTEMPT_AUTHORITY_SCHEMA.to_owned(),
            attempt_id: attempt_id.clone(),
            engagement_id: engagement_id.clone(),
            expected_engagement_generation: command.expected_engagement_generation,
            planned_run_id: planned_run_id.clone(),
            request_hash: request_hash.clone(),
            job_id: command.expected_job_id.clone(),
            source_hash: source_hash.clone(),
            profile_id: profile_id.to_owned(),
            profile_version: profile_version.to_owned(),
            profile_hash: profile_hash.to_owned(),
            profile_approval_id: profile_approval.approval_id.clone(),
            profile_approval_hash: profile_approval.approval_hash.clone(),
            mapping_id: command.expected_mapping_id.clone(),
            mapping_version: command.expected_mapping_version.clone(),
            mapping_hash: command.expected_mapping_hash.clone(),
            mapping_mode: mapping_mode.to_owned(),
            mapping_approval_id: mapping_approval.as_ref().map(|approval| approval.0.clone()),
            mapping_approval_hash: mapping_approval.as_ref().map(|approval| approval.1.clone()),
            policy_hash: policy_hash.clone(),
        };
        let authority_value = serde_json::to_value(&authority)?;
        let canonical_authority_json = canonical_json_string(&authority_value)?;
        let authority_hash = canonical_hash(&authority_value)?;
        let now = now_rfc3339();
        let expected_generation =
            i64::try_from(command.expected_engagement_generation).map_err(|_| {
                KernelError::PolicyInvalid("engagement generation is too large".to_owned())
            })?;
        transaction.execute(
            "INSERT INTO engagement_validation_attempts (
                 attempt_id, engagement_id, idempotency_key, request_hash,
                 canonical_request_json, job_id, mapping_id, mapping_version,
                 mapping_hash, policy_hash, run_id, created_at, updated_at,
                 authority_schema, expected_engagement_generation, planned_run_id,
                 source_hash, profile_id, profile_version, profile_hash,
                 profile_approval_id, profile_approval_hash, mapping_mode,
                 mapping_approval_id, mapping_approval_hash,
                 canonical_authority_json, authority_hash, attempt_state, error_code
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?11,
                 'EXACT_V13', ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                 ?21, ?22, ?23, ?24, 'RESERVED', NULL
             )",
            params![
                attempt_id,
                engagement_id,
                command.idempotency_key,
                request_hash,
                canonical,
                command.expected_job_id,
                command.expected_mapping_id,
                command.expected_mapping_version,
                command.expected_mapping_hash,
                policy_hash,
                now,
                expected_generation,
                planned_run_id,
                source_hash,
                profile_id,
                profile_version,
                profile_hash,
                profile_approval.approval_id,
                profile_approval.approval_hash,
                mapping_mode,
                mapping_approval
                    .as_ref()
                    .map(|approval| approval.0.as_str()),
                mapping_approval
                    .as_ref()
                    .map(|approval| approval.1.as_str()),
                canonical_authority_json,
                authority_hash,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_active_context
             SET validation_stage = 'RUNNING',
                 generation = generation + 1, updated_at = ?1
             WHERE engagement_id = ?2
               AND generation = ?3
               AND profile_stage = 'APPROVED'
               AND mapping_stage = 'VALIDATED'
               AND validation_stage = 'NOT_LAUNCHED'
               AND active_job_id = ?4
               AND active_mapping_id = ?5
               AND active_mapping_version = ?6
               AND active_mapping_hash = ?7",
            params![
                now,
                engagement_id,
                expected_generation,
                command.expected_job_id,
                command.expected_mapping_id,
                command.expected_mapping_version,
                command.expected_mapping_hash,
            ],
        )?;
        require_one(changed, "reserve exact validation authority")?;
        transaction.commit()?;

        let report = match run_validation_with_reserved_writer(
            &writer,
            &planned_run_id,
            &command.expected_job_id,
            &command.expected_mapping_id,
            &command.expected_mapping_version,
            &self.profile_schema_path,
            &self.validation_policy,
        ) {
            Ok(report) => report,
            Err(error) => {
                drop(writer);
                self.reconcile_validation_attempt(&attempt_id)?;
                let still_reserved = {
                    let connection = self.open_read()?;
                    load_validation_attempt(&connection, &attempt_id)?.attempt_state == "RESERVED"
                };
                if still_reserved {
                    self.fail_validation_attempt(&attempt_id, error.code())?;
                }
                return Err(error);
            }
        };
        let status = validation_status(&self.workspace, &report.run_id)?;
        Self::persist_validation_result_with_writer(&writer, &attempt_id, &engagement_id, &status)?;
        Ok(LaunchValidationResult {
            schema_version: VALIDATION_RESULT_SCHEMA.to_owned(),
            status,
            report: Some(report),
            reused_attempt: false,
        })
    }

    /// Recover kernel work and reconcile engagement-owned lifecycle pointers.
    pub fn recover(&self) -> Result<EngagementRecoveryResult> {
        self.object_workspace.require_active()?;
        self.reconcile_preflight_files()?;
        self.destination.recover_artifact_attempts()?;
        let workspace = recover_workspace(&self.workspace)?;
        let connection = self.open_read()?;
        let attempt_ids = {
            let mut statement = connection.prepare(
                "SELECT attempt_id FROM engagement_validation_attempts
                 WHERE run_id IS NULL ORDER BY created_at, attempt_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        drop(connection);
        for attempt_id in attempt_ids {
            self.reconcile_validation_attempt(&attempt_id)?;
            let abandoned_exact_reservation = {
                let connection = self.open_read()?;
                let attempt = load_validation_attempt(&connection, &attempt_id)?;
                if attempt.authority_schema != "EXACT_V13"
                    || attempt.attempt_state != "RESERVED"
                    || attempt.run_id.is_some()
                {
                    false
                } else {
                    let planned_run_id = attempt.planned_run_id.as_deref().ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "exact validation reservation has no planned run identity".to_owned(),
                        )
                    })?;
                    !connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM validation_runs WHERE id = ?1)",
                        [planned_run_id],
                        |row| row.get::<_, bool>(0),
                    )?
                }
            };
            if abandoned_exact_reservation {
                self.fail_validation_attempt(&attempt_id, "INTERRUPTED_RECOVERED")?;
            }
        }
        Ok(EngagementRecoveryResult {
            schema_version: RECOVERY_SCHEMA.to_owned(),
            workspace,
            state: self.state()?,
        })
    }

    fn reconcile_preflight_files(&self) -> Result<()> {
        let connection = self.open_read()?;
        let preflight_ids = {
            let mut statement = connection.prepare(
                "SELECT preflight_id FROM engagement_source_preflights
                 ORDER BY created_at, preflight_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        drop(connection);
        for preflight_id in preflight_ids {
            let connection = self.open_read()?;
            let row = load_preflight(&connection, &preflight_id)?;
            let file_intent = load_preflight_file_intent(&connection, &preflight_id)?;
            let publish_intent = load_preflight_publish_intent(&connection, &preflight_id)?;
            drop(connection);
            match row.state.as_str() {
                "RECEIVING" => {
                    if let Some(intent) = publish_intent.as_ref() {
                        self.reconcile_published_preflight(&row, intent)?;
                    } else {
                        self.terminate_preflight_with_file(
                            &PreflightTerminalCommand {
                                preflight_id,
                                error_code: "INTERRUPTED_RECOVERED".to_owned(),
                                detail: "prior source preflight recovered after interruption"
                                    .to_owned(),
                            },
                            "INTERRUPTED",
                            None,
                        )?;
                    }
                }
                "READY" => {
                    self.verify_ready_preflight_file(&row)?;
                    self.cleanup_preflight_partial(&row, file_intent.as_ref(), None)?;
                }
                "REJECTED" | "INTERRUPTED" => {
                    self.cleanup_preflight_partial(&row, file_intent.as_ref(), None)?;
                }
                other => {
                    return Err(KernelError::IntegrityFailed(format!(
                        "preflight {preflight_id} has unsupported recovery state {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn terminate_preflight_with_file(
        &self,
        command: &PreflightTerminalCommand,
        terminal: &str,
        expected_file: Option<&File>,
    ) -> Result<SourcePreflightView> {
        validate_identifier(&command.preflight_id, "preflight ID")?;
        validate_identifier(&command.error_code, "preflight error code")?;
        validate_bounded_text(
            &command.detail,
            "preflight error detail",
            MAXIMUM_REASON_BYTES,
        )?;
        let writer = self.open_writer()?;
        let row = load_preflight(writer.connection(), &command.preflight_id)?;
        let publish_intent =
            load_preflight_publish_intent(writer.connection(), &command.preflight_id)?;
        if row.state == "RECEIVING"
            && let Some(intent) = publish_intent.as_ref()
        {
            if let Some(file) = expected_file {
                require_controlled_file_identity(file, &intent.partial_file_identity)?;
            }
            drop(writer);
            return self.reconcile_published_preflight(&row, intent);
        }
        if row.state == terminal {
            let intent = load_preflight_file_intent(writer.connection(), &row.preflight_id)?;
            drop(writer);
            self.cleanup_preflight_partial(&row, intent.as_ref(), expected_file)?;
            return preflight_view(row);
        }
        if row.state != "RECEIVING" {
            return Err(KernelError::Conflict(format!(
                "preflight {} cannot transition from {} to {terminal}",
                command.preflight_id, row.state
            )));
        }
        require_partial_preflight_path(&row)?;
        let intent = load_preflight_file_intent(writer.connection(), &row.preflight_id)?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE engagement_source_preflights
             SET state = ?1, error_code = ?2, error_message = ?3,
                 updated_at = ?4, completed_at = ?4
             WHERE preflight_id = ?5 AND state = 'RECEIVING'",
            params![
                terminal,
                command.error_code,
                command.detail,
                now,
                command.preflight_id
            ],
        )?;
        require_one(changed, "terminate source preflight")?;
        insert_preflight_event(
            &transaction,
            &command.preflight_id,
            terminal,
            Some(&command.error_code),
            Some(&command.detail),
            &now,
        )?;
        transaction.commit()?;
        drop(writer);
        self.cleanup_preflight_partial(&row, intent.as_ref(), expected_file)?;
        let writer = self.open_writer()?;
        preflight_view(load_preflight(writer.connection(), &command.preflight_id)?)
    }

    fn cleanup_preflight_partial(
        &self,
        row: &PreflightRow,
        intent: Option<&PreflightFileIntent>,
        expected_file: Option<&File>,
    ) -> Result<()> {
        require_partial_preflight_path(row)?;
        let Some(intent) = intent else {
            // A committed RECEIVING row can legitimately exist before create-new.
            // Without immutable file identity evidence, never claim or delete a
            // same-named replacement.
            return Ok(());
        };
        if intent.relative_path != row.partial_relative_path {
            return Err(KernelError::IntegrityFailed(
                "preflight cleanup intent differs from its controlled path".to_owned(),
            ));
        }
        if let Some(file) = expected_file {
            require_controlled_file_identity(file, &intent.file_identity)?;
        }
        let directory = controlled_directory(
            &self.workspace,
            self.workspace_handle.as_ref(),
            Path::new("engagement/preflights"),
            false,
        )?;
        remove_controlled_file_if_owned(
            &directory,
            &preflight_partial_name(row),
            &intent.file_identity,
        )?;
        self.verify_workspace_identity()
    }

    fn persist_validation_result(
        &self,
        attempt_id: &str,
        engagement_id: &str,
        status: &ValidationStatus,
    ) -> Result<()> {
        let writer = self.open_writer()?;
        Self::persist_validation_result_with_writer(&writer, attempt_id, engagement_id, status)
    }

    #[allow(clippy::too_many_lines)]
    fn persist_validation_result_with_writer(
        writer: &WorkspaceWriter,
        attempt_id: &str,
        engagement_id: &str,
        status: &ValidationStatus,
    ) -> Result<()> {
        let stage = validation_stage(status);
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        let attempt = load_validation_attempt(&transaction, attempt_id)?;
        if attempt.engagement_id != engagement_id {
            return Err(KernelError::IntegrityFailed(
                "validation attempt engagement differs from completion request".to_owned(),
            ));
        }
        if attempt.authority_schema == "EXACT_V13" {
            verify_validation_attempt_authority(&transaction, &attempt, true)?;
            let planned_run_id = attempt.planned_run_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "exact validation attempt has no planned run identity".to_owned(),
                )
            })?;
            if status.run_id != planned_run_id {
                return Err(KernelError::IntegrityFailed(
                    "validation status run differs from the immutable planned run".to_owned(),
                ));
            }
            if attempt.attempt_state == "COMPLETE" {
                if attempt.run_id.as_deref() != Some(planned_run_id) {
                    return Err(KernelError::IntegrityFailed(
                        "completed validation attempt differs from its planned run".to_owned(),
                    ));
                }
                return Ok(());
            }
            if attempt.attempt_state != "RESERVED" || attempt.run_id.is_some() {
                return Err(KernelError::Conflict(
                    "validation attempt is not an open exact reservation".to_owned(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE engagement_validation_attempts
                 SET run_id = ?1, attempt_state = 'COMPLETE', updated_at = ?2
                 WHERE attempt_id = ?3
                   AND authority_schema = 'EXACT_V13'
                   AND attempt_state = 'RESERVED'
                   AND run_id IS NULL
                   AND planned_run_id = ?1",
                params![status.run_id, now, attempt_id],
            )?;
            require_one(changed, "complete exact validation attempt")?;
            let reservation_generation = i64::try_from(
                attempt
                    .expected_engagement_generation
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "exact validation attempt lacks its expected generation".to_owned(),
                        )
                    })?
                    .checked_add(1)
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "validation reservation generation overflowed".to_owned(),
                        )
                    })?,
            )
            .map_err(|_| {
                KernelError::IntegrityFailed(
                    "validation reservation generation is outside SQLite bounds".to_owned(),
                )
            })?;
            let active_changed = transaction.execute(
                "UPDATE engagement_active_context
                 SET active_validation_run_id = ?1, validation_stage = ?2,
                     generation = generation + 1, updated_at = ?3
                 WHERE engagement_id = ?4
                   AND generation = ?5
                   AND validation_stage = 'RUNNING'
                   AND active_validation_run_id IS NULL
                   AND active_job_id = ?6
                   AND selected_profile_id = ?7
                   AND selected_profile_version = ?8
                   AND selected_profile_hash = ?9
                   AND active_mapping_id = ?10
                   AND active_mapping_version = ?11
                   AND active_mapping_hash = ?12",
                params![
                    status.run_id,
                    stage,
                    now,
                    engagement_id,
                    reservation_generation,
                    attempt.job_id,
                    attempt.profile_id,
                    attempt.profile_version,
                    attempt.profile_hash,
                    attempt.mapping_id,
                    attempt.mapping_version,
                    attempt.mapping_hash,
                ],
            )?;
            require_one(active_changed, "publish exact validation result")?;
        } else {
            let changed = transaction.execute(
                "UPDATE engagement_validation_attempts
                 SET run_id = ?1, updated_at = ?2
                 WHERE attempt_id = ?3
                   AND authority_schema = 'LEGACY_V9'
                   AND run_id IS NULL",
                params![status.run_id, now, attempt_id],
            )?;
            require_one(changed, "complete legacy validation attempt")?;
            transaction.execute(
                "UPDATE engagement_active_context
                 SET active_validation_run_id = ?1, validation_stage = ?2,
                     generation = generation + 1, updated_at = ?3
                 WHERE engagement_id = ?4",
                params![status.run_id, stage, now, engagement_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn reconcile_validation_attempt(&self, attempt_id: &str) -> Result<()> {
        let connection = self.open_read()?;
        let row = load_validation_attempt(&connection, attempt_id)?;
        if row.run_id.is_some() || row.attempt_state == "FAILED" {
            return Ok(());
        }
        let run_id: Option<String> = if row.authority_schema == "EXACT_V13" {
            verify_validation_attempt_authority(&connection, &row, true)?;
            let planned_run_id = row.planned_run_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "exact validation attempt has no planned run identity".to_owned(),
                )
            })?;
            connection
                .query_row(
                    "SELECT id FROM validation_runs WHERE id = ?1",
                    [planned_run_id],
                    |run| run.get(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    "SELECT id FROM validation_runs
                     WHERE job_id = ?1 AND mapping_id = ?2 AND mapping_version = ?3
                       AND created_at >= ?4
                     ORDER BY created_at, id LIMIT 1",
                    params![
                        row.job_id,
                        row.mapping_id,
                        row.mapping_version,
                        row.created_at
                    ],
                    |run| run.get(0),
                )
                .optional()?
        };
        drop(connection);
        if let Some(run_id) = run_id {
            let status = validation_status(&self.workspace, &run_id)?;
            if status.state != crate::validation::ValidationRunState::Running {
                self.persist_validation_result(attempt_id, &row.engagement_id, &status)?;
            }
        }
        Ok(())
    }

    fn fail_validation_attempt(&self, attempt_id: &str, error_code: &str) -> Result<()> {
        validate_identifier(error_code, "validation failure code")?;
        let writer = self.open_writer()?;
        let now = now_rfc3339();
        let transaction = writer.connection().unchecked_transaction()?;
        let attempt = load_validation_attempt(&transaction, attempt_id)?;
        if attempt.authority_schema != "EXACT_V13"
            || attempt.attempt_state != "RESERVED"
            || attempt.run_id.is_some()
        {
            return Ok(());
        }
        verify_validation_attempt_authority(&transaction, &attempt, true)?;
        let changed = transaction.execute(
            "UPDATE engagement_validation_attempts
             SET attempt_state = 'FAILED', error_code = ?1, updated_at = ?2
             WHERE attempt_id = ?3
               AND authority_schema = 'EXACT_V13'
               AND attempt_state = 'RESERVED'
               AND run_id IS NULL",
            params![error_code, now, attempt_id],
        )?;
        require_one(changed, "fail exact validation attempt")?;
        let reservation_generation = i64::try_from(
            attempt
                .expected_engagement_generation
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "exact validation attempt lacks its expected generation".to_owned(),
                    )
                })?
                .checked_add(1)
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "validation reservation generation overflowed".to_owned(),
                    )
                })?,
        )
        .map_err(|_| {
            KernelError::IntegrityFailed(
                "validation reservation generation is outside SQLite bounds".to_owned(),
            )
        })?;
        let active_changed = transaction.execute(
            "UPDATE engagement_active_context
             SET validation_stage = 'FAILED', generation = generation + 1,
                 updated_at = ?1
             WHERE engagement_id = ?2
               AND generation = ?3
               AND validation_stage = 'RUNNING'
               AND active_validation_run_id IS NULL
               AND active_job_id = ?4
               AND active_mapping_id = ?5
               AND active_mapping_version = ?6
               AND active_mapping_hash = ?7",
            params![
                now,
                attempt.engagement_id,
                reservation_generation,
                attempt.job_id,
                attempt.mapping_id,
                attempt.mapping_version,
                attempt.mapping_hash,
            ],
        )?;
        require_one(active_changed, "publish exact validation failure")?;
        transaction.commit()?;
        Ok(())
    }

    fn open_read(&self) -> Result<Connection> {
        self.verify_workspace_identity()?;
        self.object_workspace.require_active()?;
        let connection = open_read_only(&self.workspace.join(DATABASE_FILENAME))?;
        self.verify_workspace_identity()?;
        Ok(connection)
    }

    fn open_writer(&self) -> Result<WorkspaceWriter> {
        self.verify_workspace_identity()?;
        self.object_workspace.require_active()?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        self.verify_workspace_identity()?;
        writer.verify()?;
        Ok(writer)
    }

    fn verify_workspace_identity(&self) -> Result<()> {
        verify_retained_workspace_root(&self.workspace, self.workspace_handle.as_ref())
    }
}

fn load_validation_attempt_by_idempotency(
    connection: &Connection,
    engagement_id: &str,
    idempotency_key: &str,
) -> Result<Option<ValidationAttemptRow>> {
    let attempt_id: Option<String> = connection
        .query_row(
            "SELECT attempt_id FROM engagement_validation_attempts
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| row.get(0),
        )
        .optional()?;
    attempt_id
        .as_deref()
        .map(|attempt_id| load_validation_attempt(connection, attempt_id))
        .transpose()
}

#[allow(clippy::too_many_lines)]
fn load_validation_attempt(
    connection: &Connection,
    attempt_id: &str,
) -> Result<ValidationAttemptRow> {
    connection
        .query_row(
            "SELECT attempt_id, engagement_id, idempotency_key, request_hash,
                    canonical_request_json, job_id, mapping_id, mapping_version,
                    mapping_hash, policy_hash, run_id, authority_schema,
                    expected_engagement_generation, planned_run_id, source_hash,
                    profile_id, profile_version, profile_hash,
                    profile_approval_id, profile_approval_hash, mapping_mode,
                    mapping_approval_id, mapping_approval_hash,
                    canonical_authority_json, authority_hash, attempt_state,
                    error_code, created_at
             FROM engagement_validation_attempts WHERE attempt_id = ?1",
            [attempt_id],
            |row| {
                let expected_generation = row.get::<_, Option<i64>>(12)?;
                Ok(ValidationAttemptRow {
                    attempt_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    idempotency_key: row.get(2)?,
                    request_hash: row.get(3)?,
                    canonical_request_json: row.get(4)?,
                    job_id: row.get(5)?,
                    mapping_id: row.get(6)?,
                    mapping_version: row.get(7)?,
                    mapping_hash: row.get(8)?,
                    policy_hash: row.get(9)?,
                    run_id: row.get(10)?,
                    authority_schema: row.get(11)?,
                    expected_engagement_generation: expected_generation
                        .map(|generation| {
                            u64::try_from(generation).map_err(|_| {
                                rusqlite::Error::IntegralValueOutOfRange(12, generation)
                            })
                        })
                        .transpose()?,
                    planned_run_id: row.get(13)?,
                    source_hash: row.get(14)?,
                    profile_id: row.get(15)?,
                    profile_version: row.get(16)?,
                    profile_hash: row.get(17)?,
                    profile_approval_id: row.get(18)?,
                    profile_approval_hash: row.get(19)?,
                    mapping_mode: row.get(20)?,
                    mapping_approval_id: row.get(21)?,
                    mapping_approval_hash: row.get(22)?,
                    canonical_authority_json: row.get(23)?,
                    authority_hash: row.get(24)?,
                    attempt_state: row.get(25)?,
                    error_code: row.get(26)?,
                    created_at: row.get(27)?,
                })
            },
        )
        .map_err(Into::into)
}

#[allow(clippy::too_many_lines, clippy::type_complexity)]
fn verify_validation_attempt_authority(
    connection: &Connection,
    attempt: &ValidationAttemptRow,
    require_active_mapping_approval: bool,
) -> Result<()> {
    if attempt.authority_schema == "LEGACY_V9" {
        if attempt.attempt_state != "LEGACY"
            || sha256_hex(attempt.canonical_request_json.as_bytes()) != attempt.request_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "legacy validation attempt {} request evidence is invalid",
                attempt.attempt_id
            )));
        }
        return Ok(());
    }
    if attempt.authority_schema != "EXACT_V13" || attempt.attempt_state == "LEGACY" {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} has unsupported authority/state",
            attempt.attempt_id
        )));
    }
    let command_value = parse_unique_json(attempt.canonical_request_json.as_bytes())?;
    let command: LaunchValidationCommand = serde_json::from_value(command_value.clone())?;
    if canonical_json_string(&command_value)? != attempt.canonical_request_json
        || canonical_hash(&command_value)? != attempt.request_hash
        || command.schema_version != LAUNCH_VALIDATION_COMMAND_SCHEMA
        || command.idempotency_key != attempt.idempotency_key
        || command.expected_job_id != attempt.job_id
        || command.expected_mapping_id != attempt.mapping_id
        || command.expected_mapping_version != attempt.mapping_version
        || command.expected_mapping_hash != attempt.mapping_hash
        || Some(command.expected_engagement_generation) != attempt.expected_engagement_generation
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} canonical request differs from stored columns",
            attempt.attempt_id
        )));
    }
    let (
        Some(expected_engagement_generation),
        Some(planned_run_id),
        Some(source_hash),
        Some(profile_id),
        Some(profile_version),
        Some(profile_hash),
        Some(profile_approval_id),
        Some(profile_approval_hash),
        Some(mapping_mode),
        Some(canonical_authority_json),
        Some(authority_hash),
    ) = (
        attempt.expected_engagement_generation,
        attempt.planned_run_id.as_deref(),
        attempt.source_hash.as_deref(),
        attempt.profile_id.as_deref(),
        attempt.profile_version.as_deref(),
        attempt.profile_hash.as_deref(),
        attempt.profile_approval_id.as_deref(),
        attempt.profile_approval_hash.as_deref(),
        attempt.mapping_mode.as_deref(),
        attempt.canonical_authority_json.as_deref(),
        attempt.authority_hash.as_deref(),
    )
    else {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} lacks exact authority columns",
            attempt.attempt_id
        )));
    };
    let mapping_approval_shape_valid = match mapping_mode {
        "CONTROLLED_SUGGESTIONS" => {
            attempt.mapping_approval_id.is_none() && attempt.mapping_approval_hash.is_none()
        }
        "OPERATOR_GOVERNED" => {
            attempt.mapping_approval_id.is_some() && attempt.mapping_approval_hash.is_some()
        }
        _ => false,
    };
    if !mapping_approval_shape_valid {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} mapping approval shape is invalid",
            attempt.attempt_id
        )));
    }
    let authority = ValidationAttemptAuthority {
        schema_version: VALIDATION_ATTEMPT_AUTHORITY_SCHEMA.to_owned(),
        attempt_id: attempt.attempt_id.clone(),
        engagement_id: attempt.engagement_id.clone(),
        expected_engagement_generation,
        planned_run_id: planned_run_id.to_owned(),
        request_hash: attempt.request_hash.clone(),
        job_id: attempt.job_id.clone(),
        source_hash: source_hash.to_owned(),
        profile_id: profile_id.to_owned(),
        profile_version: profile_version.to_owned(),
        profile_hash: profile_hash.to_owned(),
        profile_approval_id: profile_approval_id.to_owned(),
        profile_approval_hash: profile_approval_hash.to_owned(),
        mapping_id: attempt.mapping_id.clone(),
        mapping_version: attempt.mapping_version.clone(),
        mapping_hash: attempt.mapping_hash.clone(),
        mapping_mode: mapping_mode.to_owned(),
        mapping_approval_id: attempt.mapping_approval_id.clone(),
        mapping_approval_hash: attempt.mapping_approval_hash.clone(),
        policy_hash: attempt.policy_hash.clone(),
    };
    let authority_value = serde_json::to_value(&authority)?;
    if canonical_json_string(&authority_value)? != canonical_authority_json
        || canonical_hash(&authority_value)? != authority_hash
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} canonical authority bytes/hash differ",
            attempt.attempt_id
        )));
    }
    let job: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT state, source_hash FROM jobs WHERE id = ?1",
            [&attempt.job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if job
        .as_ref()
        .map(|(state, hash)| (state.as_str(), hash.as_deref()))
        != Some(("COMPLETE", Some(source_hash)))
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} source authority no longer replays",
            attempt.attempt_id
        )));
    }
    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        profile_id,
        profile_version,
        profile_hash,
    )?;
    let approval = profile.approval.as_ref().ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "validation attempt {} profile approval is absent",
            attempt.attempt_id
        ))
    })?;
    if approval.approval_id != profile_approval_id
        || approval.approval_hash != profile_approval_hash
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} exact profile approval differs",
            attempt.attempt_id
        )));
    }
    let mapping = crate::mapping::load_stored_mapping(
        connection,
        &attempt.mapping_id,
        &attempt.mapping_version,
    )?;
    if mapping.mapping_hash != attempt.mapping_hash
        || mapping.document.source.package_sha256 != source_hash
        || mapping.document.profile.profile_id != profile_id
        || mapping.document.profile.version != profile_version
        || mapping.document.profile.sha256 != profile_hash
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation attempt {} exact mapping/source/profile differs",
            attempt.attempt_id
        )));
    }
    if mapping_mode == "OPERATOR_GOVERNED" {
        verify_operator_mapping_approval_identity(
            connection,
            &attempt.engagement_id,
            &attempt.mapping_id,
            &attempt.mapping_version,
            &attempt.mapping_hash,
            attempt.mapping_approval_id.as_deref().unwrap_or_default(),
            attempt.mapping_approval_hash.as_deref().unwrap_or_default(),
            require_active_mapping_approval,
        )?;
    }
    if let Some(run_id) = attempt.run_id.as_deref() {
        if run_id != planned_run_id {
            return Err(KernelError::IntegrityFailed(format!(
                "validation attempt {} result run differs from its plan",
                attempt.attempt_id
            )));
        }
        let run: Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        )> = connection
            .query_row(
                "SELECT job_id, source_hash, profile_id, profile_version,
                            profile_hash, mapping_id, mapping_version, mapping_hash
                     FROM validation_runs
                     WHERE id = ?1 AND policy_hash = ?2",
                params![run_id, attempt.policy_hash],
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
            .optional()?;
        let expected = (
            attempt.job_id.clone(),
            source_hash.to_owned(),
            profile_id.to_owned(),
            profile_version.to_owned(),
            profile_hash.to_owned(),
            attempt.mapping_id.clone(),
            attempt.mapping_version.clone(),
            attempt.mapping_hash.clone(),
        );
        if run != Some(expected) {
            return Err(KernelError::IntegrityFailed(format!(
                "validation attempt {} run authority differs",
                attempt.attempt_id
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn verify_validation_attempt_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    for (name, expected_hash) in [
        (
            "engagement_validation_attempts_exact_insert_guard",
            "f75d12cf1ea64d146bea9e891618ae6bd8c87be1cc40a229f41923e7e5158257",
        ),
        (
            "engagement_validation_attempts_transition_guard",
            "fcfcf235465edfb824f123a3c14139855cb589b7d09206b38de6465cdc0bcea5",
        ),
        (
            "engagement_validation_attempts_no_delete",
            "d9eb006a6ea48de77974ad8bf963d4a230a434b16910c1c4343c492772e492a5",
        ),
    ] {
        let sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1
                   AND tbl_name = 'engagement_validation_attempts'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        let actual_hash = sql.as_deref().map(|sql| {
            sha256_hex(
                sql.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .as_bytes(),
            )
        });
        if actual_hash.as_deref() != Some(expected_hash) {
            issues.push(format!(
                "engagement validation-attempt guard {name} is missing or altered"
            ));
        }
    }
    let mut statement = connection.prepare(
        "SELECT attempt_id FROM engagement_validation_attempts
         ORDER BY created_at, attempt_id",
    )?;
    let attempt_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for attempt_id in attempt_ids {
        match load_validation_attempt(connection, &attempt_id).and_then(|attempt| {
            verify_validation_attempt_authority(connection, &attempt, false)?;
            if attempt.authority_schema == "EXACT_V13" {
                match attempt.attempt_state.as_str() {
                    "RESERVED" if attempt.run_id.is_none() && attempt.error_code.is_none() => {
                        let generation = attempt
                            .expected_engagement_generation
                            .and_then(|generation| generation.checked_add(1))
                            .and_then(|generation| i64::try_from(generation).ok())
                            .ok_or_else(|| {
                                KernelError::IntegrityFailed(
                                    "reserved validation attempt generation is invalid".to_owned(),
                                )
                            })?;
                        let active: bool = connection.query_row(
                            "SELECT EXISTS(
                                 SELECT 1 FROM engagement_active_context
                                 WHERE engagement_id = ?1
                                   AND generation = ?2
                                   AND validation_stage = 'RUNNING'
                                   AND active_validation_run_id IS NULL
                                   AND active_job_id = ?3
                                   AND active_mapping_id = ?4
                                   AND active_mapping_version = ?5
                                   AND active_mapping_hash = ?6
                             )",
                            params![
                                attempt.engagement_id,
                                generation,
                                attempt.job_id,
                                attempt.mapping_id,
                                attempt.mapping_version,
                                attempt.mapping_hash,
                            ],
                            |row| row.get(0),
                        )?;
                        if !active {
                            return Err(KernelError::IntegrityFailed(
                                "reserved validation attempt differs from its active CAS reservation"
                                    .to_owned(),
                            ));
                        }
                    }
                    "COMPLETE" if attempt.run_id.is_some() && attempt.error_code.is_none() => {}
                    "FAILED" if attempt.run_id.is_none() && attempt.error_code.is_some() => {}
                    state => {
                        return Err(KernelError::IntegrityFailed(format!(
                            "validation attempt has inconsistent terminal shape {state:?}"
                        )));
                    }
                }
            }
            Ok(())
        }) {
            Ok(()) => {}
            Err(error) => issues.push(format!(
                "engagement validation attempt {attempt_id} integrity replay failed: {error}"
            )),
        }
    }
    Ok(())
}

fn require_schema(actual: &str, expected: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(KernelError::PolicyInvalid(format!(
            "engagement command schema must be {expected:?}; found {actual:?}"
        )))
    }
}

fn canonical_command<T: Serialize>(command: &T) -> Result<String> {
    let value = serde_json::to_value(command)?;
    let canonical = canonical_json_string(&value)?;
    if canonical.len() > MAXIMUM_COMMAND_BYTES {
        return Err(KernelError::PolicyInvalid(format!(
            "engagement command exceeds {MAXIMUM_COMMAND_BYTES} canonical bytes"
        )));
    }
    Ok(canonical)
}

fn validate_optional_draft_expectations(command: &SaveMappingDraftCommand) -> Result<()> {
    let all_absent = command.expected_draft_id.is_none()
        && command.expected_draft_generation.is_none()
        && command.expected_draft_hash.is_none();
    let all_present = command.expected_draft_id.is_some()
        && command.expected_draft_generation.is_some()
        && command.expected_draft_hash.is_some();
    if !all_absent && !all_present {
        return Err(KernelError::MappingInvalid(
            "mapping draft expectations must be wholly absent or wholly present".to_owned(),
        ));
    }
    if let Some(draft_id) = &command.expected_draft_id {
        validate_identifier(draft_id, "expected mapping draft ID")?;
    }
    if let Some(generation) = command.expected_draft_generation
        && generation == 0
    {
        return Err(KernelError::MappingInvalid(
            "expected mapping draft generation must be positive".to_owned(),
        ));
    }
    if let Some(hash) = &command.expected_draft_hash {
        validate_sha256(hash, "expected mapping draft hash")?;
    }
    Ok(())
}

fn mapping_workspace_source_files(
    connection: &Connection,
    job_id: &str,
) -> Result<Vec<MappingWorkspaceSourceFileView>> {
    let mut statement = connection.prepare(
        "SELECT path, header_json FROM csv_files
         WHERE job_id = ?1 ORDER BY path",
    )?;
    let rows = statement.query_map([job_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut files = Vec::new();
    for row in rows {
        let (source_file, header_json) = row?;
        let header_json = header_json.ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "accepted CSV {source_file:?} has no durable header"
            ))
        })?;
        let headers: Vec<String> = serde_json::from_str(&header_json)?;
        let columns = headers
            .into_iter()
            .enumerate()
            .map(|(source_column_ordinal, source_column)| SourceColumnView {
                source_file: source_file.clone(),
                source_column,
                source_column_ordinal,
            })
            .collect();
        files.push(MappingWorkspaceSourceFileView {
            source_file,
            columns,
        });
    }
    if files.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "mapping workspace has no accepted CSV files".to_owned(),
        ));
    }
    Ok(files)
}

#[allow(clippy::too_many_lines)]
fn mapping_workspace_profile(document: &Value) -> Result<MappingWorkspaceProfileModel> {
    let entities_value = document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("profile entities vanished".to_owned()))?;
    let mut entities = Vec::with_capacity(entities_value.len());
    let mut fields = HashMap::new();
    for entity in entities_value {
        let entity_id = required_profile_string(entity, "entityId")?;
        let root = entity.get("root").and_then(Value::as_bool).ok_or_else(|| {
            KernelError::IntegrityFailed("profile entity root vanished".to_owned())
        })?;
        let key = entity
            .get("key")
            .and_then(Value::as_array)
            .ok_or_else(|| KernelError::IntegrityFailed("profile entity key vanished".to_owned()))?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    KernelError::IntegrityFailed("profile key is not a string".to_owned())
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let fields_value = entity
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::IntegrityFailed("profile entity fields vanished".to_owned())
            })?;
        let mut entity_fields = Vec::with_capacity(fields_value.len());
        for field in fields_value {
            let field_id = required_profile_string(field, "fieldId")?;
            let view = ProfileFieldView {
                entity_id: entity_id.clone(),
                field_id: field_id.clone(),
                label: field
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(&field_id)
                    .to_owned(),
                logical_type: required_profile_string(field, "logicalType")?,
                required: field
                    .get("required")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "profile field required flag vanished".to_owned(),
                        )
                    })?,
                sensitive: field
                    .get("sensitive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            if fields
                .insert((entity_id.clone(), field_id), view.clone())
                .is_some()
            {
                return Err(KernelError::IntegrityFailed(
                    "profile contains a duplicate qualified field".to_owned(),
                ));
            }
            entity_fields.push(view);
        }
        entities.push(MappingWorkspaceEntityView {
            entity_id,
            root,
            key,
            fields: entity_fields,
        });
    }
    let relationships_value = document
        .get("relationships")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("profile relationships vanished".to_owned()))?;
    let mut relationships = Vec::with_capacity(relationships_value.len());
    for relationship in relationships_value {
        let Some(relationship_id) = relationship.get("relationshipId").and_then(Value::as_str)
        else {
            continue;
        };
        let from_entity_id = required_profile_string(relationship, "from")?;
        let to_entity_id = required_profile_string(relationship, "to")?;
        let cardinality = required_profile_string(relationship, "cardinality")?;
        let key_pairs = relationship
            .get("keyPairs")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "keyed relationship {relationship_id:?} lost its key pairs"
                ))
            })?
            .iter()
            .map(|pair| {
                Ok(crate::mapping::RelationshipFieldPair {
                    from_field_id: required_profile_string(pair, "fromFieldId")?,
                    to_field_id: required_profile_string(pair, "toFieldId")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        relationships.push(MappingWorkspaceRelationshipView {
            relationship_id: relationship_id.to_owned(),
            from_entity_id,
            to_entity_id,
            cardinality,
            key_pairs,
        });
    }
    let (_, rules) = profile_fields_and_rules(document)?;
    let reference_table_obligations = mapping_reference_table_obligations(&rules, &fields)?;
    Ok(MappingWorkspaceProfileModel {
        entities,
        relationships,
        fields,
        rules,
        reference_table_obligations,
    })
}

fn mapping_reference_table_obligations(
    rules: &[ProfileRuleView],
    fields: &HashMap<(String, String), ProfileFieldView>,
) -> Result<Vec<MappingReferenceTableObligationView>> {
    let mut obligations =
        BTreeMap::<(String, String, String), (BTreeSet<String>, BTreeSet<String>)>::new();
    let mut versions = HashMap::<(String, String), String>::new();
    for rule in rules.iter().filter(|rule| rule.kind == "reference_lookup") {
        let (entity_id, field_id) = rule.target.split_once('.').ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "reference rule {:?} lost its qualified target",
                rule.rule_id
            ))
        })?;
        if !fields.contains_key(&(entity_id.to_owned(), field_id.to_owned())) {
            return Err(KernelError::IntegrityFailed(format!(
                "reference rule {:?} targets an absent active field",
                rule.rule_id
            )));
        }
        let table_id = rule
            .parameters
            .get("tableId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "reference rule {:?} lost tableId",
                    rule.rule_id
                ))
            })?;
        let table_version = rule
            .parameters
            .get("tableVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "reference rule {:?} lost tableVersion",
                    rule.rule_id
                ))
            })?;
        let identity = (entity_id.to_owned(), table_id.to_owned());
        if versions
            .insert(identity.clone(), table_version.to_owned())
            .is_some_and(|existing| existing != table_version)
        {
            return Err(KernelError::IntegrityFailed(format!(
                "profile declares conflicting versions for reference table {entity_id}.{table_id}"
            )));
        }
        let entry = obligations
            .entry((
                entity_id.to_owned(),
                table_id.to_owned(),
                table_version.to_owned(),
            ))
            .or_default();
        entry.0.insert(rule.rule_id.clone());
        entry.1.insert(rule.target.clone());
    }
    Ok(obligations
        .into_iter()
        .map(
            |((entity_id, table_id, table_version), (rule_ids, target_fields))| {
                MappingReferenceTableObligationView {
                    entity_id,
                    table_id,
                    table_version,
                    rule_ids: rule_ids.into_iter().collect(),
                    target_fields: target_fields.into_iter().collect(),
                }
            },
        )
        .collect())
}

fn mapping_workspace_model_from_view(
    workspace: &MappingWorkspaceView,
) -> MappingWorkspaceProfileModel {
    let fields = workspace
        .entities
        .iter()
        .flat_map(|entity| {
            entity
                .fields
                .iter()
                .cloned()
                .map(|field| ((field.entity_id.clone(), field.field_id.clone()), field))
        })
        .collect();
    MappingWorkspaceProfileModel {
        entities: workspace.entities.clone(),
        relationships: workspace.relationships.clone(),
        fields,
        rules: Vec::new(),
        reference_table_obligations: workspace.reference_table_obligations.clone(),
    }
}

#[allow(clippy::too_many_lines)]
fn derive_mapping_contract_coverage(
    authority: MappingContractCoverageAuthority,
    workspace: &MappingWorkspaceView,
    profile: &MappingWorkspaceProfileModel,
) -> Result<MappingContractCoverageView> {
    let snapshot = workspace.draft.as_ref().map(|draft| &draft.snapshot);
    let bindings = snapshot
        .map(|snapshot| {
            snapshot
                .bindings
                .iter()
                .map(|binding| {
                    (
                        (
                            binding.target_entity_id.as_deref().unwrap_or(""),
                            binding.target_field_id.as_str(),
                        ),
                        binding,
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let tables = snapshot
        .map(|snapshot| {
            snapshot
                .reference_tables
                .iter()
                .filter_map(|table| {
                    Some((
                        (table.entity_id.as_deref()?, table.table_id.as_str()),
                        table,
                    ))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let routes = snapshot
        .map(|snapshot| {
            snapshot
                .source_entity_assignments
                .iter()
                .map(|assignment| {
                    (
                        assignment.source_file.as_str(),
                        assignment.entity_id.as_str(),
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let accepted_columns = workspace
        .source_files
        .iter()
        .flat_map(|file| {
            file.columns.iter().map(|column| {
                (
                    column.source_file.as_str(),
                    column.source_column.as_str(),
                    column.source_column_ordinal,
                )
            })
        })
        .collect::<HashSet<_>>();
    let rules_by_target = profile.rules.iter().fold(
        HashMap::<&str, Vec<&ProfileRuleView>>::new(),
        |mut map, rule| {
            map.entry(rule.target.as_str()).or_default().push(rule);
            map
        },
    );

    let mut fields = Vec::with_capacity(profile.fields.len());
    for entity in &profile.entities {
        for field in &entity.fields {
            let target = (entity.entity_id.as_str(), field.field_id.as_str());
            let qualified = format!("{}.{}", entity.entity_id, field.field_id);
            let binding = bindings.get(&target).copied();
            let mut reasons = Vec::new();
            let mut status = if snapshot.is_some() {
                MappingContractCoverageStatus::Complete
            } else {
                reasons.push("No durable mapping draft exists.".to_owned());
                MappingContractCoverageStatus::Unresolved
            };
            let mut fixed_value_configured = false;
            let mut lookup_table_id = None;
            let mut lookup_table_version = None;
            if let Some(binding) = binding {
                let source_complete = match (
                    binding.source_file.as_deref(),
                    binding.source_column.as_deref(),
                    binding.source_column_ordinal,
                ) {
                    (Some(file), Some(column), Some(ordinal)) => {
                        accepted_columns.contains(&(file, column, ordinal))
                            && routes.get(file).copied() == Some(entity.entity_id.as_str())
                    }
                    (None, None, None) => false,
                    _ => {
                        status = MappingContractCoverageStatus::Blocker;
                        reasons.push(
                            "The source identity is incomplete or outside the active entity route."
                                .to_owned(),
                        );
                        false
                    }
                };
                match binding.kind {
                    MappingKind::Constant => {
                        fixed_value_configured = binding.constant.is_some();
                        if !fixed_value_configured
                            || binding.source_file.is_some()
                            || binding.source_column.is_some()
                            || binding.source_column_ordinal.is_some()
                            || binding.lookup_table_id.is_some()
                        {
                            status = MappingContractCoverageStatus::Blocker;
                            reasons.push(
                                "The fixed-value binding has inconsistent parameters.".to_owned(),
                            );
                        }
                    }
                    MappingKind::Lookup => {
                        if !source_complete {
                            status = MappingContractCoverageStatus::Blocker;
                            reasons.push(
                                "The exact lookup has no valid accepted source identity."
                                    .to_owned(),
                            );
                        }
                        lookup_table_id.clone_from(&binding.lookup_table_id);
                        if let Some(table_id) = binding.lookup_table_id.as_deref() {
                            if let Some(table) = tables.get(&(entity.entity_id.as_str(), table_id))
                            {
                                lookup_table_version = Some(table.version.clone());
                                if table.values.is_empty() {
                                    status = MappingContractCoverageStatus::Blocker;
                                    reasons.push(
                                        "The exact lookup table has no governed entries."
                                            .to_owned(),
                                    );
                                }
                            } else {
                                status = MappingContractCoverageStatus::Blocker;
                                reasons.push(
                                    "The exact entity-scoped lookup table is absent.".to_owned(),
                                );
                            }
                        } else {
                            status = MappingContractCoverageStatus::Blocker;
                            reasons.push("The exact lookup has no table identity.".to_owned());
                        }
                    }
                    MappingKind::Direct
                    | MappingKind::Trim
                    | MappingKind::EmptyAsNull
                    | MappingKind::UppercaseAscii
                    | MappingKind::LowercaseAscii => {
                        if !source_complete
                            || binding.constant.is_some()
                            || binding.lookup_table_id.is_some()
                        {
                            status = MappingContractCoverageStatus::Blocker;
                            reasons.push(
                                "The bounded source operation has inconsistent parameters."
                                    .to_owned(),
                            );
                        }
                    }
                }
            } else if snapshot.is_some() {
                status = MappingContractCoverageStatus::Unresolved;
                reasons.push("No durable binding covers this active destination field.".to_owned());
            }
            let field_rules = rules_by_target
                .get(qualified.as_str())
                .cloned()
                .unwrap_or_default();
            fields.push(MappingContractFieldCoverage {
                entity_id: entity.entity_id.clone(),
                field_id: field.field_id.clone(),
                binding_id: binding.map(|binding| binding.binding_id.clone()),
                operation: binding.map(|binding| binding.kind),
                source_file: binding.and_then(|binding| binding.source_file.clone()),
                source_column: binding.and_then(|binding| binding.source_column.clone()),
                source_column_ordinal: binding.and_then(|binding| binding.source_column_ordinal),
                fixed_value_configured,
                lookup_table_id,
                lookup_table_version,
                rule_ids: field_rules
                    .iter()
                    .map(|rule| rule.rule_id.clone())
                    .collect(),
                rule_kinds: field_rules.iter().map(|rule| rule.kind.clone()).collect(),
                status,
                reasons,
            });
        }
    }

    let field_status = fields
        .iter()
        .map(|field| {
            (
                (field.entity_id.as_str(), field.field_id.as_str()),
                field.status,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut rules = Vec::with_capacity(profile.rules.len());
    for rule in &profile.rules {
        let (entity_id, field_id) = rule.target.split_once('.').ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "profile rule {:?} lost its qualified target",
                rule.rule_id
            ))
        })?;
        let mut status = field_status
            .get(&(entity_id, field_id))
            .copied()
            .unwrap_or(MappingContractCoverageStatus::Blocker);
        let mut reasons = Vec::new();
        let mut reference_table_id = None;
        let mut reference_table_version = None;
        if snapshot.is_none() {
            status = MappingContractCoverageStatus::Unresolved;
            reasons.push("No durable mapping draft can satisfy this rule yet.".to_owned());
        } else if rule.kind == "conditional_required" {
            let when = rule
                .parameters
                .get("whenField")
                .and_then(Value::as_str)
                .and_then(|value| value.split_once('.'));
            match when {
                Some((when_entity, when_field))
                    if when_entity == entity_id
                        && field_status.get(&(when_entity, when_field))
                            == Some(&MappingContractCoverageStatus::Complete) => {}
                _ => {
                    status = MappingContractCoverageStatus::Blocker;
                    reasons.push(
                        "The same-entity condition field has no complete durable binding."
                            .to_owned(),
                    );
                }
            }
        } else if rule.kind == "reference_lookup" {
            let table_id = rule.parameters.get("tableId").and_then(Value::as_str);
            let table_version = rule.parameters.get("tableVersion").and_then(Value::as_str);
            reference_table_id = table_id.map(str::to_owned);
            reference_table_version = table_version.map(str::to_owned);
            if !matches!(
                (table_id, table_version),
                (Some(table_id), Some(table_version))
                    if tables.get(&(entity_id, table_id)).is_some_and(|table| {
                        table.version == table_version && !table.values.is_empty()
                    })
            ) {
                status = MappingContractCoverageStatus::Blocker;
                reasons.push(
                    "The exact profile-declared reference domain is absent or mismatched."
                        .to_owned(),
                );
            }
        }
        rules.push(MappingContractRuleCoverage {
            rule_id: rule.rule_id.clone(),
            kind: rule.kind.clone(),
            entity_id: entity_id.to_owned(),
            field_id: field_id.to_owned(),
            reference_table_id,
            reference_table_version,
            status,
            reasons,
        });
    }

    let transform_roles = snapshot
        .map(|snapshot| {
            snapshot
                .bindings
                .iter()
                .filter(|binding| binding.kind == MappingKind::Lookup)
                .filter_map(|binding| {
                    Some((
                        binding.target_entity_id.as_deref()?.to_owned(),
                        binding.lookup_table_id.as_deref()?.to_owned(),
                    ))
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let reference_roles = profile
        .reference_table_obligations
        .iter()
        .map(|obligation| {
            (
                (obligation.entity_id.clone(), obligation.table_id.clone()),
                obligation.table_version.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut table_identities = snapshot
        .map(|snapshot| {
            snapshot
                .reference_tables
                .iter()
                .filter_map(|table| {
                    Some((
                        table.entity_id.as_deref()?.to_owned(),
                        table.table_id.clone(),
                        table.version.clone(),
                    ))
                })
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    table_identities.extend(
        profile
            .reference_table_obligations
            .iter()
            .map(|obligation| {
                (
                    obligation.entity_id.clone(),
                    obligation.table_id.clone(),
                    obligation.table_version.clone(),
                )
            }),
    );
    let mut table_coverage = Vec::with_capacity(table_identities.len());
    for (entity_id, table_id, table_version) in table_identities {
        let table = tables.get(&(entity_id.as_str(), table_id.as_str()));
        let entry_count = table.map_or(0, |table| table.values.len());
        let expected_version = reference_roles.get(&(entity_id.clone(), table_id.clone()));
        let mut table_roles = Vec::new();
        if transform_roles.contains(&(entity_id.clone(), table_id.clone())) {
            table_roles.push("TRANSFORM_LOOKUP".to_owned());
        }
        if expected_version.is_some() {
            table_roles.push("REFERENCE_DOMAIN".to_owned());
        }
        let (status, reasons) = if snapshot.is_none() {
            (
                MappingContractCoverageStatus::Unresolved,
                vec!["No durable mapping draft contains this table yet.".to_owned()],
            )
        } else if table_roles.is_empty() {
            (
                MappingContractCoverageStatus::Blocker,
                vec!["The table is not used by a binding or profile rule.".to_owned()],
            )
        } else if table.is_none_or(|table| table.values.is_empty()) {
            (
                MappingContractCoverageStatus::Blocker,
                vec!["The exact entity-scoped table is absent or empty.".to_owned()],
            )
        } else if expected_version.is_some_and(|version| version != &table_version) {
            (
                MappingContractCoverageStatus::Blocker,
                vec!["The table version differs from profile authority.".to_owned()],
            )
        } else {
            (MappingContractCoverageStatus::Complete, Vec::new())
        };
        table_coverage.push(MappingContractTableCoverage {
            entity_id,
            table_id,
            table_version,
            entry_count,
            roles: table_roles,
            status,
            reasons,
        });
    }

    let mut relationships = Vec::with_capacity(profile.relationships.len());
    for relationship in &profile.relationships {
        let confirmed = snapshot.and_then(|snapshot| {
            snapshot
                .relationship_bindings
                .iter()
                .find(|binding| binding.relationship_id == relationship.relationship_id)
        });
        let key_complete = relationship.key_pairs.iter().all(|pair| {
            [
                (
                    relationship.from_entity_id.as_str(),
                    pair.from_field_id.as_str(),
                ),
                (
                    relationship.to_entity_id.as_str(),
                    pair.to_field_id.as_str(),
                ),
            ]
            .iter()
            .all(|target| {
                bindings.get(target).is_some_and(|binding| {
                    binding.kind == MappingKind::Direct
                        && binding.source_file.is_some()
                        && binding.source_column.is_some()
                        && binding.source_column_ordinal.is_some()
                })
            })
        });
        let (status, reasons) = if snapshot.is_none() {
            (
                MappingContractCoverageStatus::Unresolved,
                vec!["No durable mapping draft confirms this relationship yet.".to_owned()],
            )
        } else if confirmed
            != Some(&MappingRelationshipBinding {
                relationship_id: relationship.relationship_id.clone(),
                from_entity_id: relationship.from_entity_id.clone(),
                to_entity_id: relationship.to_entity_id.clone(),
                cardinality: relationship.cardinality.clone(),
                key_pairs: relationship.key_pairs.clone(),
            })
            || !key_complete
        {
            (
                MappingContractCoverageStatus::Blocker,
                vec![
                    "The explicit relationship or one of its direct key bindings is incomplete."
                        .to_owned(),
                ],
            )
        } else {
            (MappingContractCoverageStatus::Complete, Vec::new())
        };
        relationships.push(MappingContractRelationshipCoverage {
            relationship_id: relationship.relationship_id.clone(),
            from_entity_id: relationship.from_entity_id.clone(),
            to_entity_id: relationship.to_entity_id.clone(),
            cardinality: relationship.cardinality.clone(),
            key_pairs: relationship.key_pairs.clone(),
            status,
            reasons,
        });
    }

    let used_columns = snapshot
        .map(|snapshot| {
            snapshot
                .bindings
                .iter()
                .filter_map(|binding| {
                    Some((
                        binding.source_file.as_deref()?.to_owned(),
                        binding.source_column.as_deref()?.to_owned(),
                        binding.source_column_ordinal?,
                    ))
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let source_columns = workspace
        .source_files
        .iter()
        .flat_map(|file| file.columns.iter())
        .map(|column| {
            let used = used_columns.contains(&(
                column.source_file.clone(),
                column.source_column.clone(),
                column.source_column_ordinal,
            ));
            MappingContractSourceCoverage {
                source_file: column.source_file.clone(),
                source_column: column.source_column.clone(),
                source_column_ordinal: column.source_column_ordinal,
                status: if snapshot.is_none() {
                    MappingContractCoverageStatus::Unresolved
                } else if used {
                    MappingContractCoverageStatus::Complete
                } else {
                    MappingContractCoverageStatus::Warning
                },
                reason: if snapshot.is_none() {
                    "No durable mapping draft uses accepted columns yet.".to_owned()
                } else if used {
                    "Used by at least one destination binding.".to_owned()
                } else {
                    "Accepted column is intentionally or currently unused.".to_owned()
                },
            }
        })
        .collect::<Vec<_>>();

    let statuses = fields
        .iter()
        .map(|item| item.status)
        .chain(source_columns.iter().map(|item| item.status))
        .chain(rules.iter().map(|item| item.status))
        .chain(table_coverage.iter().map(|item| item.status))
        .chain(relationships.iter().map(|item| item.status))
        .collect::<Vec<_>>();
    let summary = MappingContractCoverageSummary {
        complete: statuses
            .iter()
            .filter(|status| **status == MappingContractCoverageStatus::Complete)
            .count(),
        unresolved: statuses
            .iter()
            .filter(|status| **status == MappingContractCoverageStatus::Unresolved)
            .count(),
        warnings: statuses
            .iter()
            .filter(|status| **status == MappingContractCoverageStatus::Warning)
            .count(),
        blockers: statuses
            .iter()
            .filter(|status| **status == MappingContractCoverageStatus::Blocker)
            .count(),
    };
    let overall_status = if summary.blockers > 0 {
        MappingContractCoverageStatus::Blocker
    } else if summary.unresolved > 0 {
        MappingContractCoverageStatus::Unresolved
    } else if summary.warnings > 0 {
        MappingContractCoverageStatus::Warning
    } else {
        MappingContractCoverageStatus::Complete
    };
    Ok(MappingContractCoverageView {
        schema_version: MAPPING_CONTRACT_COVERAGE_SCHEMA.to_owned(),
        authority,
        overall_status,
        summary,
        fields,
        source_columns,
        rules,
        tables: table_coverage,
        relationships,
        coverage_hash: String::new(),
    })
}

fn mapping_contract_coverage_hash(coverage: &MappingContractCoverageView) -> Result<String> {
    let mut hashed = coverage.clone();
    hashed.coverage_hash.clear();
    canonical_hash(&serde_json::to_value(hashed)?)
}

fn required_profile_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| KernelError::IntegrityFailed(format!("profile {key} vanished")))
}

#[allow(clippy::too_many_arguments)]
fn load_active_mapping_draft_head(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    object_pack_id: &str,
    object_pack_version: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<Option<MappingDraftHeadRow>> {
    connection
        .query_row(
            "SELECT draft_id, generation, state, head_revision_hash,
                    submitted_mapping_id, submitted_mapping_version,
                    submitted_mapping_hash, updated_at
             FROM engagement_mapping_draft_heads
             WHERE engagement_id = ?1 AND source_job_id = ?2 AND source_hash = ?3
               AND object_pack_id = ?4 AND object_pack_version = ?5
               AND profile_id = ?6 AND profile_version = ?7 AND profile_hash = ?8",
            params![
                engagement_id,
                job_id,
                source_hash,
                object_pack_id,
                object_pack_version,
                profile_id,
                profile_version,
                profile_hash,
            ],
            |row| {
                let generation = row.get::<_, i64>(1)?;
                Ok(MappingDraftHeadRow {
                    draft_id: row.get(0)?,
                    generation: u64::try_from(generation).unwrap_or(u64::MAX),
                    state: row.get(2)?,
                    head_revision_hash: row.get(3)?,
                    submitted_mapping_id: row.get(4)?,
                    submitted_mapping_version: row.get(5)?,
                    submitted_mapping_hash: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn load_active_mapping_draft(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    object_pack_id: &str,
    object_pack_version: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<Option<MappingDraftView>> {
    let Some(head) = load_active_mapping_draft_head(
        connection,
        engagement_id,
        job_id,
        source_hash,
        object_pack_id,
        object_pack_version,
        profile_id,
        profile_version,
        profile_hash,
    )?
    else {
        return Ok(None);
    };
    let (canonical_draft_json, revision_hash): (String, String) = connection.query_row(
        "SELECT canonical_draft_json, revision_hash
         FROM engagement_mapping_draft_revisions
         WHERE draft_id = ?1 AND generation = ?2",
        params![
            head.draft_id,
            i64::try_from(head.generation).unwrap_or(i64::MAX)
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if revision_hash != head.head_revision_hash {
        return Err(KernelError::IntegrityFailed(
            "mapping draft head differs from its immutable revision".to_owned(),
        ));
    }
    let value: Value = serde_json::from_str(&canonical_draft_json)?;
    if canonical_hash(&value)? != revision_hash {
        return Err(KernelError::IntegrityFailed(
            "mapping draft revision hash replay failed".to_owned(),
        ));
    }
    let snapshot: MappingDraftSnapshot = serde_json::from_value(value)?;
    Ok(Some(MappingDraftView {
        schema_version: MAPPING_DRAFT_SCHEMA.to_owned(),
        draft_id: head.draft_id,
        draft_generation: head.generation,
        draft_hash: revision_hash,
        state: head.state,
        snapshot,
        submitted_mapping_id: head.submitted_mapping_id,
        submitted_mapping_version: head.submitted_mapping_version,
        submitted_mapping_hash: head.submitted_mapping_hash,
        updated_at: head.updated_at,
    }))
}

#[allow(clippy::too_many_lines)]
fn validate_mapping_draft_snapshot(
    snapshot: &MappingDraftSnapshot,
    workspace: &MappingWorkspaceView,
    profile: &MappingWorkspaceProfileModel,
    pack: &ControlledObjectPack,
) -> Result<()> {
    if snapshot.bindings.is_empty() || snapshot.bindings.len() > workspace.limits.maximum_bindings {
        return Err(KernelError::MappingInvalid(format!(
            "operator mapping must contain 1..={} bindings",
            workspace.limits.maximum_bindings
        )));
    }
    if workspace.source_files.is_empty()
        || workspace.source_files.len() > workspace.limits.maximum_source_files
        || profile.relationships.len() > workspace.limits.maximum_relationships
    {
        return Err(KernelError::MappingInvalid(
            "mapping source or relationship inventory exceeds the active bounded contract"
                .to_owned(),
        ));
    }
    let source_files = workspace
        .source_files
        .iter()
        .map(|file| file.source_file.as_str())
        .collect::<BTreeSet<_>>();
    let entity_ids = profile
        .entities
        .iter()
        .map(|entity| entity.entity_id.as_str())
        .collect::<BTreeSet<_>>();
    if snapshot.source_entity_assignments.len() != source_files.len() {
        return Err(KernelError::MappingInvalid(
            "every accepted source file must have exactly one entity assignment".to_owned(),
        ));
    }
    let mut assigned_files = HashSet::new();
    let mut assigned_entities = HashMap::<&str, usize>::new();
    let mut route_by_file = HashMap::new();
    for assignment in &snapshot.source_entity_assignments {
        if !source_files.contains(assignment.source_file.as_str())
            || !entity_ids.contains(assignment.entity_id.as_str())
            || !assigned_files.insert(assignment.source_file.as_str())
        {
            return Err(KernelError::MappingInvalid(
                "source-to-entity assignments contain an unknown or duplicate identity".to_owned(),
            ));
        }
        *assigned_entities
            .entry(assignment.entity_id.as_str())
            .or_default() += 1;
        route_by_file.insert(
            assignment.source_file.as_str(),
            assignment.entity_id.as_str(),
        );
    }
    if assigned_files.len() != source_files.len()
        || entity_ids
            .iter()
            .any(|entity_id| assigned_entities.get(entity_id).copied().unwrap_or(0) != 1)
    {
        return Err(KernelError::MappingInvalid(
            "this bounded contract requires exactly one accepted file per active destination entity"
                .to_owned(),
        ));
    }

    validate_draft_reference_tables(snapshot, workspace, pack, profile)?;
    let reference_tables = snapshot
        .reference_tables
        .iter()
        .map(|table| {
            (
                (
                    table.entity_id.as_deref().unwrap_or(&pack.root_entity_id),
                    table.table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let accepted_columns = workspace
        .source_files
        .iter()
        .flat_map(|file| {
            file.columns.iter().map(|column| {
                (
                    column.source_file.as_str(),
                    column.source_column.as_str(),
                    column.source_column_ordinal,
                )
            })
        })
        .collect::<HashSet<_>>();
    let mut binding_ids = HashSet::new();
    let mut qualified_targets = HashSet::new();
    let mut binding_by_target = HashMap::new();
    for binding in &snapshot.bindings {
        validate_mapping_document_id(&binding.binding_id, "mapping binding ID")?;
        let target_entity = binding.target_entity_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "operator mapping binding {:?} has no target entity",
                binding.binding_id
            ))
        })?;
        if !binding_ids.insert(binding.binding_id.as_str())
            || !qualified_targets.insert((target_entity, binding.target_field_id.as_str()))
            || !profile
                .fields
                .contains_key(&(target_entity.to_owned(), binding.target_field_id.clone()))
        {
            return Err(KernelError::MappingInvalid(
                "mapping bindings contain an unknown or duplicate qualified target".to_owned(),
            ));
        }
        let has_source = match (
            binding.source_file.as_deref(),
            binding.source_column.as_deref(),
            binding.source_column_ordinal,
        ) {
            (Some(file), Some(column), Some(ordinal))
                if accepted_columns.contains(&(file, column, ordinal))
                    && route_by_file.get(file).copied() == Some(target_entity) =>
            {
                true
            }
            (None, None, None) => false,
            _ => {
                return Err(KernelError::MappingInvalid(format!(
                    "binding {:?} has an incomplete, unknown, or wrong-entity source identity",
                    binding.binding_id
                )));
            }
        };
        match binding.kind {
            MappingKind::Constant => {
                if has_source
                    || binding.constant.as_deref().is_none_or(|value| {
                        value.len() > 4_096 || value.chars().any(char::is_control)
                    })
                    || binding.lookup_table_id.is_some()
                {
                    return Err(KernelError::MappingInvalid(format!(
                        "constant binding {:?} has inconsistent parameters",
                        binding.binding_id
                    )));
                }
            }
            MappingKind::Lookup => {
                let table_id = binding.lookup_table_id.as_deref().ok_or_else(|| {
                    KernelError::MappingInvalid(format!(
                        "lookup binding {:?} has no table",
                        binding.binding_id
                    ))
                })?;
                if !has_source
                    || binding.constant.is_some()
                    || !reference_tables.contains_key(&(target_entity, table_id))
                {
                    return Err(KernelError::MappingInvalid(format!(
                        "lookup binding {:?} has inconsistent entity-scoped reference data",
                        binding.binding_id
                    )));
                }
            }
            MappingKind::Direct
            | MappingKind::Trim
            | MappingKind::EmptyAsNull
            | MappingKind::UppercaseAscii
            | MappingKind::LowercaseAscii => {
                if !has_source || binding.constant.is_some() || binding.lookup_table_id.is_some() {
                    return Err(KernelError::MappingInvalid(format!(
                        "source binding {:?} has inconsistent parameters",
                        binding.binding_id
                    )));
                }
            }
        }
        binding_by_target.insert(
            (target_entity.to_owned(), binding.target_field_id.clone()),
            binding,
        );
    }
    let missing_fields = profile
        .fields
        .keys()
        .filter(|target| !binding_by_target.contains_key(*target))
        .map(|(entity, field)| format!("{entity}.{field}"))
        .collect::<Vec<_>>();
    if !missing_fields.is_empty() {
        return Err(KernelError::MappingInvalid(format!(
            "operator mapping must bind every canonical slot; missing {}",
            missing_fields.join(", ")
        )));
    }
    if snapshot.relationship_bindings.len() != profile.relationships.len() {
        return Err(KernelError::MappingInvalid(
            "every explicit canonical relationship must be confirmed exactly once".to_owned(),
        ));
    }
    let declared = profile
        .relationships
        .iter()
        .map(|relationship| {
            (
                relationship.relationship_id.as_str(),
                MappingRelationshipBinding {
                    relationship_id: relationship.relationship_id.clone(),
                    from_entity_id: relationship.from_entity_id.clone(),
                    to_entity_id: relationship.to_entity_id.clone(),
                    cardinality: relationship.cardinality.clone(),
                    key_pairs: relationship.key_pairs.clone(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let mut confirmed_ids = HashSet::new();
    for relationship in &snapshot.relationship_bindings {
        if !confirmed_ids.insert(relationship.relationship_id.as_str())
            || declared.get(relationship.relationship_id.as_str()) != Some(relationship)
        {
            return Err(KernelError::MappingInvalid(
                "mapping relationship confirmation differs from canonical profile authority"
                    .to_owned(),
            ));
        }
        for pair in &relationship.key_pairs {
            for target in [
                (&relationship.from_entity_id, &pair.from_field_id),
                (&relationship.to_entity_id, &pair.to_field_id),
            ] {
                let binding = binding_by_target
                    .get(&(target.0.clone(), target.1.clone()))
                    .ok_or_else(|| {
                        KernelError::MappingInvalid(
                            "relationship key has no explicit source binding".to_owned(),
                        )
                    })?;
                if binding.kind != MappingKind::Direct
                    || binding.source_file.is_none()
                    || binding.source_column.is_none()
                    || binding.source_column_ordinal.is_none()
                {
                    return Err(KernelError::MappingInvalid(
                        "relationship keys require direct, non-nullable source bindings".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_draft_reference_tables(
    snapshot: &MappingDraftSnapshot,
    workspace: &MappingWorkspaceView,
    pack: &ControlledObjectPack,
    profile: &MappingWorkspaceProfileModel,
) -> Result<()> {
    if snapshot.reference_tables.len() > workspace.limits.maximum_reference_tables {
        return Err(KernelError::MappingInvalid(
            "mapping draft reference-table inventory exceeds the active bound".to_owned(),
        ));
    }
    if workspace.destination_origin == DestinationOrigin::ControlledPack.as_str() {
        return validate_controlled_draft_reference_tables(snapshot, pack, profile, workspace);
    }
    if workspace.destination_origin != DestinationOrigin::WorkspaceProfile.as_str()
        || !pack.reference_tables.is_empty()
    {
        return Err(KernelError::IntegrityFailed(
            "mapping draft has an unsupported destination/table authority combination".to_owned(),
        ));
    }

    let obligations = profile
        .reference_table_obligations
        .iter()
        .map(|obligation| {
            (
                (obligation.entity_id.as_str(), obligation.table_id.as_str()),
                obligation.table_version.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let transform_tables = snapshot
        .bindings
        .iter()
        .filter(|binding| binding.kind == MappingKind::Lookup)
        .filter_map(|binding| {
            Some((
                binding.target_entity_id.as_deref()?,
                binding.lookup_table_id.as_deref()?,
            ))
        })
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut entry_count = 0_usize;
    let mut reference_bytes = 0_usize;
    for table in &snapshot.reference_tables {
        let entity = table.entity_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(
                "workspace reference tables require explicit entity scope".to_owned(),
            )
        })?;
        validate_mapping_document_id(&table.table_id, "reference table ID")?;
        validate_operator_mapping_version(&table.version)?;
        let key = (entity, table.table_id.as_str());
        if !seen.insert(key)
            || !profile
                .entities
                .iter()
                .any(|candidate| candidate.entity_id == entity)
            || (!obligations.contains_key(&key) && !transform_tables.contains(&key))
        {
            return Err(KernelError::MappingInvalid(
                "workspace reference-table identity is duplicate, unused, or outside the active entity contract"
                    .to_owned(),
            ));
        }
        if obligations
            .get(&key)
            .is_some_and(|expected| **expected != table.version)
        {
            return Err(KernelError::MappingInvalid(format!(
                "workspace reference table {entity}.{} has the wrong declared version",
                table.table_id
            )));
        }
        if table.values.is_empty() {
            return Err(KernelError::MappingInvalid(format!(
                "workspace reference table {entity}.{} must contain at least one exact entry",
                table.table_id
            )));
        }
        entry_count = entry_count
            .checked_add(table.values.len())
            .ok_or_else(|| KernelError::MappingInvalid("reference entry overflow".to_owned()))?;
        for (source, target) in &table.values {
            if source.is_empty()
                || source.len() > workspace.limits.maximum_reference_key_bytes
                || target.is_empty()
                || target.len() > workspace.limits.maximum_reference_value_bytes
                || source.chars().any(char::is_control)
                || target.chars().any(char::is_control)
            {
                return Err(KernelError::MappingInvalid(
                    "workspace reference entry is blank, overlong, or contains control characters"
                        .to_owned(),
                ));
            }
            reference_bytes = reference_bytes
                .checked_add(source.len())
                .and_then(|bytes| bytes.checked_add(target.len()))
                .ok_or_else(|| {
                    KernelError::MappingInvalid("reference byte count overflow".to_owned())
                })?;
        }
    }
    if entry_count > workspace.limits.maximum_reference_entries
        || reference_bytes > workspace.limits.maximum_reference_bytes
    {
        return Err(KernelError::MappingInvalid(format!(
            "workspace reference data requires {entry_count} entries/{reference_bytes} bytes; active bounds are {}/{}",
            workspace.limits.maximum_reference_entries, workspace.limits.maximum_reference_bytes
        )));
    }
    for ((entity, table_id), version) in obligations {
        if !snapshot.reference_tables.iter().any(|table| {
            table.entity_id.as_deref() == Some(entity)
                && table.table_id == table_id
                && table.version == version
        }) {
            return Err(KernelError::MappingInvalid(format!(
                "profile-declared reference table {entity}.{table_id}/{version} is missing"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_controlled_draft_reference_tables(
    snapshot: &MappingDraftSnapshot,
    pack: &ControlledObjectPack,
    profile: &MappingWorkspaceProfileModel,
    workspace: &MappingWorkspaceView,
) -> Result<()> {
    if snapshot.reference_tables.len() != pack.reference_tables.len() {
        return Err(KernelError::MappingInvalid(
            "mapping draft reference-table inventory differs from the controlled pack".to_owned(),
        ));
    }
    let controlled = pack
        .reference_tables
        .iter()
        .map(|table| {
            (
                (
                    table.entity_id.as_deref().unwrap_or(&pack.root_entity_id),
                    table.table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let domain_values = pack
        .reference_tables
        .iter()
        .find(|table| table.table_id == "sales-document-type-domain")
        .map(|table| table.values.values().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut entry_count = 0_usize;
    for table in &snapshot.reference_tables {
        let entity = table.entity_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(
                "operator reference tables require explicit entity scope".to_owned(),
            )
        })?;
        let key = (entity, table.table_id.as_str());
        let expected = controlled.get(&key).ok_or_else(|| {
            KernelError::MappingInvalid(
                "mapping draft introduced an unknown entity-scoped reference table".to_owned(),
            )
        })?;
        if !seen.insert(key)
            || table.version != expected.version
            || !profile
                .entities
                .iter()
                .any(|candidate| candidate.entity_id == entity)
        {
            return Err(KernelError::MappingInvalid(
                "mapping draft reference-table identity is invalid".to_owned(),
            ));
        }
        entry_count = entry_count
            .checked_add(table.values.len())
            .ok_or_else(|| KernelError::MappingInvalid("reference entry overflow".to_owned()))?;
        if table.values.keys().collect::<BTreeSet<_>>()
            != expected.values.keys().collect::<BTreeSet<_>>()
            || table.values.iter().any(|(source, target)| {
                source.is_empty()
                    || source.len() > workspace.limits.maximum_reference_key_bytes
                    || target.len() > workspace.limits.maximum_reference_value_bytes
                    || source.chars().any(char::is_control)
                    || target.chars().any(char::is_control)
            })
        {
            return Err(KernelError::MappingInvalid(
                "mapping draft reference values exceed the controlled key/value bounds".to_owned(),
            ));
        }
        if table.table_id == "sales-document-route-map" {
            if table
                .values
                .values()
                .any(|value| !domain_values.contains(value))
            {
                return Err(KernelError::MappingInvalid(
                    "route-code lookup targets must remain inside the reviewed document-type domain"
                        .to_owned(),
                ));
            }
        } else if table.values != expected.values {
            return Err(KernelError::MappingInvalid(
                "non-governable controlled reference data cannot be changed".to_owned(),
            ));
        }
    }
    let reference_bytes = snapshot
        .reference_tables
        .iter()
        .flat_map(|table| table.values.iter())
        .try_fold(0_usize, |bytes, (source, target)| {
            bytes
                .checked_add(source.len())
                .and_then(|value| value.checked_add(target.len()))
                .ok_or_else(|| {
                    KernelError::MappingInvalid("reference byte count overflow".to_owned())
                })
        })?;
    if entry_count > workspace.limits.maximum_reference_entries
        || reference_bytes > workspace.limits.maximum_reference_bytes
    {
        return Err(KernelError::MappingInvalid(format!(
            "controlled mapping reference data requires {entry_count} entries/{reference_bytes} bytes; active bounds are {}/{}",
            workspace.limits.maximum_reference_entries, workspace.limits.maximum_reference_bytes
        )));
    }
    Ok(())
}

fn load_mapping_workspace_attempt(
    connection: &Connection,
    engagement_id: &str,
    idempotency_key: &str,
    request_hash: &str,
    command_kind: &str,
) -> Result<Option<MappingWorkspaceAttemptRow>> {
    let existing = connection
        .query_row(
            "SELECT attempt_id, command_kind, request_hash, draft_id,
                    result_generation, result_state,
                    result_reference, result_hash
             FROM engagement_mapping_workspace_attempts
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| {
                let result_generation = row.get::<_, i64>(4)?;
                Ok(MappingWorkspaceAttemptRow {
                    attempt_id: row.get(0)?,
                    command_kind: row.get(1)?,
                    request_hash: row.get(2)?,
                    draft_id: row.get(3)?,
                    result_generation: u64::try_from(result_generation).unwrap_or(u64::MAX),
                    result_state: row.get(5)?,
                    result_reference: row.get(6)?,
                    result_hash: row.get(7)?,
                })
            },
        )
        .optional()?;
    let Some(attempt) = existing else {
        return Ok(None);
    };
    if attempt.command_kind != command_kind || attempt.request_hash != request_hash {
        return Err(KernelError::Conflict(
            "mapping workspace idempotency key was used for different command bytes".to_owned(),
        ));
    }
    Ok(Some(attempt))
}

#[allow(clippy::too_many_arguments)]
fn insert_mapping_workspace_attempt(
    transaction: &rusqlite::Transaction<'_>,
    engagement_id: &str,
    draft_id: &str,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
    canonical_request_json: &str,
    expected_generation: u64,
    expected_state: &str,
    result_generation: u64,
    result_state: &str,
    result_reference: &str,
    result_hash: &str,
    created_at: &str,
) -> Result<String> {
    let attempt_id = Uuid::new_v4().to_string();
    insert_mapping_workspace_attempt_with_id(
        transaction,
        &attempt_id,
        engagement_id,
        draft_id,
        command_kind,
        idempotency_key,
        request_hash,
        canonical_request_json,
        expected_generation,
        expected_state,
        result_generation,
        result_state,
        result_reference,
        result_hash,
        created_at,
    )?;
    Ok(attempt_id)
}

#[allow(clippy::too_many_arguments)]
fn insert_mapping_workspace_attempt_with_id(
    transaction: &rusqlite::Transaction<'_>,
    attempt_id: &str,
    engagement_id: &str,
    draft_id: &str,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
    canonical_request_json: &str,
    expected_generation: u64,
    expected_state: &str,
    result_generation: u64,
    result_state: &str,
    result_reference: &str,
    result_hash: &str,
    created_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO engagement_mapping_workspace_attempts (
             attempt_id, engagement_id, draft_id, command_kind,
             idempotency_key, request_hash, canonical_request_json,
             expected_generation, expected_state,
             result_generation, result_state, result_reference,
             result_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                   ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            attempt_id,
            engagement_id,
            draft_id,
            command_kind,
            idempotency_key,
            request_hash,
            canonical_request_json,
            i64::try_from(expected_generation).unwrap_or(i64::MAX),
            expected_state,
            i64::try_from(result_generation).unwrap_or(i64::MAX),
            result_state,
            result_reference,
            result_hash,
            created_at,
        ],
    )?;
    Ok(())
}

fn load_draft_revision_view(
    connection: &Connection,
    draft_id: &str,
    generation: u64,
    expected_hash: &str,
    state: &str,
    submitted: Option<(String, String, String)>,
) -> Result<MappingDraftView> {
    let (canonical_json, revision_hash, created_at): (String, String, String) = connection
        .query_row(
            "SELECT canonical_draft_json, revision_hash, created_at
             FROM engagement_mapping_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![draft_id, i64::try_from(generation).unwrap_or(i64::MAX)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let value: Value = serde_json::from_str(&canonical_json)?;
    if revision_hash != expected_hash || canonical_hash(&value)? != revision_hash {
        return Err(KernelError::IntegrityFailed(
            "mapping draft attempt differs from its immutable revision".to_owned(),
        ));
    }
    let snapshot = serde_json::from_value(value)?;
    let (submitted_mapping_id, submitted_mapping_version, submitted_mapping_hash) = submitted
        .map_or((None, None, None), |(id, version, hash)| {
            (Some(id), Some(version), Some(hash))
        });
    Ok(MappingDraftView {
        schema_version: MAPPING_DRAFT_SCHEMA.to_owned(),
        draft_id: draft_id.to_owned(),
        draft_generation: generation,
        draft_hash: revision_hash,
        state: state.to_owned(),
        snapshot,
        submitted_mapping_id,
        submitted_mapping_version,
        submitted_mapping_hash,
        updated_at: created_at,
    })
}

fn replay_saved_mapping_draft(
    connection: &Connection,
    attempt: &MappingWorkspaceAttemptRow,
    engagement_generation: u64,
) -> Result<SaveMappingDraftResult> {
    if attempt.result_state != "EDITABLE"
        || attempt.result_reference != attempt.draft_id
        || attempt.result_generation == 0
    {
        return Err(KernelError::IntegrityFailed(
            "saved mapping-draft attempt has inconsistent result identity".to_owned(),
        ));
    }
    let draft = load_draft_revision_view(
        connection,
        &attempt.draft_id,
        attempt.result_generation,
        &attempt.result_hash,
        "EDITABLE",
        None,
    )?;
    Ok(SaveMappingDraftResult {
        schema_version: MAPPING_DRAFT_SCHEMA.to_owned(),
        engagement_generation,
        draft,
        idempotent_replay: true,
    })
}

fn replay_submitted_mapping(
    workspace: &Path,
    connection: &Connection,
    attempt: &MappingWorkspaceAttemptRow,
    engagement_generation: u64,
) -> Result<SubmitMappingDraftResult> {
    if attempt.result_state != "SUBMITTED" {
        return Err(KernelError::IntegrityFailed(
            "mapping-submission attempt has an invalid result state".to_owned(),
        ));
    }
    let (mapping_id, mapping_version, mapping_hash): (String, String, String) = connection
        .query_row(
            "SELECT mapping_id, mapping_version, mapping_hash
             FROM engagement_mapping_preparations
             WHERE preparation_id = ?1 AND mapping_hash = ?2",
            params![attempt.result_reference, attempt.result_hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let inspection = crate::mapping::inspect_mapping(workspace, &mapping_id, &mapping_version)?;
    if inspection.mapping_hash != mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "submitted mapping replay differs from registered evidence".to_owned(),
        ));
    }
    let draft = load_draft_revision_view(
        connection,
        &attempt.draft_id,
        attempt.result_generation,
        &connection.query_row(
            "SELECT head_revision_hash FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&attempt.draft_id],
            |row| row.get::<_, String>(0),
        )?,
        "SUBMITTED",
        Some((mapping_id, mapping_version, mapping_hash.clone())),
    )?;
    Ok(SubmitMappingDraftResult {
        schema_version: SUBMITTED_MAPPING_SCHEMA.to_owned(),
        engagement_generation,
        draft,
        preparation_id: attempt.result_reference.clone(),
        mapping_hash,
        coverage: inspection.coverage,
        idempotent_replay: true,
    })
}

fn replay_mapping_approval(
    connection: &Connection,
    attempt: &MappingWorkspaceAttemptRow,
    engagement_generation: u64,
) -> Result<MappingApprovalResult> {
    if attempt.result_state != "APPROVED" {
        return Err(KernelError::IntegrityFailed(
            "mapping-approval attempt has an invalid result state".to_owned(),
        ));
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
        "SELECT approval_id, approval_hash, mapping_id, mapping_version,
                mapping_hash, approver, reason, approved_at
         FROM engagement_mapping_approvals
         WHERE attempt_id = ?1 AND approval_id = ?2 AND approval_hash = ?3",
        params![
            attempt.attempt_id,
            attempt.result_reference,
            attempt.result_hash
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
            ))
        },
    )?;
    Ok(MappingApprovalResult {
        schema_version: MAPPING_APPROVAL_SCHEMA.to_owned(),
        engagement_generation,
        approval_id: row.0,
        approval_hash: row.1,
        mapping_id: row.2,
        mapping_version: row.3,
        mapping_hash: row.4,
        actor: row.5,
        reason: row.6,
        approved_at: row.7,
        idempotent_replay: true,
    })
}

fn mapping_authority_integrity(detail: impl Into<String>) -> KernelError {
    KernelError::IntegrityFailed(format!(
        "operator mapping authority integrity: {}",
        detail.into()
    ))
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn stored_text_is_bounded(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len()) && !value.chars().any(char::is_control)
}

fn normalized_mapping_authority_guard(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const MAPPING_DRAFT_HEAD_UPDATE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_draft_heads_guarded_update
    BEFORE UPDATE ON engagement_mapping_draft_heads
    WHEN NEW.draft_id IS NOT OLD.draft_id
      OR NEW.engagement_id IS NOT OLD.engagement_id
      OR NEW.source_job_id IS NOT OLD.source_job_id
      OR NEW.source_hash IS NOT OLD.source_hash
      OR NEW.object_pack_id IS NOT OLD.object_pack_id
      OR NEW.object_pack_version IS NOT OLD.object_pack_version
      OR NEW.profile_id IS NOT OLD.profile_id
      OR NEW.profile_version IS NOT OLD.profile_version
      OR NEW.profile_hash IS NOT OLD.profile_hash
      OR NEW.created_at IS NOT OLD.created_at
      OR (
           OLD.state = 'EDITABLE'
           AND NOT (
               (NEW.state = 'EDITABLE'
                AND NEW.generation = OLD.generation + 1
                AND NEW.head_revision_hash IS NOT OLD.head_revision_hash
                AND NEW.submitted_mapping_id IS NULL
                AND NEW.submitted_mapping_version IS NULL
                AND NEW.submitted_mapping_hash IS NULL
                AND NEW.submitted_at IS NULL
                AND NEW.approved_at IS NULL)
               OR
               (NEW.state = 'SUBMITTED'
                AND NEW.generation = OLD.generation
                AND NEW.head_revision_hash IS OLD.head_revision_hash
                AND NEW.submitted_mapping_id IS NOT NULL
                AND NEW.submitted_mapping_version IS NOT NULL
                AND NEW.submitted_mapping_hash IS NOT NULL
                AND NEW.submitted_at IS NOT NULL
                AND NEW.approved_at IS NULL)
           )
      )
      OR (
           OLD.state = 'SUBMITTED'
           AND NOT (
               NEW.state = 'APPROVED'
               AND NEW.generation = OLD.generation
               AND NEW.head_revision_hash IS OLD.head_revision_hash
               AND NEW.submitted_mapping_id IS OLD.submitted_mapping_id
               AND NEW.submitted_mapping_version
                   IS OLD.submitted_mapping_version
               AND NEW.submitted_mapping_hash IS OLD.submitted_mapping_hash
               AND NEW.submitted_at IS OLD.submitted_at
               AND NEW.approved_at IS NOT NULL
           )
      )
      OR OLD.state = 'APPROVED'
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping draft head transition is invalid'
        );
    END";
const MAPPING_DRAFT_HEAD_DELETE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_draft_heads_no_delete
    BEFORE DELETE ON engagement_mapping_draft_heads
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping draft heads cannot be deleted'
        );
    END";
const MAPPING_DRAFT_REVISION_UPDATE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_draft_revisions_no_update
    BEFORE UPDATE ON engagement_mapping_draft_revisions
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping draft revisions are immutable'
        );
    END";
const MAPPING_DRAFT_REVISION_DELETE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_draft_revisions_no_delete
    BEFORE DELETE ON engagement_mapping_draft_revisions
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping draft revisions are immutable'
        );
    END";
const MAPPING_WORKSPACE_ATTEMPT_UPDATE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_workspace_attempts_no_update
    BEFORE UPDATE ON engagement_mapping_workspace_attempts
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping workspace attempts are immutable'
        );
    END";
const MAPPING_WORKSPACE_ATTEMPT_DELETE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_workspace_attempts_no_delete
    BEFORE DELETE ON engagement_mapping_workspace_attempts
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping workspace attempts are immutable'
        );
    END";
const MAPPING_APPROVAL_UPDATE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_approvals_no_update
    BEFORE UPDATE ON engagement_mapping_approvals
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping approvals are immutable'
        );
    END";
const MAPPING_APPROVAL_DELETE_GUARD: &str = r"
CREATE TRIGGER engagement_mapping_approvals_no_delete
    BEFORE DELETE ON engagement_mapping_approvals
    BEGIN
        SELECT RAISE(
            ABORT,
            'engagement mapping approvals are immutable'
        );
    END";

fn verify_mapping_authority_guards(connection: &Connection) -> Result<()> {
    let guards = [
        (
            "engagement_mapping_draft_heads_guarded_update",
            "engagement_mapping_draft_heads",
            MAPPING_DRAFT_HEAD_UPDATE_GUARD,
        ),
        (
            "engagement_mapping_draft_heads_no_delete",
            "engagement_mapping_draft_heads",
            MAPPING_DRAFT_HEAD_DELETE_GUARD,
        ),
        (
            "engagement_mapping_draft_revisions_no_update",
            "engagement_mapping_draft_revisions",
            MAPPING_DRAFT_REVISION_UPDATE_GUARD,
        ),
        (
            "engagement_mapping_draft_revisions_no_delete",
            "engagement_mapping_draft_revisions",
            MAPPING_DRAFT_REVISION_DELETE_GUARD,
        ),
        (
            "engagement_mapping_workspace_attempts_no_update",
            "engagement_mapping_workspace_attempts",
            MAPPING_WORKSPACE_ATTEMPT_UPDATE_GUARD,
        ),
        (
            "engagement_mapping_workspace_attempts_no_delete",
            "engagement_mapping_workspace_attempts",
            MAPPING_WORKSPACE_ATTEMPT_DELETE_GUARD,
        ),
        (
            "engagement_mapping_approvals_no_update",
            "engagement_mapping_approvals",
            MAPPING_APPROVAL_UPDATE_GUARD,
        ),
        (
            "engagement_mapping_approvals_no_delete",
            "engagement_mapping_approvals",
            MAPPING_APPROVAL_DELETE_GUARD,
        ),
    ];
    for (name, table, expected) in guards {
        let sql = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                params![name, table],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(sql) = sql else {
            return Err(mapping_authority_integrity(format!(
                "required immutability guard {name:?} is missing"
            )));
        };
        if normalized_mapping_authority_guard(&sql) != normalized_mapping_authority_guard(expected)
        {
            return Err(mapping_authority_integrity(format!(
                "immutability guard {name:?} has an unexpected definition"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_mapping_authority_storage_bounds(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<bool> {
    let checks = [
        (
            "draft heads",
            "SELECT COUNT(*) FROM engagement_mapping_draft_heads
             WHERE length(CAST(draft_id AS BLOB))
                 + length(CAST(engagement_id AS BLOB))
                 + length(CAST(source_job_id AS BLOB))
                 + length(CAST(source_hash AS BLOB))
                 + length(CAST(object_pack_id AS BLOB))
                 + length(CAST(object_pack_version AS BLOB))
                 + length(CAST(profile_id AS BLOB))
                 + length(CAST(profile_version AS BLOB))
                 + length(CAST(profile_hash AS BLOB))
                 + length(CAST(state AS BLOB))
                 + length(CAST(head_revision_hash AS BLOB))
                 + COALESCE(length(CAST(submitted_mapping_id AS BLOB)), 0)
                 + COALESCE(length(CAST(submitted_mapping_version AS BLOB)), 0)
                 + COALESCE(length(CAST(submitted_mapping_hash AS BLOB)), 0)
                 + length(CAST(created_at AS BLOB))
                 + length(CAST(updated_at AS BLOB))
                 + COALESCE(length(CAST(submitted_at AS BLOB)), 0)
                 + COALESCE(length(CAST(approved_at AS BLOB)), 0) > ?1",
            None,
        ),
        (
            "draft revisions",
            "SELECT COUNT(*) FROM engagement_mapping_draft_revisions
             WHERE length(CAST(canonical_draft_json AS BLOB)) NOT BETWEEN 2 AND ?1
                OR length(CAST(draft_id AS BLOB))
                 + length(CAST(revision_hash AS BLOB))
                 + COALESCE(length(CAST(parent_revision_hash AS BLOB)), 0)
                 + length(CAST(actor AS BLOB))
                 + length(CAST(created_at AS BLOB)) > ?2",
            Some(i64::try_from(MAXIMUM_COMMAND_BYTES).unwrap_or(i64::MAX)),
        ),
        (
            "workspace attempts",
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE length(CAST(canonical_request_json AS BLOB)) NOT BETWEEN 2 AND ?1
                OR length(CAST(attempt_id AS BLOB))
                 + length(CAST(engagement_id AS BLOB))
                 + length(CAST(draft_id AS BLOB))
                 + length(CAST(command_kind AS BLOB))
                 + length(CAST(idempotency_key AS BLOB))
                 + length(CAST(request_hash AS BLOB))
                 + length(CAST(expected_state AS BLOB))
                 + length(CAST(result_state AS BLOB))
                 + length(CAST(result_reference AS BLOB))
                 + length(CAST(result_hash AS BLOB))
                 + length(CAST(created_at AS BLOB)) > ?2",
            Some(i64::try_from(MAXIMUM_COMMAND_BYTES).unwrap_or(i64::MAX)),
        ),
        (
            "mapping approvals",
            "SELECT COUNT(*) FROM engagement_mapping_approvals
             WHERE length(CAST(canonical_approval_json AS BLOB)) NOT BETWEEN 2 AND ?1
                OR length(CAST(approval_id AS BLOB))
                 + length(CAST(attempt_id AS BLOB))
                 + length(CAST(engagement_id AS BLOB))
                 + length(CAST(draft_id AS BLOB))
                 + length(CAST(draft_revision_hash AS BLOB))
                 + length(CAST(mapping_id AS BLOB))
                 + length(CAST(mapping_version AS BLOB))
                 + length(CAST(mapping_hash AS BLOB))
                 + length(CAST(approver AS BLOB))
                 + length(CAST(reason AS BLOB))
                 + length(CAST(acknowledgement AS BLOB))
                 + length(CAST(approval_hash AS BLOB))
                 + length(CAST(approved_at AS BLOB)) > ?2",
            Some(i64::try_from(MAXIMUM_MAPPING_APPROVAL_BYTES).unwrap_or(i64::MAX)),
        ),
        (
            "mapping preparations",
            "SELECT COUNT(*) FROM engagement_mapping_preparations p
             WHERE EXISTS (
                 SELECT 1 FROM engagement_mapping_draft_heads h
                 WHERE h.engagement_id = p.engagement_id
                   AND h.submitted_mapping_id = p.mapping_id
                   AND h.submitted_mapping_version = p.mapping_version
                   AND h.submitted_mapping_hash = p.mapping_hash
             )
               AND (
                   length(CAST(document_json AS BLOB)) NOT BETWEEN 2 AND ?1
                   OR length(CAST(preparation_id AS BLOB))
                    + length(CAST(engagement_id AS BLOB))
                    + length(CAST(mapping_id AS BLOB))
                    + length(CAST(mapping_version AS BLOB))
                    + length(CAST(mapping_hash AS BLOB))
                    + length(CAST(job_id AS BLOB))
                    + length(CAST(source_hash AS BLOB))
                    + length(CAST(profile_id AS BLOB))
                    + length(CAST(profile_version AS BLOB))
                    + length(CAST(profile_hash AS BLOB))
                    + length(CAST(created_at AS BLOB)) > ?2
               )",
            Some(i64::try_from(MAXIMUM_COMMAND_BYTES).unwrap_or(i64::MAX)),
        ),
    ];
    let mut safe = true;
    for (label, sql, canonical_bound) in checks {
        let invalid: i64 = if let Some(canonical_bound) = canonical_bound {
            connection.query_row(
                sql,
                params![canonical_bound, MAXIMUM_MAPPING_AUTHORITY_METADATA_BYTES],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(sql, [MAXIMUM_MAPPING_AUTHORITY_METADATA_BYTES], |row| {
                row.get(0)
            })?
        };
        if invalid != 0 {
            safe = false;
            issues.push(
                mapping_authority_integrity(format!(
                    "{invalid} {label} row(s) exceed canonical/metadata bounds"
                ))
                .to_string(),
            );
        }
    }
    for table in [
        "engagement_mapping_draft_heads",
        "engagement_mapping_draft_revisions",
        "engagement_mapping_workspace_attempts",
        "engagement_mapping_approvals",
    ] {
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if count > MAXIMUM_MAPPING_AUTHORITY_ROWS {
            safe = false;
            issues.push(
                mapping_authority_integrity(format!(
                    "{table} has {count} rows; bounded replay limit is {MAXIMUM_MAPPING_AUTHORITY_ROWS}"
                ))
                .to_string(),
            );
        }
    }
    Ok(safe)
}

fn replay_canonical_authority_json<T>(
    canonical_json: &str,
    expected_hash: &str,
    maximum_bytes: usize,
    label: &str,
) -> Result<T>
where
    T: DeserializeOwned + Serialize,
{
    if !(2..=maximum_bytes).contains(&canonical_json.len()) {
        return Err(mapping_authority_integrity(format!(
            "{label} canonical bytes exceed their stored bound"
        )));
    }
    if !is_canonical_sha256(expected_hash) {
        return Err(mapping_authority_integrity(format!(
            "{label} has an invalid SHA-256 identity"
        )));
    }
    let value: Value = serde_json::from_str(canonical_json).map_err(|error| {
        mapping_authority_integrity(format!("{label} JSON cannot be parsed: {error}"))
    })?;
    let replayed = canonical_json_string(&value).map_err(|error| {
        mapping_authority_integrity(format!("{label} JSON cannot be canonicalized: {error}"))
    })?;
    if replayed != canonical_json {
        return Err(mapping_authority_integrity(format!(
            "{label} bytes are not canonical JSON"
        )));
    }
    if sha256_hex(canonical_json.as_bytes()) != expected_hash {
        return Err(mapping_authority_integrity(format!(
            "{label} canonical hash mismatch"
        )));
    }
    let typed: T = serde_json::from_value(value).map_err(|error| {
        mapping_authority_integrity(format!("{label} typed replay failed: {error}"))
    })?;
    let typed_json = canonical_json_string(&serde_json::to_value(&typed).map_err(|error| {
        mapping_authority_integrity(format!("{label} typed serialization failed: {error}"))
    })?)
    .map_err(|error| {
        mapping_authority_integrity(format!("{label} typed canonicalization failed: {error}"))
    })?;
    if typed_json != canonical_json {
        return Err(mapping_authority_integrity(format!(
            "{label} typed replay changed its canonical bytes"
        )));
    }
    Ok(typed)
}

fn load_mapping_authority_head(
    connection: &Connection,
    draft_id: &str,
) -> Result<MappingAuthorityHeadRow> {
    connection
        .query_row(
            "SELECT draft_id, engagement_id, source_job_id, source_hash,
                    object_pack_id, object_pack_version,
                    profile_id, profile_version, profile_hash,
                    generation, state, head_revision_hash,
                    submitted_mapping_id, submitted_mapping_version,
                    submitted_mapping_hash, submitted_at, approved_at
             FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [draft_id],
            |row| {
                let generation = row.get::<_, i64>(9)?;
                Ok(MappingAuthorityHeadRow {
                    draft_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    source_job_id: row.get(2)?,
                    source_hash: row.get(3)?,
                    object_pack_id: row.get(4)?,
                    object_pack_version: row.get(5)?,
                    profile_id: row.get(6)?,
                    profile_version: row.get(7)?,
                    profile_hash: row.get(8)?,
                    generation: u64::try_from(generation).unwrap_or(u64::MAX),
                    state: row.get(10)?,
                    head_revision_hash: row.get(11)?,
                    submitted_mapping_id: row.get(12)?,
                    submitted_mapping_version: row.get(13)?,
                    submitted_mapping_hash: row.get(14)?,
                    submitted_at: row.get(15)?,
                    approved_at: row.get(16)?,
                })
            },
        )
        .map_err(Into::into)
}

fn load_mapping_integrity_attempt(
    connection: &Connection,
    attempt_id: &str,
) -> Result<MappingIntegrityAttemptRow> {
    connection
        .query_row(
            "SELECT attempt_id, engagement_id, draft_id, command_kind,
                    idempotency_key, request_hash, canonical_request_json,
                    expected_generation, expected_state,
                    result_generation, result_state, result_reference,
                    result_hash, created_at
             FROM engagement_mapping_workspace_attempts
             WHERE attempt_id = ?1",
            [attempt_id],
            |row| {
                let expected_generation = row.get::<_, i64>(7)?;
                let result_generation = row.get::<_, i64>(9)?;
                Ok(MappingIntegrityAttemptRow {
                    attempt_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    draft_id: row.get(2)?,
                    command_kind: row.get(3)?,
                    idempotency_key: row.get(4)?,
                    request_hash: row.get(5)?,
                    canonical_request_json: row.get(6)?,
                    expected_generation: u64::try_from(expected_generation).unwrap_or(u64::MAX),
                    expected_state: row.get(8)?,
                    result_generation: u64::try_from(result_generation).unwrap_or(u64::MAX),
                    result_state: row.get(10)?,
                    result_reference: row.get(11)?,
                    result_hash: row.get(12)?,
                    created_at: row.get(13)?,
                })
            },
        )
        .map_err(Into::into)
}

fn load_mapping_approval_row(
    connection: &Connection,
    approval_id: &str,
) -> Result<MappingApprovalRow> {
    connection
        .query_row(
            "SELECT approval_id, attempt_id, engagement_id, draft_id,
                    draft_generation, draft_revision_hash,
                    mapping_id, mapping_version, mapping_hash,
                    approver, reason, acknowledgement,
                    canonical_approval_json, approval_hash, approved_at
             FROM engagement_mapping_approvals
             WHERE approval_id = ?1",
            [approval_id],
            |row| {
                let draft_generation = row.get::<_, i64>(4)?;
                Ok(MappingApprovalRow {
                    approval_id: row.get(0)?,
                    attempt_id: row.get(1)?,
                    engagement_id: row.get(2)?,
                    draft_id: row.get(3)?,
                    draft_generation: u64::try_from(draft_generation).unwrap_or(u64::MAX),
                    draft_revision_hash: row.get(5)?,
                    mapping_id: row.get(6)?,
                    mapping_version: row.get(7)?,
                    mapping_hash: row.get(8)?,
                    approver: row.get(9)?,
                    reason: row.get(10)?,
                    acknowledgement: row.get(11)?,
                    canonical_approval_json: row.get(12)?,
                    approval_hash: row.get(13)?,
                    approved_at: row.get(14)?,
                })
            },
        )
        .map_err(Into::into)
}

#[allow(clippy::too_many_lines)]
fn verify_mapping_preparation(
    connection: &Connection,
    head: &MappingAuthorityHeadRow,
    expected_preparation_id: Option<&str>,
) -> Result<()> {
    let (Some(mapping_id), Some(mapping_version), Some(mapping_hash)) = (
        head.submitted_mapping_id.as_deref(),
        head.submitted_mapping_version.as_deref(),
        head.submitted_mapping_hash.as_deref(),
    ) else {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} lacks its submitted mapping identity",
            head.draft_id
        )));
    };
    let preparation: Option<(String, String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT preparation_id, document_json, job_id, source_hash,
                    profile_id, profile_version, profile_hash
             FROM engagement_mapping_preparations
             WHERE engagement_id = ?1
               AND mapping_id = ?2 AND mapping_version = ?3 AND mapping_hash = ?4",
            params![
                head.engagement_id,
                mapping_id,
                mapping_version,
                mapping_hash
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
                ))
            },
        )
        .optional()?;
    let Some((
        preparation_id,
        document_json,
        job_id,
        source_hash,
        profile_id,
        profile_version,
        profile_hash,
    )) = preparation
    else {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} has no exact immutable mapping preparation",
            head.draft_id
        )));
    };
    if !stored_text_is_bounded(&preparation_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&job_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !is_canonical_sha256(&source_hash)
        || !stored_text_is_bounded(&profile_id, 1, 128)
        || !stored_text_is_bounded(&profile_version, 1, 32)
        || !is_canonical_sha256(&profile_hash)
        || expected_preparation_id.is_some_and(|expected| expected != preparation_id)
        || job_id != head.source_job_id
        || source_hash != head.source_hash
        || profile_id != head.profile_id
        || profile_version != head.profile_version
        || profile_hash != head.profile_hash
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} mapping preparation differs from its source/profile/result identity",
            head.draft_id
        )));
    }
    let document: MappingDocument = replay_canonical_authority_json(
        &document_json,
        mapping_hash,
        MAXIMUM_COMMAND_BYTES,
        "submitted mapping",
    )?;
    if document.mapping_id != mapping_id
        || document.version != mapping_version
        || document.profile.profile_id != head.profile_id
        || document.profile.version != head.profile_version
        || document.profile.sha256 != head.profile_hash
        || document.source.package_sha256 != head.source_hash
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} submitted mapping bytes differ from registered authority",
            head.draft_id
        )));
    }
    let registered: Option<(String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT validated_job_id, source_hash,
                    profile_id, profile_version, profile_hash, document_json
             FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2 AND mapping_hash = ?3",
            params![mapping_id, mapping_version, mapping_hash],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((
        registered_job,
        registered_source,
        registered_profile,
        registered_version,
        registered_hash,
        registered_json,
    )) = registered
    else {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} submitted mapping is not registered",
            head.draft_id
        )));
    };
    if registered_job != head.source_job_id
        || registered_source != head.source_hash
        || registered_profile != head.profile_id
        || registered_version != head.profile_version
        || registered_hash != head.profile_hash
        || registered_json != document_json
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} mapping preparation differs from mapping-version evidence",
            head.draft_id
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_active_mapping_head(
    connection: &Connection,
    head: &MappingAuthorityHeadRow,
    require_active: bool,
) -> Result<()> {
    let active = connection.query_row(
        "SELECT active_job_id, selected_object_pack_id,
                selected_profile_id, selected_profile_version,
                selected_profile_hash, active_mapping_id,
                active_mapping_version, active_mapping_hash, mapping_stage
         FROM engagement_active_context
         WHERE engagement_id = ?1",
        [&head.engagement_id],
        |row| {
            Ok(MappingActiveAuthorityRow {
                job_id: row.get(0)?,
                object_pack_id: row.get(1)?,
                profile_id: row.get(2)?,
                profile_version: row.get(3)?,
                profile_hash: row.get(4)?,
                mapping_id: row.get(5)?,
                mapping_version: row.get(6)?,
                mapping_hash: row.get(7)?,
                mapping_stage: row.get(8)?,
            })
        },
    )?;
    let head_mapping = (
        head.submitted_mapping_id.as_deref(),
        head.submitted_mapping_version.as_deref(),
        head.submitted_mapping_hash.as_deref(),
    );
    let active_mapping = (
        active.mapping_id.as_deref(),
        active.mapping_version.as_deref(),
        active.mapping_hash.as_deref(),
    );
    if !require_active {
        return Ok(());
    }
    if active_mapping != head_mapping {
        return Err(mapping_authority_integrity(format!(
            "approved draft {:?} is not the exact active mapping identity",
            head.draft_id
        )));
    }
    let expected_stage = if head.state == "APPROVED" {
        "VALIDATED"
    } else {
        "PREPARED"
    };
    let active_job_matches_mapping = active.job_id.as_deref() == Some(head.source_job_id.as_str());
    let active_job_has_exact_reuse = if active_job_matches_mapping {
        false
    } else if let (
        Some(active_job_id),
        Some(mapping_id),
        Some(mapping_version),
        Some(mapping_hash),
    ) = (
        active.job_id.as_deref(),
        head.submitted_mapping_id.as_deref(),
        head.submitted_mapping_version.as_deref(),
        head.submitted_mapping_hash.as_deref(),
    ) {
        let reuse: Option<(String, String, String)> = connection
            .query_row(
                "SELECT v.source_hash, v.source_reuse_id, v.source_reuse_hash
                 FROM engagement_active_context c
                 JOIN validation_runs v ON v.id = c.active_validation_run_id
                 WHERE c.engagement_id = ?1 AND v.job_id = ?2
                   AND v.mapping_id = ?3 AND v.mapping_version = ?4
                   AND v.mapping_hash = ?5
                   AND v.state = 'COMPLETE'
                   AND v.source_reuse_id IS NOT NULL
                   AND v.source_reuse_hash IS NOT NULL",
                params![
                    head.engagement_id,
                    active_job_id,
                    mapping_id,
                    mapping_version,
                    mapping_hash,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        reuse.is_some_and(|(source_hash, source_reuse_id, source_reuse_hash)| {
            crate::validation::verify_mapping_source_reuse_authority(
                connection,
                &source_reuse_id,
                &source_reuse_hash,
                active_job_id,
                &source_hash,
                &head.profile_id,
                &head.profile_version,
                &head.profile_hash,
                mapping_id,
                mapping_version,
                mapping_hash,
            )
            .is_ok()
        })
    } else {
        false
    };
    if (!active_job_matches_mapping && !active_job_has_exact_reuse)
        || active.object_pack_id.as_deref() != Some(head.object_pack_id.as_str())
        || active.profile_id.as_deref() != Some(head.profile_id.as_str())
        || active.profile_version.as_deref() != Some(head.profile_version.as_str())
        || active.profile_hash.as_deref() != Some(head.profile_hash.as_str())
        || active.mapping_stage != expected_stage
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} for pack {:?}@{:?} differs from its active job/pack/profile/mapping stage",
            head.draft_id, head.object_pack_id, head.object_pack_version
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_mapping_authority_head(
    connection: &Connection,
    head: &MappingAuthorityHeadRow,
) -> Result<()> {
    if !stored_text_is_bounded(&head.draft_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&head.engagement_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&head.source_job_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&head.object_pack_id, 1, 128)
        || !stored_text_is_bounded(&head.object_pack_version, 1, 32)
        || !stored_text_is_bounded(&head.profile_id, 1, 128)
        || !stored_text_is_bounded(&head.profile_version, 1, 32)
        || !is_canonical_sha256(&head.source_hash)
        || !is_canonical_sha256(&head.profile_hash)
        || !is_canonical_sha256(&head.head_revision_hash)
        || head.generation == 0
        || head.generation > MAXIMUM_MAPPING_DRAFT_REVISIONS
        || head
            .submitted_mapping_id
            .as_deref()
            .is_some_and(|value| !stored_text_is_bounded(value, 1, 128))
        || head
            .submitted_mapping_version
            .as_deref()
            .is_some_and(|value| !stored_text_is_bounded(value, 1, 32))
        || head
            .submitted_mapping_hash
            .as_deref()
            .is_some_and(|value| !is_canonical_sha256(value))
        || head
            .submitted_at
            .as_deref()
            .is_some_and(|value| !stored_text_is_bounded(value, 1, 64))
        || head
            .approved_at
            .as_deref()
            .is_some_and(|value| !stored_text_is_bounded(value, 1, 64))
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} metadata is outside the bounded replay contract",
            head.draft_id
        )));
    }
    let mut statement = connection.prepare(
        "SELECT generation, revision_hash, parent_revision_hash,
                canonical_draft_json, actor
         FROM engagement_mapping_draft_revisions
         WHERE draft_id = ?1 ORDER BY generation",
    )?;
    let revisions = statement
        .query_map([&head.draft_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if revisions.len() != usize::try_from(head.generation).unwrap_or(usize::MAX) {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} revision chain is not contiguous through its head",
            head.draft_id
        )));
    }
    let mut parent_hash: Option<String> = None;
    let mut carry_forward_origin = false;
    for (index, (generation, revision_hash, stored_parent, canonical_json, actor)) in
        revisions.iter().enumerate()
    {
        let expected_generation = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if *generation != expected_generation || *stored_parent != parent_hash {
            return Err(mapping_authority_integrity(format!(
                "draft {:?} revision {} has a broken generation/parent link",
                head.draft_id,
                index + 1
            )));
        }
        if !stored_text_is_bounded(actor, 1, MAXIMUM_IDENTIFIER_BYTES)
            || stored_parent
                .as_deref()
                .is_some_and(|hash| !is_canonical_sha256(hash))
        {
            return Err(mapping_authority_integrity(format!(
                "draft {:?} revision {} metadata is outside its stored bound",
                head.draft_id,
                index + 1
            )));
        }
        let snapshot: MappingDraftSnapshot = replay_canonical_authority_json(
            canonical_json,
            revision_hash,
            MAXIMUM_COMMAND_BYTES,
            "mapping draft revision",
        )?;
        if snapshot.author.as_deref() != Some(actor.as_str()) {
            return Err(mapping_authority_integrity(format!(
                "draft {:?} revision {} actor differs from canonical draft bytes",
                head.draft_id,
                index + 1
            )));
        }
        parent_hash = Some(revision_hash.clone());
        let save_attempt_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE draft_id = ?1 AND command_kind = 'SAVE_DRAFT'
               AND result_generation = ?2 AND result_state = 'EDITABLE'
               AND result_reference = ?1 AND result_hash = ?3",
            params![head.draft_id, generation, revision_hash],
            |row| row.get(0),
        )?;
        let carry_origin_count = if index == 0
            && crate::destination::verify_mapping_carry_forward_draft_origin(
                connection,
                &head.draft_id,
                revision_hash,
            )? {
            carry_forward_origin = true;
            1
        } else {
            0
        };
        if save_attempt_count + carry_origin_count != 1 {
            return Err(mapping_authority_integrity(format!(
                "draft {:?} revision {} has {} authoritative creation results; expected one",
                head.draft_id,
                index + 1,
                save_attempt_count + carry_origin_count
            )));
        }
    }
    if parent_hash.as_deref() != Some(head.head_revision_hash.as_str()) {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} head hash differs from the terminal revision",
            head.draft_id
        )));
    }
    let total_save_attempts: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
         WHERE draft_id = ?1 AND command_kind = 'SAVE_DRAFT'",
        [&head.draft_id],
        |row| row.get(0),
    )?;
    if total_save_attempts + i64::from(carry_forward_origin)
        != i64::try_from(head.generation).unwrap_or(i64::MAX)
    {
        return Err(mapping_authority_integrity(format!(
            "draft {:?} creation-result cardinality differs from its generation chain",
            head.draft_id
        )));
    }
    let approval_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_mapping_approvals
         WHERE draft_id = ?1 AND draft_generation = ?2",
        params![
            head.draft_id,
            i64::try_from(head.generation).unwrap_or(i64::MAX)
        ],
        |row| row.get(0),
    )?;
    let submit_attempt_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
         WHERE draft_id = ?1 AND command_kind = 'SUBMIT_MAPPING'",
        [&head.draft_id],
        |row| row.get(0),
    )?;
    let approve_attempt_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
         WHERE draft_id = ?1 AND command_kind = 'APPROVE_MAPPING'",
        [&head.draft_id],
        |row| row.get(0),
    )?;
    match head.state.as_str() {
        "EDITABLE" => {
            if head.submitted_mapping_id.is_some()
                || head.submitted_mapping_version.is_some()
                || head.submitted_mapping_hash.is_some()
                || head.submitted_at.is_some()
                || head.approved_at.is_some()
                || approval_count != 0
                || submit_attempt_count != 0
                || approve_attempt_count != 0
            {
                return Err(mapping_authority_integrity(format!(
                    "editable draft {:?} carries terminal authority evidence",
                    head.draft_id
                )));
            }
        }
        "SUBMITTED" | "APPROVED" => {
            verify_mapping_preparation(connection, head, None)?;
            verify_active_mapping_head(connection, head, false)?;
            let expected_approvals = i64::from(head.state == "APPROVED");
            if approval_count != expected_approvals
                || submit_attempt_count != 1
                || approve_attempt_count != expected_approvals
                || (head.state == "SUBMITTED" && head.approved_at.is_some())
                || (head.state == "APPROVED" && head.approved_at.is_none())
            {
                return Err(mapping_authority_integrity(format!(
                    "draft {:?} state differs from its approval evidence",
                    head.draft_id
                )));
            }
        }
        _ => {
            return Err(mapping_authority_integrity(format!(
                "draft {:?} has unknown state {:?}",
                head.draft_id, head.state
            )));
        }
    }
    let mut attempt_statement = connection.prepare(
        "SELECT attempt_id FROM engagement_mapping_workspace_attempts
         WHERE draft_id = ?1 ORDER BY created_at, attempt_id",
    )?;
    let attempt_ids = attempt_statement
        .query_map([&head.draft_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(attempt_statement);
    for attempt_id in attempt_ids {
        let attempt = load_mapping_integrity_attempt(connection, &attempt_id)?;
        verify_mapping_workspace_attempt(connection, &attempt)?;
    }
    Ok(())
}

fn verify_save_attempt(
    connection: &Connection,
    attempt: &MappingIntegrityAttemptRow,
) -> Result<()> {
    let command: SaveMappingDraftCommand = replay_canonical_authority_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        MAXIMUM_COMMAND_BYTES,
        "SAVE_DRAFT request",
    )?;
    let expected_generation = command.expected_draft_generation.unwrap_or(0);
    let expected_state = if expected_generation == 0 {
        "ABSENT"
    } else {
        "EDITABLE"
    };
    if command.schema_version != SAVE_MAPPING_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != attempt.idempotency_key
        || command
            .expected_draft_id
            .as_deref()
            .is_some_and(|draft_id| draft_id != attempt.draft_id)
        || expected_generation != attempt.expected_generation
        || expected_state != attempt.expected_state
        || attempt.result_generation != expected_generation.saturating_add(1)
        || attempt.result_state != "EDITABLE"
        || attempt.result_reference != attempt.draft_id
    {
        return Err(mapping_authority_integrity(format!(
            "SAVE_DRAFT attempt {:?} has inconsistent typed command/result links",
            attempt.attempt_id
        )));
    }
    let revision: Option<(String, Option<String>, String)> = connection
        .query_row(
            "SELECT revision_hash, parent_revision_hash, canonical_draft_json
             FROM engagement_mapping_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![
                attempt.draft_id,
                i64::try_from(attempt.result_generation).unwrap_or(i64::MAX)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((revision_hash, parent_revision_hash, canonical_draft_json)) = revision else {
        return Err(mapping_authority_integrity(format!(
            "SAVE_DRAFT attempt {:?} has no result revision",
            attempt.attempt_id
        )));
    };
    let command_snapshot = canonical_json_string(&serde_json::to_value(&command.snapshot)?)
        .map_err(|error| {
            mapping_authority_integrity(format!(
                "SAVE_DRAFT attempt {:?} snapshot cannot be canonicalized: {error}",
                attempt.attempt_id
            ))
        })?;
    if revision_hash != attempt.result_hash
        || canonical_draft_json != command_snapshot
        || parent_revision_hash != command.expected_draft_hash
    {
        return Err(mapping_authority_integrity(format!(
            "SAVE_DRAFT attempt {:?} result differs from its revision chain",
            attempt.attempt_id
        )));
    }
    Ok(())
}

fn verify_submit_attempt(
    connection: &Connection,
    attempt: &MappingIntegrityAttemptRow,
) -> Result<()> {
    let command: SubmitMappingDraftCommand = replay_canonical_authority_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        MAXIMUM_COMMAND_BYTES,
        "SUBMIT_MAPPING request",
    )?;
    if command.schema_version != SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != attempt.idempotency_key
        || command.draft_id != attempt.draft_id
        || command.expected_draft_generation != attempt.expected_generation
        || attempt.result_generation != command.expected_draft_generation
        || attempt.expected_state != "EDITABLE"
        || attempt.result_state != "SUBMITTED"
        || command.acknowledgement != OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT
    {
        return Err(mapping_authority_integrity(format!(
            "SUBMIT_MAPPING attempt {:?} has inconsistent typed command/result links",
            attempt.attempt_id
        )));
    }
    let revision_hash: Option<String> = connection
        .query_row(
            "SELECT revision_hash FROM engagement_mapping_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![
                attempt.draft_id,
                i64::try_from(attempt.result_generation).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )
        .optional()?;
    if revision_hash.as_deref() != Some(command.expected_draft_hash.as_str()) {
        return Err(mapping_authority_integrity(format!(
            "SUBMIT_MAPPING attempt {:?} differs from its exact draft revision",
            attempt.attempt_id
        )));
    }
    let head = load_mapping_authority_head(connection, &attempt.draft_id)?;
    if head.submitted_mapping_id.as_deref() != Some(command.mapping_id.as_str())
        || head.submitted_mapping_version.as_deref() != Some(command.version.as_str())
        || head.submitted_mapping_hash.as_deref() != Some(attempt.result_hash.as_str())
    {
        return Err(mapping_authority_integrity(format!(
            "SUBMIT_MAPPING attempt {:?} differs from its frozen draft head",
            attempt.attempt_id
        )));
    }
    verify_mapping_preparation(connection, &head, Some(&attempt.result_reference))
}

fn verify_approve_attempt(
    connection: &Connection,
    attempt: &MappingIntegrityAttemptRow,
) -> Result<()> {
    let command: ApproveMappingVersionCommand = replay_canonical_authority_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        MAXIMUM_COMMAND_BYTES,
        "APPROVE_MAPPING request",
    )?;
    if command.schema_version != APPROVE_MAPPING_VERSION_COMMAND_SCHEMA
        || command.idempotency_key != attempt.idempotency_key
        || command.draft_id != attempt.draft_id
        || command.expected_draft_generation != attempt.expected_generation
        || attempt.result_generation != command.expected_draft_generation
        || attempt.expected_state != "SUBMITTED"
        || attempt.result_state != "APPROVED"
        || command.acknowledgement != OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT
    {
        return Err(mapping_authority_integrity(format!(
            "APPROVE_MAPPING attempt {:?} has inconsistent typed command/result links",
            attempt.attempt_id
        )));
    }
    let approval: Option<(String, String, String, String, String, u64)> = connection
        .query_row(
            "SELECT approval_id, approval_hash, mapping_id, mapping_version,
                    mapping_hash, draft_generation
             FROM engagement_mapping_approvals
             WHERE attempt_id = ?1",
            [&attempt.attempt_id],
            |row| {
                let generation = row.get::<_, i64>(5)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    u64::try_from(generation).unwrap_or(u64::MAX),
                ))
            },
        )
        .optional()?;
    let Some(approval) = approval else {
        return Err(mapping_authority_integrity(format!(
            "APPROVE_MAPPING attempt {:?} has no approval result",
            attempt.attempt_id
        )));
    };
    if approval.0 != attempt.result_reference
        || approval.1 != attempt.result_hash
        || approval.2 != command.expected_mapping_id
        || approval.3 != command.expected_mapping_version
        || approval.4 != command.expected_mapping_hash
        || approval.5 != command.expected_draft_generation
    {
        return Err(mapping_authority_integrity(format!(
            "APPROVE_MAPPING attempt {:?} differs from its approval row",
            attempt.attempt_id
        )));
    }
    Ok(())
}

fn verify_mapping_workspace_attempt(
    connection: &Connection,
    attempt: &MappingIntegrityAttemptRow,
) -> Result<()> {
    if !stored_text_is_bounded(&attempt.attempt_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&attempt.engagement_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&attempt.draft_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&attempt.idempotency_key, 16, 128)
        || !is_canonical_sha256(&attempt.request_hash)
        || !stored_text_is_bounded(&attempt.result_reference, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !is_canonical_sha256(&attempt.result_hash)
        || !stored_text_is_bounded(&attempt.created_at, 1, 64)
        || attempt.result_generation == 0
        || attempt.result_generation > MAXIMUM_MAPPING_DRAFT_REVISIONS
        || attempt.expected_generation > MAXIMUM_MAPPING_DRAFT_REVISIONS
    {
        return Err(mapping_authority_integrity(format!(
            "mapping attempt {:?} metadata is outside the bounded replay contract",
            attempt.attempt_id
        )));
    }
    let head = load_mapping_authority_head(connection, &attempt.draft_id)?;
    if head.engagement_id != attempt.engagement_id {
        return Err(mapping_authority_integrity(format!(
            "mapping attempt {:?} crosses engagement authority",
            attempt.attempt_id
        )));
    }
    match attempt.command_kind.as_str() {
        "SAVE_DRAFT" => verify_save_attempt(connection, attempt),
        "SUBMIT_MAPPING" => verify_submit_attempt(connection, attempt),
        "APPROVE_MAPPING" => verify_approve_attempt(connection, attempt),
        _ => Err(mapping_authority_integrity(format!(
            "mapping attempt {:?} has unknown command kind {:?}",
            attempt.attempt_id, attempt.command_kind
        ))),
    }
}

#[allow(clippy::too_many_lines)]
fn verify_mapping_approval(
    connection: &Connection,
    approval: &MappingApprovalRow,
    require_active: bool,
) -> Result<()> {
    if !stored_text_is_bounded(&approval.approval_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&approval.attempt_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&approval.engagement_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&approval.draft_id, 1, MAXIMUM_IDENTIFIER_BYTES)
        || approval.draft_generation == 0
        || approval.draft_generation > MAXIMUM_MAPPING_DRAFT_REVISIONS
        || !is_canonical_sha256(&approval.draft_revision_hash)
        || !stored_text_is_bounded(&approval.mapping_id, 1, 128)
        || !stored_text_is_bounded(&approval.mapping_version, 1, 32)
        || !is_canonical_sha256(&approval.mapping_hash)
        || !stored_text_is_bounded(&approval.approver, 1, MAXIMUM_IDENTIFIER_BYTES)
        || !stored_text_is_bounded(&approval.reason, 1, MAXIMUM_REASON_BYTES)
        || !is_canonical_sha256(&approval.approval_hash)
        || !stored_text_is_bounded(&approval.approved_at, 1, 64)
    {
        return Err(mapping_authority_integrity(format!(
            "mapping approval {:?} metadata is outside the bounded replay contract",
            approval.approval_id
        )));
    }
    let evidence: MappingApprovalEvidence = replay_canonical_authority_json(
        &approval.canonical_approval_json,
        &approval.approval_hash,
        MAXIMUM_MAPPING_APPROVAL_BYTES,
        "mapping approval",
    )?;
    if evidence.schema_version != MAPPING_APPROVAL_SCHEMA
        || evidence.approval_id != approval.approval_id
        || evidence.draft_id != approval.draft_id
        || evidence.draft_generation != approval.draft_generation
        || evidence.draft_hash != approval.draft_revision_hash
        || evidence.mapping_id != approval.mapping_id
        || evidence.mapping_version != approval.mapping_version
        || evidence.mapping_hash != approval.mapping_hash
        || evidence.actor != approval.approver
        || evidence.reason != approval.reason
        || evidence.acknowledgement != approval.acknowledgement
        || evidence.approved_at != approval.approved_at
        || approval.acknowledgement != OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT
    {
        return Err(mapping_authority_integrity(format!(
            "mapping approval {:?} JSON differs from its immutable row",
            approval.approval_id
        )));
    }
    let attempt = load_mapping_integrity_attempt(connection, &approval.attempt_id)?;
    verify_approve_attempt(connection, &attempt)?;
    let command: ApproveMappingVersionCommand = replay_canonical_authority_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        MAXIMUM_COMMAND_BYTES,
        "APPROVE_MAPPING request",
    )?;
    if attempt.engagement_id != approval.engagement_id
        || attempt.draft_id != approval.draft_id
        || attempt.result_reference != approval.approval_id
        || attempt.result_hash != approval.approval_hash
        || attempt.created_at != approval.approved_at
        || command.expected_draft_hash != approval.draft_revision_hash
        || command.expected_mapping_id != approval.mapping_id
        || command.expected_mapping_version != approval.mapping_version
        || command.expected_mapping_hash != approval.mapping_hash
        || command.actor != approval.approver
        || command.reason != approval.reason
    {
        return Err(mapping_authority_integrity(format!(
            "mapping approval {:?} differs from its typed attempt",
            approval.approval_id
        )));
    }
    let head = load_mapping_authority_head(connection, &approval.draft_id)?;
    if head.engagement_id != approval.engagement_id
        || head.generation != approval.draft_generation
        || head.head_revision_hash != approval.draft_revision_hash
        || head.state != "APPROVED"
        || head.submitted_mapping_id.as_deref() != Some(approval.mapping_id.as_str())
        || head.submitted_mapping_version.as_deref() != Some(approval.mapping_version.as_str())
        || head.submitted_mapping_hash.as_deref() != Some(approval.mapping_hash.as_str())
        || head.approved_at.as_deref() != Some(approval.approved_at.as_str())
    {
        return Err(mapping_authority_integrity(format!(
            "mapping approval {:?} differs from its approved draft head",
            approval.approval_id
        )));
    }
    verify_mapping_authority_head(connection, &head)?;
    verify_active_mapping_head(connection, &head, require_active)
}

pub(crate) fn verified_operator_mapping_approval_identity(
    connection: &Connection,
    engagement_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<Option<(String, String)>> {
    verify_mapping_authority_guards(connection)?;
    let mut authority_bound_issues = Vec::new();
    if !verify_mapping_authority_storage_bounds(connection, &mut authority_bound_issues)? {
        return Err(mapping_authority_integrity(format!(
            "stored authority bounds failed: {}",
            authority_bound_issues.join("; ")
        )));
    }
    let mut statement = connection.prepare(
        "SELECT approval_id FROM engagement_mapping_approvals
         WHERE engagement_id = ?1
           AND mapping_id = ?2 AND mapping_version = ?3 AND mapping_hash = ?4
         ORDER BY approval_id",
    )?;
    let approval_ids = statement
        .query_map(
            params![engagement_id, mapping_id, mapping_version, mapping_hash],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if approval_ids.is_empty() {
        return Ok(None);
    }
    if approval_ids.len() != 1 {
        return Err(mapping_authority_integrity(format!(
            "active mapping {mapping_id:?} {mapping_version:?} has {} approvals; expected one",
            approval_ids.len()
        )));
    }
    let approval = load_mapping_approval_row(connection, &approval_ids[0])?;
    verify_mapping_approval(connection, &approval, true)?;
    let mut registered_mapping_issues = Vec::new();
    crate::mapping::verify_integrity(connection, &mut registered_mapping_issues)?;
    if let Some(issue) = registered_mapping_issues.first() {
        return Err(mapping_authority_integrity(format!(
            "registered mapping/coverage replay failed: {issue}"
        )));
    }
    Ok(Some((approval.approval_id, approval.approval_hash)))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_operator_mapping_approval_identity(
    connection: &Connection,
    engagement_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    approval_id: &str,
    approval_hash: &str,
    require_active: bool,
) -> Result<()> {
    let approval = load_mapping_approval_row(connection, approval_id)?;
    if approval.engagement_id != engagement_id
        || approval.mapping_id != mapping_id
        || approval.mapping_version != mapping_version
        || approval.mapping_hash != mapping_hash
        || approval.approval_hash != approval_hash
    {
        return Err(mapping_authority_integrity(
            "mapping approval identity differs from the exact requested authority",
        ));
    }
    verify_mapping_approval(connection, &approval, require_active)
}

/// Replay the complete bounded v10 operator-mapping authority graph.
///
/// Semantic mismatches are appended to `issues` so the wider workspace
/// integrity report can continue checking independent evidence families.
pub(crate) fn verify_mapping_authority_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    if let Err(error) = verify_mapping_authority_guards(connection) {
        issues.push(error.to_string());
    }
    if !verify_mapping_authority_storage_bounds(connection, issues)? {
        return Ok(());
    }
    let mut head_statement = connection
        .prepare("SELECT draft_id FROM engagement_mapping_draft_heads ORDER BY draft_id")?;
    let head_ids = head_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(head_statement);
    for draft_id in head_ids {
        let result = load_mapping_authority_head(connection, &draft_id)
            .and_then(|head| verify_mapping_authority_head(connection, &head));
        if let Err(error) = result {
            issues.push(error.to_string());
        }
    }

    let mut attempt_statement = connection.prepare(
        "SELECT attempt_id FROM engagement_mapping_workspace_attempts ORDER BY attempt_id",
    )?;
    let attempt_ids = attempt_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(attempt_statement);
    for attempt_id in attempt_ids {
        let result = load_mapping_integrity_attempt(connection, &attempt_id)
            .and_then(|attempt| verify_mapping_workspace_attempt(connection, &attempt));
        if let Err(error) = result {
            issues.push(error.to_string());
        }
    }

    let mut approval_statement = connection
        .prepare("SELECT approval_id FROM engagement_mapping_approvals ORDER BY approval_id")?;
    let approval_ids = approval_statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(approval_statement);
    for approval_id in approval_ids {
        let result = load_mapping_approval_row(connection, &approval_id)
            .and_then(|approval| verify_mapping_approval(connection, &approval, false));
        if let Err(error) = result {
            issues.push(error.to_string());
        }
    }
    Ok(())
}

fn require_active_generation(
    connection: &Connection,
    engagement_id: &str,
    expected: u64,
    label: &str,
) -> Result<()> {
    let generation: i64 = connection.query_row(
        "SELECT generation FROM engagement_active_context WHERE engagement_id = ?1",
        [engagement_id],
        |row| row.get(0),
    )?;
    if u64::try_from(generation).unwrap_or(u64::MAX) != expected {
        return Err(KernelError::Conflict(format!(
            "{label} used a stale engagement generation"
        )));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    validate_bounded_text(value, label, MAXIMUM_IDENTIFIER_BYTES)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
    }) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must use bounded ASCII identifier characters"
        )));
    }
    Ok(())
}

fn validate_mapping_document_id(value: &str, label: &str) -> Result<()> {
    let mut bytes = value.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid_first
        || value.len() > 128
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
    {
        return Err(KernelError::MappingInvalid(format!(
            "{label} must match the immutable mapping identifier vocabulary"
        )));
    }
    Ok(())
}

fn validate_operator_mapping_version(value: &str) -> Result<()> {
    let components = value.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || value.len() > 32
        || components
            .iter()
            .any(|component| component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(KernelError::MappingInvalid(
            "operator mapping version must be numeric major.minor.patch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<()> {
    if !(16..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(KernelError::PolicyInvalid(
            "idempotency key must be 16..=128 bounded ASCII token bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<()> {
    validate_bounded_text(value, "engagement display name", MAXIMUM_DISPLAY_NAME_BYTES)
}

fn validate_source_name(value: &str) -> Result<()> {
    validate_bounded_text(value, "source display name", 512)?;
    if value.contains(['/', '\\']) || value.chars().any(char::is_control) {
        return Err(KernelError::PolicyInvalid(
            "source display name cannot contain paths or control characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must contain 1..={maximum} non-control UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be one lowercase hexadecimal SHA-256"
        )));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must use lowercase hexadecimal"
        )));
    }
    Ok(())
}

fn require_engagement_id(connection: &Connection) -> Result<String> {
    connection
        .query_row(
            "SELECT engagement_id FROM engagement_records WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound("engagement has not been created".to_owned()))
}

fn require_original_source_intake_open(connection: &Connection, engagement_id: &str) -> Result<()> {
    let active_job_id: Option<String> = connection.query_row(
        "SELECT active_job_id FROM engagement_active_context
         WHERE engagement_id = ?1",
        [engagement_id],
        |row| row.get(0),
    )?;
    if active_job_id.is_some() {
        return Err(KernelError::Conflict(
            "original source intake is closed after accepting the active source; use the governed revised-source tour".to_owned(),
        ));
    }
    Ok(())
}

fn is_revised_source_preflight(
    connection: &Connection,
    engagement_id: &str,
    preflight_id: &str,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM engagement_tour_command_attempts
                 WHERE engagement_id = ?1
                   AND command_kind = 'RESERVE_SOURCE'
                   AND tour_attempt_id IS NOT NULL
                   AND result_reference = ?2
             )",
            params![engagement_id, preflight_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn require_source_preflight_mutation_open(
    connection: &Connection,
    engagement_id: &str,
    preflight_id: &str,
) -> Result<()> {
    if is_revised_source_preflight(connection, engagement_id, preflight_id)? {
        Ok(())
    } else {
        require_original_source_intake_open(connection, engagement_id)
    }
}

fn load_command_attempt(
    connection: &Connection,
    engagement_id: &str,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<String>> {
    let existing: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT command_kind, request_hash, result_reference
             FROM engagement_command_attempts
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_kind, stored_hash, result_reference)) = existing else {
        return Ok(None);
    };
    if stored_kind != command_kind || stored_hash != request_hash {
        return Err(KernelError::Conflict(
            "engagement idempotency key was used for a different command".to_owned(),
        ));
    }
    Ok(result_reference)
}

#[allow(clippy::too_many_arguments)]
fn insert_command_attempt(
    transaction: &rusqlite::Transaction<'_>,
    engagement_id: &str,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
    canonical_request_json: &str,
    result_reference: Option<&str>,
    now: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO engagement_command_attempts (
             attempt_id, engagement_id, command_kind, idempotency_key,
             request_hash, canonical_request_json, result_reference, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Uuid::new_v4().to_string(),
            engagement_id,
            command_kind,
            idempotency_key,
            request_hash,
            canonical_request_json,
            result_reference,
            now,
        ],
    )?;
    Ok(())
}

#[allow(clippy::type_complexity, clippy::too_many_lines)]
fn load_engagement_state(connection: &Connection) -> Result<EngagementState> {
    let row: (
        String,
        String,
        i64,
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
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT r.engagement_id, r.display_name, c.generation,
                    c.current_preflight_id, c.active_job_id,
                    c.active_profiling_run_id, c.selected_object_pack_id,
                    c.destination_origin, c.destination_authority_id,
                    c.destination_origin_hash,
                    c.selected_profile_id, c.selected_profile_version,
                    c.selected_profile_hash, c.profile_stage,
                    c.active_mapping_id, c.active_mapping_version,
                    c.active_mapping_hash, c.mapping_stage,
                    c.active_validation_run_id, c.validation_stage, c.updated_at
             FROM engagement_records r
             JOIN engagement_active_context c
               ON c.engagement_id = r.engagement_id
             WHERE r.singleton = 1",
            [],
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
                ))
            },
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound("engagement has not been created".to_owned()))?;
    let active = ActiveEngagementIds {
        preflight_id: row.3.clone(),
        job_id: row.4.clone(),
        profiling_run_id: row.5.clone(),
        object_pack_id: row.6.clone(),
        destination_origin: row.7.clone(),
        destination_authority_id: row.8.clone(),
        destination_origin_hash: row.9.clone(),
        profile_id: row.10.clone(),
        profile_version: row.11.clone(),
        profile_hash: row.12.clone(),
        mapping_id: row.14.clone(),
        mapping_version: row.15.clone(),
        mapping_hash: row.16.clone(),
        validation_run_id: row.18.clone(),
    };
    let phase = derive_phase(
        connection,
        active.preflight_id.as_deref(),
        active.job_id.as_deref(),
        active.profiling_run_id.as_deref(),
        active.validation_run_id.as_deref(),
        &row.13,
        &row.17,
        &row.19,
    )?;
    Ok(EngagementState {
        schema_version: ENGAGEMENT_STATE_SCHEMA.to_owned(),
        engagement_id: row.0,
        display_name: row.1,
        phase,
        generation: u64::try_from(row.2).map_err(|_| {
            KernelError::IntegrityFailed("engagement generation is negative".to_owned())
        })?,
        active,
        profile_stage: row.13,
        mapping_stage: row.17,
        validation_stage: row.19,
        updated_at: row.20,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn derive_phase(
    connection: &Connection,
    preflight_id: Option<&str>,
    job_id: Option<&str>,
    profiling_run_id: Option<&str>,
    validation_run_id: Option<&str>,
    profile_stage: &str,
    mapping_stage: &str,
    validation_stage: &str,
) -> Result<EngagementPhase> {
    match validation_stage {
        "COMPLETE" => {
            let run_id = validation_run_id.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "COMPLETE engagement validation has no active validation run".to_owned(),
                )
            })?;
            let evidence: Option<(String, i64, i64)> = connection
                .query_row(
                    "SELECT state, issue_count,
                            (SELECT COUNT(*) FROM validation_issues
                             WHERE run_id = validation_runs.id)
                     FROM validation_runs WHERE id = ?1",
                    [run_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let (run_state, declared_issue_count, actual_issue_count) =
                evidence.ok_or_else(|| {
                    KernelError::IntegrityFailed(format!(
                        "active validation run {run_id} is missing"
                    ))
                })?;
            if run_state != "COMPLETE" {
                return Err(KernelError::IntegrityFailed(format!(
                    "engagement validation is COMPLETE but active run {run_id} is {run_state}"
                )));
            }
            let declared_issue_count = u64::try_from(declared_issue_count).map_err(|_| {
                KernelError::IntegrityFailed(format!(
                    "active validation run {run_id} has a negative issue count"
                ))
            })?;
            let actual_issue_count = u64::try_from(actual_issue_count).map_err(|_| {
                KernelError::IntegrityFailed(format!(
                    "active validation run {run_id} has a negative issue-row count"
                ))
            })?;
            if declared_issue_count != actual_issue_count {
                return Err(KernelError::IntegrityFailed(format!(
                    "active validation run {run_id} declares {declared_issue_count} issues but has {actual_issue_count} issue rows"
                )));
            }
            return if actual_issue_count == 0 {
                Ok(EngagementPhase::ValidationComplete)
            } else {
                Ok(EngagementPhase::FirstIssuesReady)
            };
        }
        "RUNNING" => return Ok(EngagementPhase::Validating),
        "FAILED" | "INTERRUPTED" | "BOUND_EXCEEDED" => {
            return Ok(EngagementPhase::Failed);
        }
        "NOT_LAUNCHED" => {}
        other => {
            return Err(KernelError::IntegrityFailed(format!(
                "unknown engagement validation stage {other:?}"
            )));
        }
    }
    match mapping_stage {
        "VALIDATED" => return Ok(EngagementPhase::ReadyToValidate),
        "PREPARED" => return Ok(EngagementPhase::MappingPrepared),
        "NOT_PREPARED" => {}
        other => {
            return Err(KernelError::IntegrityFailed(format!(
                "unknown engagement mapping stage {other:?}"
            )));
        }
    }
    if let Some(job_id) = job_id {
        let state: String =
            connection.query_row("SELECT state FROM jobs WHERE id = ?1", [job_id], |row| {
                row.get(0)
            })?;
        if state != "COMPLETE" {
            return if matches!(state.as_str(), "FAILED" | "INTERRUPTED") {
                Ok(EngagementPhase::Failed)
            } else {
                Ok(EngagementPhase::Ingesting)
            };
        }
        if profiling_run_id.is_some() {
            // Destination authority is evaluated below once source profiling
            // is complete, allowing either side of the journey to finish first.
        } else {
            return Ok(EngagementPhase::Profiled);
        }
    } else if let Some(preflight_id) = preflight_id {
        let state: String = connection.query_row(
            "SELECT state FROM engagement_source_preflights WHERE preflight_id = ?1",
            [preflight_id],
            |row| row.get(0),
        )?;
        return match state.as_str() {
            "RECEIVING" => Ok(EngagementPhase::PreflightReceiving),
            "READY" => Ok(EngagementPhase::PreflightReady),
            "REJECTED" | "INTERRUPTED" => Ok(EngagementPhase::Failed),
            other => Err(KernelError::IntegrityFailed(format!(
                "unknown engagement preflight state {other:?}"
            ))),
        };
    } else {
        return Ok(EngagementPhase::AwaitingSource);
    }
    match profile_stage {
        "APPROVED" => Ok(EngagementPhase::AwaitingMapping),
        "REVIEWED" => Ok(EngagementPhase::AwaitingProfileApproval),
        "NOT_SELECTED" => Ok(EngagementPhase::AwaitingObjectPack),
        other => Err(KernelError::IntegrityFailed(format!(
            "unknown engagement profile stage {other:?}"
        ))),
    }
}

fn preflight_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PreflightRow> {
    Ok(PreflightRow {
        preflight_id: row.get(0)?,
        state: row.get(1)?,
        source_name: row.get(2)?,
        partial_relative_path: row.get(3)?,
        retained_relative_path: row.get(4)?,
        source_hash: row.get(5)?,
        source_size: row.get(6)?,
        policy_hash: row.get(7)?,
        manifest_json: row.get(8)?,
        manifest_hash: row.get(9)?,
        error_code: row.get(10)?,
        error_message: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        completed_at: row.get(14)?,
    })
}

fn load_preflight(connection: &Connection, preflight_id: &str) -> Result<PreflightRow> {
    connection
        .query_row(
            "SELECT preflight_id, state, source_name, partial_relative_path,
                    retained_relative_path, source_hash, source_size, policy_hash,
                    manifest_json, manifest_hash, error_code, error_message,
                    created_at, updated_at, completed_at
             FROM engagement_source_preflights WHERE preflight_id = ?1",
            [preflight_id],
            preflight_row,
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("source preflight {preflight_id}")))
}

fn load_preflight_by_idempotency(
    connection: &Connection,
    engagement_id: &str,
    idempotency_key: &str,
) -> Result<Option<(PreflightRow, String)>> {
    connection
        .query_row(
            "SELECT preflight_id, state, source_name, partial_relative_path,
                    retained_relative_path, source_hash, source_size, policy_hash,
                    manifest_json, manifest_hash, error_code, error_message,
                    created_at, updated_at, completed_at, request_hash
             FROM engagement_source_preflights
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| Ok((preflight_row(row)?, row.get(15)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn load_preflight_file_intent(
    connection: &Connection,
    preflight_id: &str,
) -> Result<Option<PreflightFileIntent>> {
    load_preflight_event_contract(
        connection,
        preflight_id,
        PREFLIGHT_FILE_INTENT_SCHEMA,
        |value| serde_json::from_value(value).map_err(Into::into),
    )
}

fn load_preflight_publish_intent(
    connection: &Connection,
    preflight_id: &str,
) -> Result<Option<PreflightPublishIntent>> {
    let current = load_preflight_event_contract(
        connection,
        preflight_id,
        PREFLIGHT_PUBLISH_INTENT_SCHEMA,
        |value| serde_json::from_value(value).map_err(Into::into),
    )?;
    let legacy = load_preflight_event_contract(
        connection,
        preflight_id,
        LEGACY_PREFLIGHT_PUBLISH_INTENT_SCHEMA,
        |value| serde_json::from_value::<LegacyPreflightPublishIntent>(value).map_err(Into::into),
    )?;
    match (current, legacy) {
        (Some(_), Some(_)) => Err(KernelError::IntegrityFailed(
            "preflight has multiple publication authority schema generations".to_owned(),
        )),
        (Some(intent), None) => Ok(Some(intent)),
        (None, Some(intent)) => Ok(Some(PreflightPublishIntent {
            schema_version: intent.schema_version,
            retained_relative_path: intent.retained_relative_path,
            source_hash: intent.source_hash,
            source_size: intent.source_size,
            manifest_json: intent.manifest_json,
            manifest_hash: intent.manifest_hash,
            retained_file_identity: intent.partial_file_identity.clone(),
            partial_file_identity: intent.partial_file_identity,
        })),
        (None, None) => Ok(None),
    }
}

fn require_supported_preflight_publish_intent(intent: &PreflightPublishIntent) -> Result<()> {
    if intent.schema_version == PREFLIGHT_PUBLISH_INTENT_SCHEMA
        || (intent.schema_version == LEGACY_PREFLIGHT_PUBLISH_INTENT_SCHEMA
            && intent.retained_file_identity == intent.partial_file_identity)
    {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "preflight publication authority uses an unsupported identity contract".to_owned(),
        ))
    }
}

fn require_preflight_file_intent(row: &PreflightRow, intent: &PreflightFileIntent) -> Result<()> {
    require_partial_preflight_path(row)?;
    if intent.schema_version != PREFLIGHT_FILE_INTENT_SCHEMA
        || intent.relative_path != row.partial_relative_path
    {
        return Err(KernelError::IntegrityFailed(
            "preflight reserved-file authority differs from its controlled path".to_owned(),
        ));
    }
    Ok(())
}

fn require_preflight_publish_contract(
    row: &PreflightRow,
    file_intent: &PreflightFileIntent,
    intent: &PreflightPublishIntent,
) -> Result<()> {
    require_preflight_file_intent(row, file_intent)?;
    require_supported_preflight_publish_intent(intent)?;
    validate_sha256(&intent.source_hash, "preflight publish source hash")?;
    validate_sha256(&intent.manifest_hash, "preflight publish manifest hash")?;
    let expected_retained = format!("engagement/preflights/retained/{}.zip", intent.source_hash);
    if intent.retained_relative_path != expected_retained {
        return Err(KernelError::IntegrityFailed(
            "preflight publication intent has an unsafe retained identity".to_owned(),
        ));
    }
    if intent.partial_file_identity != file_intent.file_identity {
        return Err(KernelError::IntegrityFailed(
            "preflight publication partial identity differs from its durable reservation"
                .to_owned(),
        ));
    }
    let manifest = parse_unique_json(intent.manifest_json.as_bytes())?;
    if canonical_json_string(&manifest)? != intent.manifest_json
        || canonical_hash(&manifest)? != intent.manifest_hash
    {
        return Err(KernelError::IntegrityFailed(
            "preflight publication intent has invalid manifest evidence".to_owned(),
        ));
    }
    if row.state == "READY" {
        let source_size = optional_nonnegative(row.source_size, "preflight source size")?
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "READY preflight has no durable source size".to_owned(),
                )
            })?;
        if row.retained_relative_path.as_deref() != Some(intent.retained_relative_path.as_str())
            || row.source_hash.as_deref() != Some(intent.source_hash.as_str())
            || source_size != intent.source_size
            || row.manifest_json.as_deref() != Some(intent.manifest_json.as_str())
            || row.manifest_hash.as_deref() != Some(intent.manifest_hash.as_str())
        {
            return Err(KernelError::IntegrityFailed(
                "READY preflight differs from its immutable publication authority".to_owned(),
            ));
        }
    }
    Ok(())
}

fn open_optional_controlled_file(
    directory: &ControlledDirectory,
    name: &OsStr,
) -> Result<Option<File>> {
    match open_controlled_file(directory, name, false) {
        Ok(file) => Ok(Some(file)),
        Err(KernelError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn count_declared_shared_acceptance_links(
    workspace: &Path,
    workspace_handle: &File,
    connection: &Connection,
    retained_relative_path: &str,
    source_hash: &str,
    retained_identity: &ControlledFileIdentity,
    create_directory: bool,
) -> Result<u64> {
    let acceptance_rows = {
        let mut statement = connection.prepare(
            "SELECT a.attempt_id, a.controlled_source_relative_path,
                    a.accepted_source_hash, a.accepted_manifest_hash,
                    a.job_id, a.profiling_run_id, p.manifest_hash
             FROM engagement_acceptance_attempts a
             JOIN engagement_source_preflights p
               ON p.preflight_id = a.preflight_id
             WHERE p.state = 'READY' AND p.retained_relative_path = ?1
             ORDER BY a.attempt_id",
        )?;
        statement
            .query_map([retained_relative_path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if acceptance_rows.is_empty() {
        return Ok(0);
    }
    let requires_existing_directory = acceptance_rows
        .iter()
        .any(|row| row.4.is_some() || row.5.is_some());
    let acceptance_directory = match controlled_directory(
        workspace,
        workspace_handle,
        Path::new("engagement/acceptances"),
        create_directory,
    ) {
        Ok(directory) => Some(directory),
        Err(KernelError::Io { source, .. })
            if !create_directory
                && source.kind() == std::io::ErrorKind::NotFound
                && !requires_existing_directory =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let mut seen_paths = BTreeSet::new();
    let mut hard_links = 0_u64;
    for (
        attempt_id,
        relative_path,
        accepted_hash,
        accepted_manifest_hash,
        job_id,
        profiling_run_id,
        preflight_manifest_hash,
    ) in acceptance_rows
    {
        let expected_relative = format!("engagement/acceptances/{attempt_id}.zip");
        if relative_path != expected_relative || !seen_paths.insert(relative_path) {
            return Err(KernelError::IntegrityFailed(
                "shared retained evidence has an unsafe or duplicate acceptance path".to_owned(),
            ));
        }
        if accepted_hash != source_hash
            || preflight_manifest_hash.as_deref() != Some(accepted_manifest_hash.as_str())
        {
            return Err(KernelError::IntegrityFailed(
                "shared retained evidence has an acceptance bound to different source authority"
                    .to_owned(),
            ));
        }
        let Some(directory) = acceptance_directory.as_ref() else {
            continue;
        };
        let name = OsString::from(format!("{attempt_id}.zip"));
        let Some(file) = open_optional_controlled_file(directory, &name)? else {
            if job_id.is_some() || profiling_run_id.is_some() {
                return Err(KernelError::IntegrityFailed(
                    "bound source acceptance lost its controlled source evidence".to_owned(),
                ));
            }
            continue;
        };
        if controlled_file_identity(&file)? == *retained_identity {
            hard_links = hard_links.checked_add(1).ok_or_else(|| {
                KernelError::IntegrityFailed("acceptance hard-link count overflowed".to_owned())
            })?;
        } else {
            require_unaliased_controlled_file(&file)?;
        }
    }
    Ok(hard_links)
}

#[allow(clippy::too_many_lines)]
fn require_declared_retained_aliases_with_connection(
    workspace: &Path,
    workspace_handle: &File,
    connection: &Connection,
    row: &PreflightRow,
    intent: &PreflightPublishIntent,
    retained: &File,
    create_acceptance_directory: bool,
) -> Result<u64> {
    require_supported_preflight_publish_intent(intent)?;
    require_partial_preflight_path(row)?;
    require_controlled_file_identity(retained, &intent.retained_file_identity)?;
    let retained_identity = controlled_file_identity(retained)?;
    let file_intent =
        load_preflight_file_intent(connection, &row.preflight_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "preflight retained authority has no partial-file intent".to_owned(),
            )
        })?;
    require_preflight_file_intent(row, &file_intent)?;
    if file_intent.file_identity != intent.partial_file_identity {
        return Err(KernelError::IntegrityFailed(
            "preflight publication partial identity differs from its durable reservation"
                .to_owned(),
        ));
    }
    EngagementService::require_shared_ready_retained_authorities(
        connection,
        &intent.retained_relative_path,
        &intent.source_hash,
        intent.source_size,
        &retained_identity,
    )?;

    let shared_rows = {
        let mut statement = connection.prepare(
            "SELECT preflight_id, partial_relative_path
             FROM engagement_source_preflights
             WHERE state = 'READY' AND retained_relative_path = ?1
             ORDER BY preflight_id",
        )?;
        statement
            .query_map([&intent.retained_relative_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut declared_links = 1_u64;
    let preflight_directory = controlled_directory(
        workspace,
        workspace_handle,
        Path::new("engagement/preflights"),
        false,
    )?;
    let mut checked_preflights = BTreeSet::new();
    for (preflight_id, partial_relative_path) in shared_rows.into_iter().chain(std::iter::once((
        row.preflight_id.clone(),
        row.partial_relative_path.clone(),
    ))) {
        if !checked_preflights.insert(preflight_id.clone()) {
            continue;
        }
        validate_identifier(&preflight_id, "preflight ID")?;
        let expected_relative = format!("engagement/preflights/{preflight_id}.part");
        if partial_relative_path != expected_relative {
            return Err(KernelError::IntegrityFailed(
                "shared retained preflight has an unsafe partial path".to_owned(),
            ));
        }
        let shared_file_intent = load_preflight_file_intent(connection, &preflight_id)?
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "shared retained preflight has no partial-file authority".to_owned(),
                )
            })?;
        if shared_file_intent.schema_version != PREFLIGHT_FILE_INTENT_SCHEMA
            || shared_file_intent.relative_path != partial_relative_path
        {
            return Err(KernelError::IntegrityFailed(
                "shared retained preflight has invalid partial-file authority".to_owned(),
            ));
        }
        let name = OsString::from(format!("{preflight_id}.part"));
        let Some(partial) = open_optional_controlled_file(&preflight_directory, &name)? else {
            continue;
        };
        require_controlled_file_identity(&partial, &shared_file_intent.file_identity)?;
        if controlled_file_identity(&partial)? == retained_identity {
            declared_links = declared_links.checked_add(1).ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "retained preflight hard-link count overflowed".to_owned(),
                )
            })?;
        } else {
            require_unaliased_controlled_file(&partial)?;
        }
    }

    declared_links = declared_links
        .checked_add(count_declared_shared_acceptance_links(
            workspace,
            workspace_handle,
            connection,
            &intent.retained_relative_path,
            &intent.source_hash,
            &retained_identity,
            create_acceptance_directory,
        )?)
        .ok_or_else(|| {
            KernelError::IntegrityFailed("acceptance hard-link count overflowed".to_owned())
        })?;
    require_exact_link_count(retained, declared_links)?;
    Ok(declared_links)
}

fn verify_ready_preflight_authority(
    workspace: &Path,
    workspace_handle: &File,
    connection: &Connection,
    row: &PreflightRow,
) -> Result<()> {
    if row.state != "READY" {
        return Err(KernelError::IntegrityFailed(
            "retained preflight verification requires READY evidence".to_owned(),
        ));
    }
    let file_intent =
        load_preflight_file_intent(connection, &row.preflight_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "READY preflight has no durable reserved-file authority".to_owned(),
            )
        })?;
    let publish_intent =
        load_preflight_publish_intent(connection, &row.preflight_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "READY preflight has no durable publication authority".to_owned(),
            )
        })?;
    require_preflight_publish_contract(row, &file_intent, &publish_intent)?;
    let retained_directory = controlled_directory(
        workspace,
        workspace_handle,
        Path::new("engagement/preflights/retained"),
        false,
    )?;
    let retained_name = OsString::from(format!("{}.zip", publish_intent.source_hash));
    let retained = open_controlled_file(&retained_directory, &retained_name, false)?;
    require_controlled_file_identity(&retained, &publish_intent.retained_file_identity)?;
    let retained_path = retained_directory.path.join(&retained_name);
    require_read_only_controlled_file(&retained, &retained_path)?;
    let (actual_hash, actual_size) = hash_open_regular_file(&retained, publish_intent.source_size)?;
    if actual_hash != publish_intent.source_hash || actual_size != publish_intent.source_size {
        return Err(KernelError::IntegrityFailed(
            "retained preflight differs from its immutable publication authority".to_owned(),
        ));
    }
    require_declared_retained_aliases_with_connection(
        workspace,
        workspace_handle,
        connection,
        row,
        &publish_intent,
        &retained,
        false,
    )?;
    verify_retained_workspace_root(workspace, workspace_handle)
}

fn verify_receiving_preflight_publication(
    workspace: &Path,
    workspace_handle: &File,
    connection: &Connection,
    row: &PreflightRow,
    file_intent: &PreflightFileIntent,
    publish_intent: &PreflightPublishIntent,
) -> Result<()> {
    require_preflight_publish_contract(row, file_intent, publish_intent)?;
    let preflight_directory = controlled_directory(
        workspace,
        workspace_handle,
        Path::new("engagement/preflights"),
        false,
    )?;
    let partial_name = preflight_partial_name(row);
    let partial = open_optional_controlled_file(&preflight_directory, &partial_name)?;
    if let Some(partial) = partial.as_ref() {
        require_controlled_file_identity(partial, &file_intent.file_identity)?;
        let (actual_hash, actual_size) =
            hash_open_regular_file(partial, publish_intent.source_size)?;
        if actual_hash != publish_intent.source_hash || actual_size != publish_intent.source_size {
            return Err(KernelError::IntegrityFailed(
                "published preflight partial differs from its immutable authority".to_owned(),
            ));
        }
    }

    let retained_directory = controlled_directory(
        workspace,
        workspace_handle,
        Path::new("engagement/preflights/retained"),
        false,
    );
    let retained = match retained_directory {
        Ok(directory) => {
            let name = OsString::from(format!("{}.zip", publish_intent.source_hash));
            open_optional_controlled_file(&directory, &name)?.map(|file| (directory, name, file))
        }
        Err(KernelError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            None
        }
        Err(error) => return Err(error),
    };
    let Some((retained_directory, retained_name, retained)) = retained else {
        if let Some(partial) = partial.as_ref() {
            require_unaliased_controlled_file(partial)?;
            return verify_retained_workspace_root(workspace, workspace_handle);
        }
        return Err(KernelError::IntegrityFailed(
            "preflight publication lost both partial and retained evidence".to_owned(),
        ));
    };
    require_controlled_file_identity(&retained, &publish_intent.retained_file_identity)?;
    let retained_path = retained_directory.path.join(&retained_name);
    require_read_only_controlled_file(&retained, &retained_path)?;
    let (actual_hash, actual_size) = hash_open_regular_file(&retained, publish_intent.source_size)?;
    if actual_hash != publish_intent.source_hash || actual_size != publish_intent.source_size {
        return Err(KernelError::IntegrityFailed(
            "published retained preflight differs from its immutable authority".to_owned(),
        ));
    }
    require_declared_retained_aliases_with_connection(
        workspace,
        workspace_handle,
        connection,
        row,
        publish_intent,
        &retained,
        false,
    )?;
    verify_retained_workspace_root(workspace, workspace_handle)
}

fn require_exact_preflight_history_trigger(
    connection: &Connection,
    name: &str,
    expected_sql: &str,
) -> Result<()> {
    let actual: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    let normalized = actual
        .as_deref()
        .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" "));
    if normalized.as_deref() == Some(expected_sql) {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(format!(
            "preflight immutable-history trigger {name} is missing or altered"
        )))
    }
}

/// Replay engagement preflight filesystem authority without mutating the workspace.
pub(crate) fn verify_preflight_authority_integrity(
    workspace: &Path,
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    for (name, expected_sql) in [
        (
            "engagement_preflight_events_no_update",
            PREFLIGHT_EVENTS_NO_UPDATE_TRIGGER_SQL,
        ),
        (
            "engagement_preflight_events_no_delete",
            PREFLIGHT_EVENTS_NO_DELETE_TRIGGER_SQL,
        ),
    ] {
        if let Err(error) = require_exact_preflight_history_trigger(connection, name, expected_sql)
        {
            issues.push(format!(
                "engagement preflight immutable authority mismatch: {error}"
            ));
        }
    }
    let workspace_handle = retain_workspace_root(workspace)?;
    let preflight_ids = {
        let mut statement = connection.prepare(
            "SELECT preflight_id FROM engagement_source_preflights
             ORDER BY created_at, preflight_id",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for preflight_id in preflight_ids {
        let result = (|| {
            let row = load_preflight(connection, &preflight_id)?;
            let file_intent = load_preflight_file_intent(connection, &preflight_id)?;
            let publish_intent = load_preflight_publish_intent(connection, &preflight_id)?;
            if let Some(intent) = file_intent.as_ref() {
                require_preflight_file_intent(&row, intent)?;
            }
            match row.state.as_str() {
                "READY" => {
                    verify_ready_preflight_authority(
                        workspace,
                        &workspace_handle,
                        connection,
                        &row,
                    )?;
                }
                "RECEIVING" => match (file_intent.as_ref(), publish_intent.as_ref()) {
                    (Some(file_intent), Some(publish_intent)) => {
                        verify_receiving_preflight_publication(
                            workspace,
                            &workspace_handle,
                            connection,
                            &row,
                            file_intent,
                            publish_intent,
                        )?;
                    }
                    (None, Some(_)) => {
                        return Err(KernelError::IntegrityFailed(
                            "published preflight has no reserved-file authority".to_owned(),
                        ));
                    }
                    (Some(file_intent), None) => {
                        let directory = controlled_directory(
                            workspace,
                            &workspace_handle,
                            Path::new("engagement/preflights"),
                            false,
                        )?;
                        let partial =
                            open_controlled_file(&directory, &preflight_partial_name(&row), false)?;
                        require_controlled_file_identity(&partial, &file_intent.file_identity)?;
                        require_unaliased_controlled_file(&partial)?;
                    }
                    (None, None) => {}
                },
                "REJECTED" | "INTERRUPTED" => {
                    if publish_intent.is_some() {
                        return Err(KernelError::IntegrityFailed(
                            "terminal preflight retains unfinished publication authority"
                                .to_owned(),
                        ));
                    }
                }
                state => {
                    return Err(KernelError::IntegrityFailed(format!(
                        "preflight has unsupported durable state {state:?}"
                    )));
                }
            }
            verify_retained_workspace_root(workspace, &workspace_handle)
        })();
        if let Err(error) = result {
            issues.push(format!(
                "engagement preflight {preflight_id} filesystem authority mismatch: {error}"
            ));
        }
    }
    Ok(())
}

fn load_preflight_event_contract<T, F>(
    connection: &Connection,
    preflight_id: &str,
    schema: &str,
    decode: F,
) -> Result<Option<T>>
where
    T: Eq,
    F: Fn(Value) -> Result<T>,
{
    let mut statement = connection.prepare(
        "SELECT detail FROM engagement_preflight_events
         WHERE preflight_id = ?1 AND detail IS NOT NULL
         ORDER BY sequence",
    )?;
    let details = statement
        .query_map([preflight_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut found = None;
    for detail in details {
        let Ok(value) = parse_unique_json(detail.as_bytes()) else {
            continue;
        };
        if value.get("schemaVersion").and_then(Value::as_str) != Some(schema) {
            continue;
        }
        if canonical_json_string(&value)? != detail {
            return Err(KernelError::IntegrityFailed(
                "preflight filesystem intent is not canonical JSON".to_owned(),
            ));
        }
        let decoded = decode(value)?;
        if found.as_ref().is_some_and(|existing| existing != &decoded) {
            return Err(KernelError::IntegrityFailed(
                "preflight has conflicting immutable filesystem intents".to_owned(),
            ));
        }
        found = Some(decoded);
    }
    Ok(found)
}

fn preflight_view(row: PreflightRow) -> Result<SourcePreflightView> {
    let manifest = row
        .manifest_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    Ok(SourcePreflightView {
        schema_version: PREFLIGHT_SCHEMA.to_owned(),
        preflight_id: row.preflight_id,
        state: SourcePreflightState::parse(&row.state)?,
        source_name: row.source_name,
        source_hash: row.source_hash,
        source_size: optional_nonnegative(row.source_size, "preflight source size")?,
        policy_hash: row.policy_hash,
        manifest_hash: row.manifest_hash,
        manifest,
        error_code: row.error_code,
        error_message: row.error_message,
        created_at: row.created_at,
        updated_at: row.updated_at,
        completed_at: row.completed_at,
    })
}

fn require_partial_preflight_path(row: &PreflightRow) -> Result<()> {
    let expected = format!("engagement/preflights/{}.part", row.preflight_id);
    if row.partial_relative_path == expected {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "preflight partial path differs from its server-controlled identity".to_owned(),
        ))
    }
}

fn preflight_partial_name(row: &PreflightRow) -> OsString {
    OsString::from(format!("{}.part", row.preflight_id))
}

fn controlled_preflight_path(workspace: &Path, row: &PreflightRow) -> Result<PathBuf> {
    checked_workspace_path(
        workspace,
        row.retained_relative_path
            .as_deref()
            .unwrap_or(&row.partial_relative_path),
    )
}

fn update_context_preflight(
    transaction: &rusqlite::Transaction<'_>,
    engagement_id: &str,
    preflight_id: &str,
    now: &str,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE engagement_active_context
         SET current_preflight_id = ?1, generation = generation + 1, updated_at = ?2
         WHERE engagement_id = ?3",
        params![preflight_id, now, engagement_id],
    )?;
    require_one(changed, "select source preflight")
}

fn insert_preflight_event(
    transaction: &rusqlite::Transaction<'_>,
    preflight_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO engagement_preflight_events (
             preflight_id, state, error_code, detail, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![preflight_id, state, error_code, detail, occurred_at],
    )?;
    Ok(())
}

fn require_one(changed: usize, action: &str) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "{action} changed {changed} rows"
        )))
    }
}

fn optional_nonnegative(value: Option<i64>, label: &str) -> Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                KernelError::IntegrityFailed(format!("{label} is negative in durable state"))
            })
        })
        .transpose()
}

fn checked_workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(KernelError::IntegrityFailed(format!(
            "engagement-controlled relative path is unsafe: {}",
            relative.display()
        )));
    }
    Ok(workspace.join(relative))
}

fn controlled_directory(
    workspace: &Path,
    workspace_handle: &File,
    relative: &Path,
    create: bool,
) -> Result<ControlledDirectory> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(KernelError::IntegrityFailed(
            "controlled engagement directory has an unsafe relative identity".to_owned(),
        ));
    }
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(component) => Some(component.to_os_string()),
            _ => None,
        })
        .collect::<Vec<OsString>>();
    controlled_directory_platform(workspace, workspace_handle, relative, &components, create)
}

#[cfg(unix)]
fn controlled_directory_platform(
    workspace: &Path,
    workspace_handle: &File,
    relative: &Path,
    components: &[OsString],
    create: bool,
) -> Result<ControlledDirectory> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};

    let mut current = workspace_handle
        .try_clone()
        .map_err(|error| KernelError::io(workspace, error))?;
    let mut named_path = workspace.to_path_buf();
    for component in components {
        named_path.push(component);
        let opened = openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        );
        let descriptor = match opened {
            Ok(descriptor) => descriptor,
            Err(error) if create => {
                let io_error: std::io::Error = error.into();
                if io_error.kind() != std::io::ErrorKind::NotFound {
                    return Err(KernelError::io(&named_path, io_error));
                }
                match mkdirat(&current, component, Mode::RWXU) {
                    Ok(()) => current
                        .sync_all()
                        .map_err(|error| KernelError::io(&named_path, error))?,
                    Err(error) => {
                        let io_error: std::io::Error = error.into();
                        if io_error.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(KernelError::io(&named_path, io_error));
                        }
                    }
                }
                openat(
                    &current,
                    component,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| {
                    let error: std::io::Error = error.into();
                    KernelError::io(&named_path, error)
                })?
            }
            Err(error) => {
                let error: std::io::Error = error.into();
                return Err(KernelError::io(&named_path, error));
            }
        };
        current = File::from(descriptor);
        let metadata = current
            .metadata()
            .map_err(|error| KernelError::io(&named_path, error))?;
        if !metadata.is_dir() {
            return Err(KernelError::IntegrityFailed(
                "controlled engagement directory component is not a real directory".to_owned(),
            ));
        }
    }
    Ok(ControlledDirectory {
        path: workspace.join(relative),
        handle: current,
    })
}

#[cfg(windows)]
fn controlled_directory_platform(
    workspace: &Path,
    _workspace_handle: &File,
    relative: &Path,
    _components: &[OsString],
    create: bool,
) -> Result<ControlledDirectory> {
    let path = if create {
        ensure_workspace_directory(workspace, relative)?
    } else {
        checked_workspace_path(workspace, relative.to_string_lossy().as_ref())?
    };
    verify_workspace_directory_chain(
        workspace,
        relative,
        &workspace
            .canonicalize()
            .map_err(|error| KernelError::io(workspace, error))?,
    )?;
    let handle = crate::durability::open_directory_nofollow(&path)
        .map_err(|error| KernelError::io(&path, error))?;
    let metadata = handle
        .metadata()
        .map_err(|error| KernelError::io(&path, error))?;
    if !metadata.is_dir() || crate::durability::is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "controlled engagement directory is not a real non-reparse directory".to_owned(),
        ));
    }
    Ok(ControlledDirectory { path, handle })
}

#[cfg(not(any(unix, windows)))]
fn controlled_directory_platform(
    _workspace: &Path,
    _workspace_handle: &File,
    _relative: &Path,
    _components: &[OsString],
    _create: bool,
) -> Result<ControlledDirectory> {
    Err(KernelError::IntegrityFailed(
        "controlled engagement filesystem authority is unsupported on this platform".to_owned(),
    ))
}

fn create_new_controlled_file(directory: &ControlledDirectory, name: &OsStr) -> Result<File> {
    create_new_controlled_file_platform(directory, name)
        .map_err(|error| KernelError::io(directory.path.join(name), error))
}

#[cfg(unix)]
fn create_new_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    let descriptor = openat(
        &directory.handle,
        name,
        OFlags::CREATE
            | OFlags::EXCL
            | OFlags::RDWR
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn create_new_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory.path.join(name))
}

#[cfg(not(any(unix, windows)))]
fn create_new_controlled_file_platform(
    _directory: &ControlledDirectory,
    _name: &OsStr,
) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "controlled file creation is unsupported on this platform",
    ))
}

fn open_controlled_file(
    directory: &ControlledDirectory,
    name: &OsStr,
    writable: bool,
) -> Result<File> {
    let file = open_controlled_file_platform(directory, name, writable)
        .map_err(|error| KernelError::io(directory.path.join(name), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(directory.path.join(name), error))?;
    if !metadata.file_type().is_file() || metadata_is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "controlled engagement file is not one real regular non-link file".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
    writable: bool,
) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    let access = if writable {
        OFlags::WRONLY
    } else {
        OFlags::RDONLY
    };
    let descriptor = openat(
        &directory.handle,
        name,
        access | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn open_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
    writable: bool,
) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(!writable)
        .write(writable)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory.path.join(name))
}

#[cfg(not(any(unix, windows)))]
fn open_controlled_file_platform(
    _directory: &ControlledDirectory,
    _name: &OsStr,
    _writable: bool,
) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "controlled file opening is unsupported on this platform",
    ))
}

fn controlled_file_identity(file: &File) -> Result<ControlledFileIdentity> {
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    if !metadata.file_type().is_file() || metadata_is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "controlled engagement identity is not a regular non-link file".to_owned(),
        ));
    }
    #[cfg(windows)]
    {
        controlled_file_identity_platform(file)
    }
    #[cfg(not(windows))]
    {
        controlled_file_identity_platform(&metadata)
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn controlled_file_identity_platform(metadata: &fs::Metadata) -> Result<ControlledFileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    Ok(ControlledFileIdentity {
        schema_version: PREFLIGHT_FILE_INTENT_SCHEMA.to_owned(),
        platform: "unix-dev-inode".to_owned(),
        primary: metadata.dev(),
        secondary: metadata.ino(),
        file_id: None,
    })
}

#[cfg(windows)]
fn controlled_file_identity_platform(file: &File) -> Result<ControlledFileIdentity> {
    use std::fmt::Write as _;

    let identity = migration_stream_windows_native::file_identity_from_open_file(file)
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    let file_id = identity.file_id();
    let mut file_id_hex = String::with_capacity(file_id.len() * 2);
    for byte in file_id {
        write!(&mut file_id_hex, "{byte:02x}").expect("writing into String is infallible");
    }
    Ok(ControlledFileIdentity {
        schema_version: PREFLIGHT_FILE_INTENT_SCHEMA.to_owned(),
        platform: "windows-volume-file-id-v2".to_owned(),
        primary: identity.volume_serial_number(),
        secondary: u64::from_be_bytes(file_id[..8].try_into().expect("FileId prefix is 8 bytes")),
        file_id: Some(file_id_hex),
    })
}

#[cfg(not(any(unix, windows)))]
fn controlled_file_identity_platform(_metadata: &fs::Metadata) -> Result<ControlledFileIdentity> {
    Err(KernelError::IntegrityFailed(
        "controlled file identity is unsupported on this platform".to_owned(),
    ))
}

fn require_controlled_file_identity(file: &File, expected: &ControlledFileIdentity) -> Result<()> {
    if controlled_file_identity(file)? == *expected {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "controlled engagement file identity differs from its durable intent".to_owned(),
        ))
    }
}

fn require_unaliased_controlled_file(file: &File) -> Result<()> {
    require_exact_link_count(file, 1).map_err(|_| {
        KernelError::IntegrityFailed(
            "controlled engagement file has an undeclared hard-link alias".to_owned(),
        )
    })
}

fn require_exact_link_count(file: &File, expected: u64) -> Result<()> {
    require_exact_link_count_platform(file, expected)
}

fn require_read_only_controlled_file(file: &File, path: &Path) -> Result<()> {
    if file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .permissions()
        .readonly()
    {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "controlled engagement evidence is not read-only".to_owned(),
        ))
    }
}

fn sync_read_only_controlled_file(
    directory: &ControlledDirectory,
    name: &OsStr,
    file: &File,
    write_authority: Option<&File>,
) -> Result<()> {
    let path = directory.path.join(name);
    #[cfg(unix)]
    {
        let _ = write_authority;
        let mut permissions = file
            .metadata()
            .map_err(|error| KernelError::io(&path, error))?
            .permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
            .map_err(|error| KernelError::io(&path, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(&path, error))?;
        require_read_only_controlled_file(file, &path)
    }
    #[cfg(windows)]
    {
        let expected_identity = controlled_file_identity(file)?;
        let already_read_only = file
            .metadata()
            .map_err(|error| KernelError::io(&path, error))?
            .permissions()
            .readonly();
        if already_read_only && write_authority.is_none() {
            let replayed = open_controlled_file(directory, name, false)?;
            require_controlled_file_identity(&replayed, &expected_identity)?;
            return require_read_only_controlled_file(&replayed, &path);
        }

        let reopened;
        let writer = if let Some(writer) = write_authority {
            writer
        } else {
            reopened = open_controlled_file(directory, name, true)?;
            &reopened
        };
        require_controlled_file_identity(writer, &expected_identity)?;
        writer
            .sync_all()
            .map_err(|error| KernelError::io(&path, error))?;
        let mut permissions = writer
            .metadata()
            .map_err(|error| KernelError::io(&path, error))?
            .permissions();
        permissions.set_readonly(true);
        writer
            .set_permissions(permissions)
            .map_err(|error| KernelError::io(&path, error))?;
        writer
            .sync_all()
            .map_err(|error| KernelError::io(&path, error))?;
        require_controlled_file_identity(writer, &expected_identity)?;
        let replayed = open_controlled_file(directory, name, false)?;
        require_controlled_file_identity(&replayed, &expected_identity)?;
        require_read_only_controlled_file(&replayed, &path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, name, file, write_authority);
        Err(KernelError::IntegrityFailed(
            "controlled engagement file durability is unsupported on this platform".to_owned(),
        ))
    }
}

fn install_retained_ready_commit_guard(
    writer: &WorkspaceWriter,
    directory: &ControlledDirectory,
    name: &OsStr,
    retained: &File,
    expected_identity: &ControlledFileIdentity,
    expected_links: u64,
) -> Result<()> {
    let guard_directory = ControlledDirectory {
        path: directory.path.clone(),
        handle: directory
            .handle
            .try_clone()
            .map_err(|error| KernelError::io(&directory.path, error))?,
    };
    let guard_name = name.to_os_string();
    let guard_retained = retained
        .try_clone()
        .map_err(|error| KernelError::io(&directory.path, error))?;
    let guard_identity = expected_identity.clone();
    require_retained_ready_commit_authority(
        &guard_directory,
        &guard_name,
        &guard_retained,
        &guard_identity,
        expected_links,
    )?;
    writer.install_supplemental_commit_guard(move || {
        require_retained_ready_commit_authority(
            &guard_directory,
            &guard_name,
            &guard_retained,
            &guard_identity,
            expected_links,
        )
    });
    Ok(())
}

fn require_retained_ready_commit_authority(
    directory: &ControlledDirectory,
    name: &OsStr,
    retained: &File,
    expected_identity: &ControlledFileIdentity,
    expected_links: u64,
) -> Result<()> {
    let path = directory.path.join(name);
    require_controlled_file_identity(retained, expected_identity)?;
    require_exact_link_count(retained, expected_links)?;
    require_read_only_controlled_file(retained, &path)?;
    let named = open_controlled_file(directory, name, false)?;
    require_controlled_file_identity(&named, expected_identity)?;
    require_exact_link_count(&named, expected_links)?;
    require_read_only_controlled_file(&named, &path)
}

#[cfg(unix)]
fn require_exact_link_count_platform(file: &File, expected: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    if metadata.nlink() == expected {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "controlled engagement evidence has an undeclared hard-link alias".to_owned(),
        ))
    }
}

#[cfg(windows)]
fn require_exact_link_count_platform(file: &File, expected: u64) -> Result<()> {
    let links = crate::durability::file_link_count(file)
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    if links == expected {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "controlled engagement evidence has an undeclared hard-link alias".to_owned(),
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn require_exact_link_count_platform(_file: &File, _expected: u64) -> Result<()> {
    Err(KernelError::IntegrityFailed(
        "controlled hard-link identity is unsupported on this platform".to_owned(),
    ))
}

fn remove_controlled_file_if_owned(
    directory: &ControlledDirectory,
    name: &OsStr,
    expected: &ControlledFileIdentity,
) -> Result<bool> {
    let named = match open_controlled_file(directory, name, false) {
        Ok(file) => file,
        Err(KernelError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if controlled_file_identity(&named)? != *expected {
        return Ok(false);
    }
    remove_controlled_file_platform(directory, name)
        .map_err(|error| KernelError::io(directory.path.join(name), error))?;
    sync_controlled_directory(directory)?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn publish_controlled_file(
    source_directory: &ControlledDirectory,
    source_name: &OsStr,
    destination_directory: &ControlledDirectory,
    destination_name: &OsStr,
    source_identity: &ControlledFileIdentity,
    retained_identity: &ControlledFileIdentity,
    expected_hash: &str,
    expected_size: u64,
    maximum: u64,
) -> Result<File> {
    let source = open_controlled_file(source_directory, source_name, false)?;
    require_controlled_file_identity(&source, source_identity)?;
    let destination = match open_controlled_file(destination_directory, destination_name, false) {
        Ok(file) => {
            require_controlled_file_identity(&file, retained_identity)?;
            if source_identity != retained_identity {
                require_unaliased_controlled_file(&source)?;
            }
            file
        }
        Err(KernelError::Io {
            source: open_error, ..
        }) if open_error.kind() == std::io::ErrorKind::NotFound => {
            if retained_identity != source_identity {
                return Err(KernelError::IntegrityFailed(
                    "retained preflight identity disappeared before deduplicated publication"
                        .to_owned(),
                ));
            }
            require_unaliased_controlled_file(&source)?;
            publish_controlled_file_platform(
                source_directory,
                source_name,
                destination_directory,
                destination_name,
            )
            .map_err(|error| {
                KernelError::io(destination_directory.path.join(destination_name), error)
            })?;
            let file = open_controlled_file(destination_directory, destination_name, false)?;
            require_controlled_file_identity(&file, retained_identity)?;
            file
        }
        Err(error) => return Err(error),
    };
    let (actual_hash, actual_size) = hash_open_regular_file(&destination, maximum)?;
    if actual_hash != expected_hash || actual_size != expected_size {
        return Err(KernelError::IntegrityFailed(
            "retained preflight conflicts with its content-addressed identity".to_owned(),
        ));
    }
    sync_controlled_directory(destination_directory)?;
    Ok(destination)
}

fn sync_controlled_directory(directory: &ControlledDirectory) -> Result<()> {
    #[cfg(unix)]
    {
        directory
            .handle
            .sync_all()
            .map_err(|error| KernelError::io(&directory.path, error))
    }
    #[cfg(windows)]
    {
        let metadata = directory
            .handle
            .metadata()
            .map_err(|error| KernelError::io(&directory.path, error))?;
        let path_matches =
            crate::durability::path_names_open_directory(&directory.path, &directory.handle)
                .map_err(|error| KernelError::io(&directory.path, error))?;
        if !metadata.file_type().is_dir()
            || crate::durability::is_reparse_point(&metadata)
            || !path_matches
        {
            return Err(KernelError::IntegrityFailed(
                "controlled engagement directory changed identity before durability replay"
                    .to_owned(),
            ));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(KernelError::IntegrityFailed(
            "controlled engagement directory durability is unsupported on this platform".to_owned(),
        ))
    }
}

#[cfg(unix)]
fn publish_controlled_file_platform(
    source_directory: &ControlledDirectory,
    source_name: &OsStr,
    destination_directory: &ControlledDirectory,
    destination_name: &OsStr,
) -> std::io::Result<()> {
    rustix::fs::linkat(
        &source_directory.handle,
        source_name,
        &destination_directory.handle,
        destination_name,
        rustix::fs::AtFlags::empty(),
    )
    .map_err(Into::into)
}

#[cfg(windows)]
fn publish_controlled_file_platform(
    source_directory: &ControlledDirectory,
    source_name: &OsStr,
    destination_directory: &ControlledDirectory,
    destination_name: &OsStr,
) -> std::io::Result<()> {
    fs::hard_link(
        source_directory.path.join(source_name),
        destination_directory.path.join(destination_name),
    )
}

#[cfg(not(any(unix, windows)))]
fn publish_controlled_file_platform(
    _source_directory: &ControlledDirectory,
    _source_name: &OsStr,
    _destination_directory: &ControlledDirectory,
    _destination_name: &OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "controlled file publication is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn remove_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
) -> std::io::Result<()> {
    rustix::fs::unlinkat(&directory.handle, name, rustix::fs::AtFlags::empty()).map_err(Into::into)
}

#[cfg(windows)]
fn remove_controlled_file_platform(
    directory: &ControlledDirectory,
    name: &OsStr,
) -> std::io::Result<()> {
    fs::remove_file(directory.path.join(name))
}

#[cfg(not(any(unix, windows)))]
fn remove_controlled_file_platform(
    _directory: &ControlledDirectory,
    _name: &OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "controlled file cleanup is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    crate::durability::is_reparse_point(metadata)
}

#[cfg(not(windows))]
const fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn ensure_workspace_directory(workspace: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace directory path is unsafe: {}",
            relative.display()
        )));
    }
    let root_metadata =
        fs::symlink_metadata(workspace).map_err(|error| KernelError::io(workspace, error))?;
    require_real_workspace_directory(workspace, &root_metadata)?;
    let canonical_root = workspace
        .canonicalize()
        .map_err(|error| KernelError::io(workspace, error))?;
    let mut current = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => require_real_workspace_directory(&current, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| KernelError::io(&current, error))?;
                let metadata = fs::symlink_metadata(&current)
                    .map_err(|error| KernelError::io(&current, error))?;
                require_real_workspace_directory(&current, &metadata)?;
            }
            Err(error) => return Err(KernelError::io(&current, error)),
        }
        require_canonical_workspace_containment(&canonical_root, &current)?;
    }
    verify_workspace_directory_chain(workspace, relative, &canonical_root)?;
    Ok(current)
}

fn verify_workspace_directory_chain(
    workspace: &Path,
    relative: &Path,
    canonical_root: &Path,
) -> Result<()> {
    let root_metadata =
        fs::symlink_metadata(workspace).map_err(|error| KernelError::io(workspace, error))?;
    require_real_workspace_directory(workspace, &root_metadata)?;
    let replayed_root = workspace
        .canonicalize()
        .map_err(|error| KernelError::io(workspace, error))?;
    if replayed_root != canonical_root {
        return Err(KernelError::IntegrityFailed(
            "workspace root changed while preparing a controlled directory".to_owned(),
        ));
    }
    let mut current = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|error| KernelError::io(&current, error))?;
        require_real_workspace_directory(&current, &metadata)?;
        require_canonical_workspace_containment(canonical_root, &current)?;
    }
    Ok(())
}

fn require_real_workspace_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let is_unsafe_link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let is_unsafe_link = is_unsafe_link || crate::durability::is_reparse_point(metadata);
    if !metadata.file_type().is_dir() || is_unsafe_link {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace directory component must be a real non-link directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_canonical_workspace_containment(canonical_root: &Path, path: &Path) -> Result<()> {
    let canonical = path
        .canonicalize()
        .map_err(|error| KernelError::io(path, error))?;
    if !canonical.starts_with(canonical_root) {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace directory escaped its canonical root: {}",
            path.display()
        )));
    }
    Ok(())
}

fn hash_open_regular_file(file: &File, maximum: u64) -> Result<(String, u64)> {
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    if !metadata.file_type().is_file() || metadata_is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "controlled engagement evidence is not a regular non-link file".to_owned(),
        ));
    }
    let mut reader = file
        .try_clone()
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| KernelError::io(Path::new("controlled engagement file"), error))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| KernelError::PolicyInvalid("source size overflow".to_owned()))?;
        if size > maximum {
            return Err(KernelError::SourceSize {
                actual: size,
                limit: maximum,
            });
        }
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:064x}", digest.finalize()), size))
}

#[allow(clippy::too_many_lines)]
fn reconcile_acceptance_jobs(
    workspace: &Path,
    attempt_id: &str,
    source: &Path,
    expected_hash: &str,
    expected_size: u64,
) -> Result<Option<String>> {
    ObjectWorkspaceService::new(workspace)?.require_active()?;
    let writer = WorkspaceWriter::open(workspace)?;
    let source_path = source.to_string_lossy();
    let now = now_rfc3339();
    let transaction = writer.connection().unchecked_transaction()?;

    let bound_job_id: Option<String> = transaction.query_row(
        "SELECT job_id FROM engagement_acceptance_attempts WHERE attempt_id = ?1",
        [attempt_id],
        |row| row.get(0),
    )?;
    let jobs = {
        let mut statement = transaction.prepare(
            "SELECT id, state, source_hash, source_size
             FROM jobs
             WHERE source_path = ?1
             ORDER BY created_at, id",
        )?;
        statement
            .query_map([source_path.as_ref()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if bound_job_id
        .as_ref()
        .is_some_and(|bound| !jobs.iter().any(|job| &job.0 == bound))
    {
        return Err(KernelError::IntegrityFailed(
            "source acceptance is bound to a job outside its controlled source identity".to_owned(),
        ));
    }

    let mut complete = Vec::new();
    for (job_id, state, source_hash, source_size) in jobs {
        let bindings = {
            let mut statement = transaction.prepare(
                "SELECT attempt_id
                 FROM engagement_acceptance_attempts
                 WHERE job_id = ?1
                 ORDER BY attempt_id",
            )?;
            statement
                .query_map([&job_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if bindings.len() > 1
            || bindings
                .first()
                .is_some_and(|binding| binding != attempt_id)
        {
            return Err(KernelError::IntegrityFailed(
                "controlled acceptance job is bound to a different acceptance intent".to_owned(),
            ));
        }
        let bound_to_attempt = !bindings.is_empty();
        match state.as_str() {
            "COMPLETE" => {
                if source_hash.as_deref() != Some(expected_hash)
                    || optional_nonnegative(source_size, "accepted job source size")?
                        != Some(expected_size)
                {
                    return Err(KernelError::IntegrityFailed(
                        "completed job at the controlled acceptance path differs from its exact preflight identity"
                            .to_owned(),
                    ));
                }
                complete.push(job_id);
            }
            "FAILED" | "INTERRUPTED" => {
                if bound_to_attempt {
                    return Err(KernelError::IntegrityFailed(
                        "source acceptance is bound to a non-complete intake job".to_owned(),
                    ));
                }
            }
            "SPOOLING" | "INSPECTING" | "INGESTING" | "RECONCILING" => {
                if bound_to_attempt {
                    return Err(KernelError::IntegrityFailed(
                        "source acceptance is bound to unfinished intake evidence".to_owned(),
                    ));
                }
                let (source_records, csv_stored): (i64, i64) = transaction.query_row(
                    "SELECT COALESCE(SUM(source_records), 0),
                            COALESCE(SUM(stored_records), 0)
                     FROM csv_files WHERE job_id = ?1",
                    [&job_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let stored_records: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
                    [&job_id],
                    |row| row.get(0),
                )?;
                if source_records < stored_records || csv_stored != stored_records {
                    return Err(KernelError::IntegrityFailed(
                        "unfinished acceptance job counters do not reconcile".to_owned(),
                    ));
                }
                let changed = transaction.execute(
                    "UPDATE jobs
                     SET state = 'INTERRUPTED',
                         error_code = 'INTERRUPTED_ACCEPTANCE_RECOVERED',
                         error_message = 'unfinished acceptance-owned intake recovered before retry',
                         source_records = ?1, stored_records = ?2, pending_records = ?3,
                         updated_at = ?4, completed_at = ?4
                     WHERE id = ?5 AND state = ?6",
                    params![
                        source_records,
                        stored_records,
                        source_records - stored_records,
                        now,
                        job_id,
                        state,
                    ],
                )?;
                require_one(changed, "interrupt unfinished acceptance intake job")?;
                transaction.execute(
                    "INSERT INTO job_events (
                         job_id, state, error_code, detail, occurred_at
                     ) VALUES (
                         ?1, 'INTERRUPTED', 'INTERRUPTED_ACCEPTANCE_RECOVERED',
                         'unfinished acceptance-owned intake recovered before retry', ?2
                     )",
                    params![job_id, now],
                )?;
            }
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "controlled acceptance job has an unknown durable state".to_owned(),
                ));
            }
        }
    }

    if complete.len() > 1 {
        return Err(KernelError::IntegrityFailed(
            "controlled acceptance source has multiple complete intake jobs".to_owned(),
        ));
    }
    if let Some(bound) = bound_job_id.as_deref()
        && complete.first().is_none_or(|job_id| job_id != bound)
    {
        return Err(KernelError::IntegrityFailed(
            "source acceptance job binding differs from complete intake evidence".to_owned(),
        ));
    }
    transaction.commit()?;
    Ok(complete.into_iter().next())
}

fn bind_completed_acceptance_job(
    workspace: &Path,
    attempt_id: &str,
    engagement_id: &str,
    job_id: &str,
    source: &Path,
    expected_hash: &str,
    expected_size: u64,
) -> Result<()> {
    ObjectWorkspaceService::new(workspace)?.require_active()?;
    let writer = WorkspaceWriter::open(workspace)?;
    let now = now_rfc3339();
    let transaction = writer.connection().unchecked_transaction()?;
    require_exact_completed_acceptance_job(
        &transaction,
        attempt_id,
        engagement_id,
        job_id,
        source,
        expected_hash,
        expected_size,
        false,
    )?;
    let changed = transaction.execute(
        "UPDATE engagement_acceptance_attempts
         SET job_id = ?1, updated_at = ?2
         WHERE attempt_id = ?3 AND engagement_id = ?4
           AND (job_id IS NULL OR job_id = ?1)",
        params![job_id, now, attempt_id, engagement_id],
    )?;
    require_one(changed, "bind verified source acceptance job")?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn require_exact_completed_acceptance_job(
    connection: &Connection,
    attempt_id: &str,
    engagement_id: &str,
    job_id: &str,
    source: &Path,
    expected_hash: &str,
    expected_size: u64,
    require_bound: bool,
) -> Result<()> {
    let attempt_job_id: Option<String> = connection.query_row(
        "SELECT job_id
         FROM engagement_acceptance_attempts
         WHERE attempt_id = ?1 AND engagement_id = ?2",
        params![attempt_id, engagement_id],
        |row| row.get(0),
    )?;
    if require_bound && attempt_job_id.as_deref() != Some(job_id) {
        return Err(KernelError::IntegrityFailed(
            "source acceptance lost its verified intake job binding".to_owned(),
        ));
    }
    if attempt_job_id
        .as_deref()
        .is_some_and(|existing| existing != job_id)
    {
        return Err(KernelError::Conflict(
            "source acceptance is already bound to a different intake job".to_owned(),
        ));
    }
    let (state, source_path, source_hash, source_size): (
        String,
        String,
        Option<String>,
        Option<i64>,
    ) = connection
        .query_row(
            "SELECT state, source_path, source_hash, source_size
             FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| KernelError::IntegrityFailed("accepted intake job is missing".to_owned()))?;
    if state != "COMPLETE"
        || source_path != source.to_string_lossy()
        || source_hash.as_deref() != Some(expected_hash)
        || optional_nonnegative(source_size, "accepted job source size")? != Some(expected_size)
    {
        return Err(KernelError::IntegrityFailed(
            "source acceptance job is not exact complete evidence for its preflight".to_owned(),
        ));
    }
    Ok(())
}

fn selected_profile_identity(state: &EngagementState) -> Result<(&str, &str, &str)> {
    Ok((
        state.active.profile_id.as_deref().ok_or_else(|| {
            KernelError::Conflict("engagement has no selected profile ID".to_owned())
        })?,
        state.active.profile_version.as_deref().ok_or_else(|| {
            KernelError::Conflict("engagement has no selected profile version".to_owned())
        })?,
        state.active.profile_hash.as_deref().ok_or_else(|| {
            KernelError::Conflict("engagement has no selected profile hash".to_owned())
        })?,
    ))
}

fn require_journey_tour_binding(
    state: &EngagementState,
    job_identity: Option<&(String, String)>,
    manifest_hash: Option<&String>,
    validation_identity: Option<&(String, Option<String>)>,
    tour: &TourSummaryView,
) -> Result<()> {
    let exact = job_identity.is_some_and(|(source_hash, job_state)| {
        job_state == "COMPLETE" && source_hash == &tour.source_hash
    }) && manifest_hash.is_some_and(|hash| hash == &tour.manifest_hash)
        && state.active.profiling_run_id.as_deref() == Some(tour.profiling_run_id.as_str())
        && state.active.profile_id.as_deref() == Some(tour.profile_id.as_str())
        && state.active.profile_version.as_deref() == Some(tour.profile_version.as_str())
        && state.active.profile_hash.as_deref() == Some(tour.profile_hash.as_str())
        && state.active.mapping_id.as_deref() == Some(tour.mapping_id.as_str())
        && state.active.mapping_version.as_deref() == Some(tour.mapping_version.as_str())
        && state.active.mapping_hash.as_deref() == Some(tour.mapping_hash.as_str())
        && state.active.validation_run_id.as_deref() == Some(tour.validation_run_id.as_str())
        && validation_identity.is_some_and(|(run_state, result_hash)| {
            run_state == "COMPLETE"
                && result_hash.as_deref() == Some(tour.validation_result_hash.as_str())
        });
    if !exact {
        return Err(KernelError::IntegrityFailed(
            "journey active context does not match immutable tour authority".to_owned(),
        ));
    }
    Ok(())
}

fn profile_fields_and_rules(
    document: &Value,
) -> Result<(Vec<ProfileFieldView>, Vec<ProfileRuleView>)> {
    let entities = document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::ProfileSemantic("profile has no entities".to_owned()))?;
    let mut fields = Vec::new();
    for entity in entities {
        let entity_id = required_string(entity, "entityId")?;
        let entity_fields = entity
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::ProfileSemantic("profile entity has no fields".to_owned())
            })?;
        for field in entity_fields {
            fields.push(ProfileFieldView {
                entity_id: entity_id.to_owned(),
                field_id: required_string(field, "fieldId")?.to_owned(),
                label: required_string(field, "label")?.to_owned(),
                logical_type: required_string(field, "logicalType")?.to_owned(),
                required: field
                    .get("required")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(
                            "profile field has no required flag".to_owned(),
                        )
                    })?,
                sensitive: field
                    .get("sensitive")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(
                            "profile field has no sensitive flag".to_owned(),
                        )
                    })?,
            });
        }
    }
    let rule_values = document
        .get("rules")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::ProfileSemantic("profile has no rules".to_owned()))?;
    let rules = rule_values
        .iter()
        .map(|rule| {
            Ok(ProfileRuleView {
                rule_id: required_string(rule, "ruleId")?.to_owned(),
                severity: required_string(rule, "severity")?.to_owned(),
                kind: required_string(rule, "kind")?.to_owned(),
                target: required_string(rule, "target")?.to_owned(),
                message: required_string(rule, "message")?.to_owned(),
                parameters: rule.get("parameters").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((fields, rules))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        KernelError::ProfileSemantic(format!("profile value has no string {field:?}"))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactFileDisposition {
    Created,
    ExistingMatching,
}

fn write_exact_file(path: &Path, bytes: &[u8]) -> Result<ExactFileDisposition> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
            if let Err(error) = write_result {
                drop(file);
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(cleanup_error) => {
                        return Err(KernelError::IntegrityFailed(format!(
                            "controlled mapping file {} could not be completed ({error}) or removed ({cleanup_error})",
                            path.display()
                        )));
                    }
                }
                return Err(KernelError::io(path, error));
            }
            Ok(ExactFileDisposition::Created)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = crate::mapping::read_regular_bounded_mapping_input(
                path,
                bytes.len(),
                "controlled mapping file",
            )
            .map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "controlled mapping file {} cannot be reused safely: {error}",
                    path.display()
                ))
            })?;
            if existing != bytes {
                return Err(KernelError::Conflict(
                    "controlled mapping file already contains different bytes".to_owned(),
                ));
            }
            Ok(ExactFileDisposition::ExistingMatching)
        }
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn validation_stage(status: &ValidationStatus) -> &'static str {
    match status.state {
        crate::validation::ValidationRunState::Running => "RUNNING",
        crate::validation::ValidationRunState::Complete => "COMPLETE",
        crate::validation::ValidationRunState::Failed => "FAILED",
        crate::validation::ValidationRunState::Interrupted => "INTERRUPTED",
        crate::validation::ValidationRunState::BoundExceeded => "BOUND_EXCEEDED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validation_evidence(issue_count: i64, issue_rows: usize) -> Connection {
        let connection = Connection::open_in_memory().expect("open validation evidence");
        connection
            .execute_batch(
                "CREATE TABLE validation_runs (
                     id TEXT PRIMARY KEY,
                     state TEXT NOT NULL,
                     issue_count INTEGER NOT NULL
                 );
                 CREATE TABLE validation_issues (
                     run_id TEXT NOT NULL
                 );
                 INSERT INTO validation_runs (id, state, issue_count)
                 VALUES ('active-run', 'COMPLETE', 0);",
            )
            .expect("create validation evidence");
        connection
            .execute(
                "UPDATE validation_runs SET issue_count = ?1 WHERE id = 'active-run'",
                [issue_count],
            )
            .expect("set declared issue count");
        for _ in 0..issue_rows {
            connection
                .execute(
                    "INSERT INTO validation_issues (run_id) VALUES ('active-run')",
                    [],
                )
                .expect("insert issue evidence");
        }
        connection
    }

    fn complete_phase(connection: &Connection) -> Result<EngagementPhase> {
        derive_phase(
            connection,
            None,
            None,
            None,
            Some("active-run"),
            "APPROVED",
            "VALIDATED",
            "COMPLETE",
        )
    }

    #[test]
    fn complete_zero_issue_evidence_has_a_non_issue_phase() {
        let connection = validation_evidence(0, 0);
        assert_eq!(
            complete_phase(&connection).expect("derive zero-issue phase"),
            EngagementPhase::ValidationComplete
        );
    }

    #[test]
    fn first_issues_phase_requires_exact_material_issue_evidence() {
        let connection = validation_evidence(1, 1);
        assert_eq!(
            complete_phase(&connection).expect("derive issue phase"),
            EngagementPhase::FirstIssuesReady
        );

        connection
            .execute(
                "UPDATE validation_runs SET issue_count = 2 WHERE id = 'active-run'",
                [],
            )
            .expect("tamper declared issue count");
        let error = complete_phase(&connection).expect_err("mismatched evidence must fail closed");
        assert!(matches!(error, KernelError::IntegrityFailed(_)));
    }

    #[test]
    fn active_source_closes_original_preflight_mutation_but_not_governed_revised_preflight() {
        let connection = Connection::open_in_memory().expect("open intake gate evidence");
        connection
            .execute_batch(
                "CREATE TABLE engagement_active_context (
                     engagement_id TEXT PRIMARY KEY,
                     active_job_id TEXT
                 );
                 CREATE TABLE engagement_tour_command_attempts (
                     engagement_id TEXT NOT NULL,
                     command_kind TEXT NOT NULL,
                     tour_attempt_id TEXT,
                     result_reference TEXT NOT NULL
                 );
                 INSERT INTO engagement_active_context (
                     engagement_id, active_job_id
                 ) VALUES ('engagement-1', 'active-job');
                 INSERT INTO engagement_tour_command_attempts (
                     engagement_id, command_kind,
                     tour_attempt_id, result_reference
                 ) VALUES (
                     'engagement-1', 'RESERVE_SOURCE',
                     'tour-attempt-2', 'revised-preflight'
                 );",
            )
            .expect("create intake gate evidence");
        assert!(matches!(
            require_source_preflight_mutation_open(
                &connection,
                "engagement-1",
                "original-preflight"
            ),
            Err(KernelError::Conflict(_))
        ));
        require_source_preflight_mutation_open(&connection, "engagement-1", "revised-preflight")
            .expect("governed revised preflight remains open");
    }

    #[cfg(windows)]
    #[test]
    fn controlled_file_identity_retains_the_full_windows_file_id() {
        let temporary = tempfile::tempdir().expect("temporary evidence directory");
        let path = temporary.path().join("controlled-evidence.bin");
        fs::write(&path, b"synthetic controlled evidence").expect("write evidence");
        let file = File::open(&path).expect("open evidence");

        let identity = controlled_file_identity(&file).expect("capture stable identity");
        assert_eq!(identity.platform, "windows-volume-file-id-v2");
        let file_id = identity.file_id.expect("full Windows file ID");
        assert_eq!(file_id.len(), 32);
        assert!(file_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[cfg(windows)]
    #[test]
    fn controlled_directory_durability_replays_the_retained_windows_identity() {
        let temporary = tempfile::tempdir().expect("temporary evidence directory");
        let retained_path = temporary.path().join("retained");
        let other_path = temporary.path().join("other");
        fs::create_dir(&retained_path).expect("create retained directory");
        fs::create_dir(&other_path).expect("create mismatched directory");
        let handle = crate::durability::open_directory_nofollow(&retained_path)
            .expect("retain directory identity");

        let retained = ControlledDirectory {
            path: retained_path,
            handle,
        };
        sync_controlled_directory(&retained).expect("replay retained directory identity");

        let mismatched = ControlledDirectory {
            path: other_path,
            handle: retained.handle.try_clone().expect("clone retained handle"),
        };
        assert!(matches!(
            sync_controlled_directory(&mismatched),
            Err(KernelError::IntegrityFailed(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn controlled_file_durability_flushes_with_windows_write_authority_before_freezing() {
        let temporary = tempfile::tempdir().expect("temporary evidence directory");
        let directory = ControlledDirectory {
            path: temporary.path().to_path_buf(),
            handle: crate::durability::open_directory_nofollow(temporary.path())
                .expect("retain evidence directory"),
        };

        let borrowed_name = OsStr::new("borrowed.bin");
        let mut borrowed_writer = create_new_controlled_file(&directory, borrowed_name)
            .expect("create borrowed-authority evidence");
        borrowed_writer
            .write_all(b"borrowed authority bytes")
            .expect("write borrowed-authority evidence");
        let borrowed_reader = open_controlled_file(&directory, borrowed_name, false)
            .expect("open borrowed-authority evidence");
        sync_read_only_controlled_file(
            &directory,
            borrowed_name,
            &borrowed_reader,
            Some(&borrowed_writer),
        )
        .expect("flush through borrowed write authority");
        require_read_only_controlled_file(&borrowed_reader, &directory.path.join(borrowed_name))
            .expect("borrowed-authority evidence is frozen");

        let reopened_name = OsStr::new("reopened.bin");
        let mut reopened_writer = create_new_controlled_file(&directory, reopened_name)
            .expect("create reopened-authority evidence");
        reopened_writer
            .write_all(b"reopened authority bytes")
            .expect("write reopened-authority evidence");
        drop(reopened_writer);
        let reopened_reader = open_controlled_file(&directory, reopened_name, false)
            .expect("open reopened-authority evidence");
        sync_read_only_controlled_file(&directory, reopened_name, &reopened_reader, None)
            .expect("flush through reopened write authority");
        require_read_only_controlled_file(&reopened_reader, &directory.path.join(reopened_name))
            .expect("reopened-authority evidence is frozen");

        drop(borrowed_reader);
        drop(borrowed_writer);
        drop(reopened_reader);
        for name in [borrowed_name, reopened_name] {
            let path = directory.path.join(name);
            let mut permissions = fs::metadata(&path)
                .expect("inspect frozen evidence")
                .permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&path, permissions).expect("thaw test evidence for cleanup");
        }
    }
}

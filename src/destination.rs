//! Governed, workspace-local destination contract workshop.
//!
//! Technical catalogues remain `GENERATED_DRAFT` evidence. This module adds
//! practitioner-authored, append-only profile drafting and binds a frozen
//! `REVIEWED_DRAFT` to the exact technical evidence before the existing
//! registry can grant narrow local mapping/validation authority.

#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::type_complexity
)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{
        canonical_hash, canonical_json_string, decode_cursor, encode_cursor, parse_unique_json,
        sha256_hex,
    },
    edmx::{EdmxLimits, EdmxSourceContext, TechnicalCatalogueArtifact},
    metadata::import_edmx_to_workspace,
    object_workspace::{ObjectWorkspaceRecord, ObjectWorkspaceService},
    profile::validate_profile_document,
    profile_registry::{
        ProfileAuthority, ProfileLifecycleStatus, ProfileVersionView, RegistryLimits,
        approve_profile_transaction, inspect_profile, register_profile_document_transaction,
    },
    state::{DATABASE_FILENAME, WorkspaceWriter, now_rfc3339, open_read_only},
};

pub const DESTINATION_WORKSHOP_SCHEMA: &str = "migration-stream/engagement/destination-workshop/v1";
pub const DESTINATION_CATALOGUE_ENTITIES_SCHEMA: &str =
    "migration-stream/engagement/destination-catalogue-entities/v1";
pub const DESTINATION_CATALOGUE_PROPERTIES_SCHEMA: &str =
    "migration-stream/engagement/destination-catalogue-properties/v1";
pub const DESTINATION_ARTIFACT_RESERVATION_SCHEMA: &str =
    "migration-stream/engagement/destination-artifact-reservation/v1";
pub const DESTINATION_ARTIFACT_SCHEMA: &str = "migration-stream/engagement/destination-artifact/v1";
pub const DESTINATION_DRAFT_SCHEMA: &str = "migration-stream/engagement/destination-draft/v1";
pub const DESTINATION_REVIEWED_PROFILE_SCHEMA: &str =
    "migration-stream/engagement/destination-reviewed-profile/v1";
pub const DESTINATION_PROFILE_APPROVAL_SCHEMA: &str =
    "migration-stream/engagement/destination-profile-approval/v1";
pub const DESTINATION_REBASE_SCHEMA: &str = "migration-stream/engagement/destination-rebase/v1";
pub const DESTINATION_CARRY_FORWARD_SCHEMA: &str =
    "migration-stream/engagement/destination-mapping-carry-forward/v1";

pub const RESERVE_EDMX_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/reserve-destination-edmx/v1";
pub const CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/create-manual-destination/v1";
pub const START_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/start-destination-draft/v1";
pub const SAVE_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/save-destination-draft/v1";
pub const REBASE_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/rebase-destination-draft/v1";
pub const FREEZE_DRAFT_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/freeze-destination-draft/v1";
pub const APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/approve-destination-profile/v1";
pub const CARRY_FORWARD_MAPPING_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/carry-forward-mapping/v1";

pub const LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-mapping-validation-authority/v1";
pub const DESTINATION_FREEZE_ACKNOWLEDGEMENT: &str =
    "migration-stream/destination-profile-reviewed-and-frozen/v1";

const ARTIFACT_EVENT_SCHEMA: &str = "migration-stream/destination-artifact-event/v1";
const ORIGIN_BINDING_SCHEMA: &str = "migration-stream/destination-profile-origin/v1";
const SCOPED_COMPLETENESS_SCHEMA: &str = "migration-stream/destination-scoped-completeness/v1";
const REPORT_ALGORITHM: &str = "migration-stream/edmx-compatibility/v1";
const REBASE_ALGORITHM: &str = "migration-stream/destination-rebase/v1";
const CARRY_FORWARD_ALGORITHM: &str = "migration-stream/mapping-carry-forward/v1";
const ENTITY_CURSOR: &str = "migration-stream/destination-entities/v1";
const PROPERTY_CURSOR: &str = "migration-stream/destination-properties/v1";
const MAX_EDMX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_ENTITIES: usize = 128;
const MAX_FIELDS: usize = 4096;
const MAX_RELATIONSHIPS: usize = 512;
const MAX_RULES: usize = 4096;
const MAX_PAGE_SIZE: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DestinationOrigin {
    ControlledPack,
    WorkspaceProfile,
}

impl DestinationOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ControlledPack => "CONTROLLED_PACK",
            Self::WorkspaceProfile => "WORKSPACE_PROFILE",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DestinationFieldState {
    Available,
    Active,
    NeedsReview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DestinationDiffState {
    Unchanged,
    Added,
    Removed,
    TechnicallyChanged,
    ManualPotentialMatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveEdmxCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub expected_workspace_id: String,
    pub expected_workspace_descriptor_hash: String,
    pub source_name: String,
    pub artifact_label: String,
    pub system: Option<String>,
    pub client_or_tenant: Option<String>,
    pub release: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationArtifactReservation {
    pub schema_version: &'static str,
    pub attempt_id: String,
    pub state: String,
    pub artifact_label: String,
    pub source_name: String,
    pub maximum_bytes: u64,
    pub created_at: String,
    #[serde(skip_serializing)]
    pub controlled_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualPropertyDefinition {
    pub name: String,
    pub edm_type: String,
    pub nullable: bool,
    pub max_length: Option<u64>,
    pub key_candidate: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualEntityDefinition {
    pub namespace: String,
    pub name: String,
    pub properties: Vec<ManualPropertyDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateManualCatalogueCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub artifact_label: String,
    pub actor: String,
    pub reason: String,
    pub entities: Vec<ManualEntityDefinition>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartDestinationDraftCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub catalogue_authority_id: String,
    pub expected_catalogue_hash: String,
    pub root_technical_entity: String,
    pub child_technical_entities: Vec<String>,
    pub profile_id: String,
    pub title: String,
    pub owner: String,
    pub actor: String,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationRuleDraft {
    pub rule_id: String,
    pub severity: String,
    pub kind: String,
    pub target: String,
    pub message: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualSupplementEvidence {
    pub supplement_id: String,
    pub actor: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TechnicalChangeDecision {
    AcceptRevisedTechnicalEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TechnicalChangeReviewEvidence {
    pub decision: TechnicalChangeDecision,
    pub actor: String,
    pub reason: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationFieldDraft {
    pub field_id: String,
    pub label: String,
    pub logical_type: String,
    pub business_required: bool,
    pub sensitive: bool,
    pub key: bool,
    pub state: DestinationFieldState,
    pub technical_property: Option<String>,
    pub manual_supplement: Option<ManualSupplementEvidence>,
    pub technical_change_review: Option<TechnicalChangeReviewEvidence>,
    pub confirm_max_length: bool,
    pub max_length: Option<u64>,
    pub diff_state: Option<DestinationDiffState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationEntityDraft {
    pub entity_id: String,
    pub label: String,
    pub root: bool,
    pub technical_entity: Option<String>,
    pub fields: Vec<DestinationFieldDraft>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationRelationshipKeyPair {
    pub from_field_id: String,
    pub to_field_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationRelationshipDraft {
    pub relationship_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    pub cardinality: String,
    pub technical_navigation: Option<String>,
    pub key_pairs: Vec<DestinationRelationshipKeyPair>,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatibilityAcknowledgement {
    pub finding_hash: String,
    pub actor: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationDraftSnapshot {
    pub profile_id: String,
    pub title: String,
    pub owner: String,
    pub entities: Vec<DestinationEntityDraft>,
    pub relationships: Vec<DestinationRelationshipDraft>,
    pub rules: Vec<DestinationRuleDraft>,
    pub compatibility_acknowledgements: Vec<CompatibilityAcknowledgement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveDestinationDraftCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub draft_id: String,
    pub expected_draft_generation: u64,
    pub expected_draft_hash: String,
    pub snapshot: DestinationDraftSnapshot,
    pub actor: String,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RebaseDestinationDraftCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub draft_id: String,
    pub expected_draft_generation: u64,
    pub expected_draft_hash: String,
    pub target_catalogue_authority_id: String,
    pub expected_target_catalogue_hash: String,
    pub actor: String,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeDestinationDraftCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub draft_id: String,
    pub expected_draft_generation: u64,
    pub expected_draft_hash: String,
    pub profile_version: String,
    pub acknowledgement: String,
    pub actor: String,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveDestinationProfileCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub origin_binding_hash: String,
    pub actor: String,
    pub reason: String,
    pub authority_acknowledgement: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CarryForwardMappingCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub source_mapping_id: String,
    pub source_mapping_version: String,
    pub source_mapping_hash: String,
    pub target_profile_id: String,
    pub target_profile_version: String,
    pub target_profile_hash: String,
    pub actor: String,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityFindingView {
    pub code: String,
    pub classification: String,
    pub technical_location: String,
    pub count: u64,
    pub blocking: bool,
    pub finding_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityReportView {
    pub report_hash: String,
    #[serde(default)]
    pub scoped_completeness_hash: Option<String>,
    pub algorithm: String,
    pub odata_generation: Option<String>,
    pub safely_extracted_count: u64,
    #[serde(default)]
    pub unsupported_inventory_count: u64,
    pub unsupported_irrelevant_count: u64,
    pub unresolved_selected_scope_count: u64,
    pub findings: Vec<CompatibilityFindingView>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationArtifactView {
    pub schema_version: &'static str,
    pub catalogue_authority_id: String,
    pub origin: String,
    pub artifact_label: String,
    pub system: Option<String>,
    pub client_or_tenant: Option<String>,
    pub release: Option<String>,
    pub manual_actor: Option<String>,
    pub manual_reason: Option<String>,
    pub snapshot_id: Option<String>,
    pub snapshot_sha256: Option<String>,
    pub snapshot_bytes: Option<u64>,
    pub catalogue_hash: String,
    pub catalogue_payload_sha256: String,
    pub catalogue_file_sha256: Option<String>,
    pub parser_name: Option<String>,
    pub parser_version: Option<String>,
    pub odata_generation: Option<String>,
    pub namespace_count: u64,
    pub namespaces: Vec<String>,
    pub entity_count: u64,
    pub property_count: u64,
    pub navigation_count: u64,
    pub compatibility: CompatibilityReportView,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationDraftView {
    pub schema_version: &'static str,
    pub draft_id: String,
    pub catalogue_authority_id: String,
    pub catalogue_hash: String,
    pub generation: u64,
    pub draft_hash: String,
    pub state: String,
    pub snapshot: DestinationDraftSnapshot,
    pub freeze_blockers: Vec<String>,
    pub frozen_profile_id: Option<String>,
    pub frozen_profile_version: Option<String>,
    pub frozen_profile_hash: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveDestinationView {
    pub origin: DestinationOrigin,
    pub destination_authority_id: String,
    pub destination_origin_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub profile_authority: ProfileAuthority,
    pub approval_id: Option<String>,
    pub approval_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationWorkshopView {
    pub schema_version: &'static str,
    pub workspace_id: String,
    pub workspace_descriptor_hash: String,
    pub engagement_id: String,
    pub engagement_generation: u64,
    pub destination_origin: Option<DestinationOrigin>,
    pub active_destination: Option<ActiveDestinationView>,
    pub reviewed_candidate: Option<FrozenDestinationProfileView>,
    pub eligible_carry_forward_source: Option<EligibleCarryForwardSourceView>,
    pub latest_artifact: Option<DestinationArtifactView>,
    pub draft: Option<DestinationDraftView>,
    pub allowed_origins: Vec<String>,
    pub limits: DestinationWorkshopLimits,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EligibleCarryForwardSourceView {
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub mapping_approval_id: String,
    pub mapping_approval_hash: String,
    pub active_job_id: String,
    pub active_source_hash: String,
    pub validated_job_id: String,
    pub validated_source_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationWorkshopLimits {
    pub maximum_edmx_bytes: u64,
    pub maximum_entities: usize,
    pub maximum_fields: usize,
    pub maximum_relationships: usize,
    pub maximum_rules: usize,
    pub maximum_page_size: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCatalogueEntityView {
    pub technical_entity: String,
    pub namespace: String,
    pub name: String,
    pub key_properties: Vec<String>,
    pub property_count: usize,
    pub navigation_count: usize,
    pub selected: bool,
    pub root: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCatalogueEntitiesPage {
    pub schema_version: &'static str,
    pub catalogue_authority_id: String,
    pub catalogue_hash: String,
    pub draft_id: Option<String>,
    pub draft_generation: Option<u64>,
    pub draft_hash: Option<String>,
    pub items: Vec<DestinationCatalogueEntityView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCataloguePropertyView {
    pub technical_property: String,
    pub technical_entity: String,
    pub name: String,
    pub edm_type: String,
    pub nullable: Option<bool>,
    pub max_length: Option<String>,
    pub precision: Option<String>,
    pub scale: Option<String>,
    pub key_candidate: bool,
    pub required_candidate: bool,
    pub state: DestinationFieldState,
    pub field: Option<DestinationFieldDraft>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCataloguePropertiesPage {
    pub schema_version: &'static str,
    pub catalogue_authority_id: String,
    pub catalogue_hash: String,
    pub technical_entity: String,
    pub draft_id: Option<String>,
    pub draft_generation: Option<u64>,
    pub draft_hash: Option<String>,
    pub items: Vec<DestinationCataloguePropertyView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenDestinationProfileView {
    pub schema_version: &'static str,
    pub engagement_generation: u64,
    pub profile: ProfileVersionView,
    pub origin_binding_id: String,
    pub origin_binding_hash: String,
    pub draft_id: String,
    pub draft_generation: u64,
    pub draft_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedDestinationProfileView {
    pub schema_version: &'static str,
    pub engagement_generation: u64,
    pub profile: ProfileVersionView,
    pub destination_origin: DestinationOrigin,
    pub destination_authority_id: String,
    pub destination_origin_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationRebaseView {
    pub schema_version: &'static str,
    pub engagement_generation: u64,
    pub rebase_id: String,
    pub diff_hash: String,
    pub counts: BTreeMap<String, u64>,
    pub draft: DestinationDraftView,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCarryForwardView {
    pub schema_version: &'static str,
    pub engagement_generation: u64,
    pub carry_forward_id: String,
    pub result_hash: String,
    pub cloned_binding_count: usize,
    pub unresolved_target_count: usize,
    pub unresolved_relationship_count: usize,
    pub unresolved_relationships: Vec<UnresolvedCarryForwardRelationshipView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedCarryForwardRelationshipView {
    pub relationship_id: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct DestinationMappingContext {
    pub origin: DestinationOrigin,
    pub authority_id: String,
    pub authority_version: String,
    pub authority_hash: String,
    pub root_entity_id: String,
}

#[derive(Clone, Debug)]
pub struct DestinationService {
    workspace: PathBuf,
    profile_schema: PathBuf,
}

#[derive(Debug)]
struct CatalogueAuthorityRow {
    id: String,
    origin: String,
    snapshot_id: Option<String>,
    snapshot_sha256: Option<String>,
    catalogue_path: Option<String>,
    catalogue_file_sha256: Option<String>,
    catalogue_payload_sha256: String,
    canonical_catalogue_json: String,
    catalogue_hash: String,
    compatibility_report_json: String,
    compatibility_report_hash: String,
    report_algorithm: String,
    artifact_label: String,
    odata_generation: Option<String>,
    created_at: String,
}

#[derive(Debug)]
struct DraftHeadRow {
    draft_id: String,
    catalogue_authority_id: String,
    catalogue_hash: String,
    generation: u64,
    state: String,
    head_revision_hash: String,
    frozen_profile_id: Option<String>,
    frozen_profile_version: Option<String>,
    frozen_profile_hash: Option<String>,
    updated_at: String,
}

impl DestinationService {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>, profile_schema: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            profile_schema: profile_schema.into(),
        }
    }

    pub(crate) fn recover_artifact_attempts(&self) -> Result<()> {
        let connection = self.open_read()?;
        let attempts = {
            let mut statement = connection.prepare(
                "SELECT attempt_id, idempotency_key, state, catalogue_authority_id
                 FROM destination_artifact_attempts
                 ORDER BY created_at, attempt_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        drop(connection);
        for (attempt_id, idempotency_key, state, catalogue_authority_id) in attempts {
            match state.as_str() {
                "RECEIVING" => self.fail_edmx_upload(
                    &attempt_id,
                    &idempotency_key,
                    "INTERRUPTED_RECOVERED",
                    true,
                )?,
                "READY" => {
                    let authority_id = catalogue_authority_id.ok_or_else(|| {
                        KernelError::IntegrityFailed(format!(
                            "READY destination artifact {attempt_id} has no catalogue authority"
                        ))
                    })?;
                    self.artifact_view(&authority_id)?;
                }
                "REJECTED" | "INTERRUPTED" => {}
                other => {
                    return Err(KernelError::IntegrityFailed(format!(
                        "destination artifact {attempt_id} has unsupported recovery state {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn reserve_edmx(
        &self,
        command: &ReserveEdmxCommand,
    ) -> Result<DestinationArtifactReservation> {
        require_schema(&command.schema_version, RESERVE_EDMX_COMMAND_SCHEMA)?;
        validate_idempotency(&command.idempotency_key)?;
        validate_name(&command.source_name, "EDMX source name", 255)?;
        if !command.source_name.to_ascii_lowercase().ends_with(".edmx") {
            return Err(KernelError::PolicyInvalid(
                "destination artifact must use the .edmx extension".to_owned(),
            ));
        }
        validate_name(&command.artifact_label, "artifact label", 256)?;
        validate_optional_text(command.system.as_deref(), "system provenance")?;
        validate_optional_text(
            command.client_or_tenant.as_deref(),
            "client or tenant provenance",
        )?;
        validate_optional_text(command.release.as_deref(), "release provenance")?;
        let workspace = self.require_workspace_identity(
            &command.expected_workspace_id,
            &command.expected_workspace_descriptor_hash,
        )?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination artifact reservation used a stale engagement generation".to_owned(),
            ));
        }
        if let Some(row) = writer
            .connection()
            .query_row(
                "SELECT attempt_id, request_hash, state, artifact_label,
                        source_name, partial_relative_path, created_at
                 FROM destination_artifact_attempts
                 WHERE engagement_id = ?1 AND idempotency_key = ?2",
                params![engagement_id, command.idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?
        {
            if row.1 != request_hash || row.2 != "RECEIVING" {
                return Err(KernelError::Conflict(
                    "destination artifact reservation retry differs from durable state".to_owned(),
                ));
            }
            let controlled_path = checked_workspace_path(&self.workspace, &row.5)?;
            return Ok(DestinationArtifactReservation {
                schema_version: DESTINATION_ARTIFACT_RESERVATION_SCHEMA,
                attempt_id: row.0,
                state: row.2,
                artifact_label: row.3,
                source_name: row.4,
                maximum_bytes: MAX_EDMX_BYTES,
                created_at: row.6,
                controlled_path,
            });
        }
        let attempt_id = Uuid::new_v4().to_string();
        let relative_path = format!("destination/uploads/{attempt_id}.edmx.part");
        let controlled_path = checked_workspace_path(&self.workspace, &relative_path)?;
        ensure_real_directory(controlled_path.parent().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "destination upload path has no controlled parent".to_owned(),
            )
        })?)?;
        let created_at = now_rfc3339();
        let event_hash = artifact_event_hash(&attempt_id, "RECEIVING", None, &created_at)?;
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO destination_artifact_attempts (
                 attempt_id, engagement_id, workspace_id, workspace_descriptor_hash,
                 idempotency_key, request_hash, canonical_request_json, state,
                 partial_relative_path, artifact_label, source_name,
                 system_context, client_context, release_context,
                 created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'RECEIVING',
                       ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            params![
                attempt_id,
                engagement_id,
                workspace.workspace_id,
                workspace.descriptor_hash,
                command.idempotency_key,
                request_hash,
                canonical,
                relative_path,
                command.artifact_label,
                command.source_name,
                command.system,
                command.client_or_tenant,
                command.release,
                created_at,
            ],
        )?;
        transaction.execute(
            "INSERT INTO destination_artifact_events (
                 attempt_id, state, event_hash, previous_event_hash, occurred_at
             ) VALUES (?1, 'RECEIVING', ?2, NULL, ?3)",
            params![attempt_id, event_hash, created_at],
        )?;
        transaction.commit()?;
        Ok(DestinationArtifactReservation {
            schema_version: DESTINATION_ARTIFACT_RESERVATION_SCHEMA,
            attempt_id,
            state: "RECEIVING".to_owned(),
            artifact_label: command.artifact_label.clone(),
            source_name: command.source_name.clone(),
            maximum_bytes: MAX_EDMX_BYTES,
            created_at,
            controlled_path,
        })
    }

    pub fn open_edmx_upload(&self, attempt_id: &str, idempotency_key: &str) -> Result<File> {
        validate_identifier(attempt_id, "destination artifact attempt")?;
        validate_idempotency(idempotency_key)?;
        let connection = self.open_read()?;
        let (stored_key, state, relative_path): (String, String, String) = connection.query_row(
            "SELECT idempotency_key, state, partial_relative_path
                 FROM destination_artifact_attempts WHERE attempt_id = ?1",
            [attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if stored_key != idempotency_key || state != "RECEIVING" {
            return Err(KernelError::Conflict(
                "destination upload does not match its exact receiving reservation".to_owned(),
            ));
        }
        let path = checked_workspace_path(&self.workspace, &relative_path)?;
        let parent = path.parent().ok_or_else(|| {
            KernelError::IntegrityFailed("destination upload path has no parent".to_owned())
        })?;
        require_real_directory(parent)?;
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&path)
            .map_err(|error| KernelError::io(&path, error))?;
        Ok(file)
    }

    pub fn complete_edmx_upload(
        &self,
        attempt_id: &str,
        idempotency_key: &str,
        mut file: File,
    ) -> Result<DestinationArtifactView> {
        file.sync_all()
            .map_err(|error| KernelError::io(self.workspace.join("destination/uploads"), error))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| KernelError::io(self.workspace.join("destination/uploads"), error))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_EDMX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| KernelError::io(self.workspace.join("destination/uploads"), error))?;
        if bytes.is_empty() || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_EDMX_BYTES {
            return Err(KernelError::EdmxLimit(format!(
                "destination EDMX bytes must be in 1..={MAX_EDMX_BYTES}"
            )));
        }
        let source_hash = sha256_hex(&bytes);
        let source_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let connection = self.open_read()?;
        let (
            engagement_id,
            stored_key,
            state,
            relative_path,
            artifact_label,
            _source_name,
            system,
            client,
            release,
        ): (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = connection.query_row(
            "SELECT engagement_id, idempotency_key, state, partial_relative_path,
                    artifact_label, source_name, system_context, client_context,
                    release_context
             FROM destination_artifact_attempts WHERE attempt_id = ?1",
            [attempt_id],
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
        )?;
        drop(connection);
        if stored_key != idempotency_key || state != "RECEIVING" {
            return Err(KernelError::Conflict(
                "destination upload completion differs from its reservation".to_owned(),
            ));
        }
        let controlled_path = checked_workspace_path(&self.workspace, &relative_path)?;
        let opened = fs::symlink_metadata(&controlled_path)
            .map_err(|error| KernelError::io(&controlled_path, error))?;
        if !opened.file_type().is_file() || opened.file_type().is_symlink() {
            return Err(KernelError::SourceNotRegular(controlled_path));
        }
        let technical = crate::edmx::import_edmx_bytes(
            &bytes,
            EdmxSourceContext {
                artifact: artifact_label.clone(),
                system: system.clone(),
                client: client.clone(),
                release: release.clone(),
            },
            &EdmxLimits::default(),
        )?;
        let compatibility = compatibility_report_from_edmx(&bytes, &technical)?;
        let imported = import_edmx_to_workspace(
            &controlled_path,
            &self.workspace,
            EdmxSourceContext {
                artifact: artifact_label.clone(),
                system,
                client,
                release,
            },
            &EdmxLimits::default(),
        )?;
        if imported.snapshot_sha256 != source_hash || imported.snapshot_bytes != source_size {
            return Err(KernelError::IntegrityFailed(
                "destination import differs from uploaded EDMX identity".to_owned(),
            ));
        }
        let artifact_bytes = fs::read(&imported.catalogue_path)
            .map_err(|error| KernelError::io(&imported.catalogue_path, error))?;
        let artifact_value = parse_unique_json(&artifact_bytes)?;
        let canonical_catalogue_json = canonical_json_string(&artifact_value)?;
        let catalogue_hash = canonical_hash(&artifact_value)?;
        let catalogue_authority_id = format!("destination-{}", &catalogue_hash[..32]);
        let report_value = serde_json::to_value(&compatibility)?;
        let compatibility_report_json = canonical_json_string(&report_value)?;
        let compatibility_report_hash = canonical_hash(&report_value)?;
        let relative_catalogue =
            relative_workspace_path(&self.workspace, &imported.catalogue_path)?;
        let created_at = now_rfc3339();
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (active_engagement, _) = active_engagement_identity(writer.connection())?;
        if active_engagement != engagement_id {
            return Err(KernelError::Conflict(
                "active engagement changed during destination import".to_owned(),
            ));
        }
        let previous_event_hash: String = writer.connection.query_row(
            "SELECT event_hash FROM destination_artifact_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [attempt_id],
            |row| row.get(0),
        )?;
        let event_hash =
            artifact_event_hash(attempt_id, "READY", Some(&previous_event_hash), &created_at)?;
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO destination_catalogue_authorities (
                 catalogue_authority_id, engagement_id, origin, snapshot_id,
                 snapshot_sha256, catalogue_path, catalogue_file_sha256,
                 catalogue_payload_sha256, canonical_catalogue_json, catalogue_hash,
                 compatibility_report_json, compatibility_report_hash,
                 report_algorithm, artifact_label, odata_generation, created_at
             ) VALUES (?1, ?2, 'EDMX', ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                catalogue_authority_id,
                engagement_id,
                imported.snapshot_id,
                imported.snapshot_sha256,
                relative_catalogue,
                imported.catalogue_file_sha256,
                imported.catalogue_payload_sha256,
                canonical_catalogue_json,
                catalogue_hash,
                compatibility_report_json,
                compatibility_report_hash,
                REPORT_ALGORITHM,
                artifact_label,
                compatibility.odata_generation,
                created_at,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE destination_artifact_attempts
             SET state = 'READY', source_sha256 = ?1, source_size = ?2,
                 snapshot_id = ?3, catalogue_authority_id = ?4,
                 updated_at = ?5, completed_at = ?5
             WHERE attempt_id = ?6 AND engagement_id = ?7
               AND idempotency_key = ?8 AND state = 'RECEIVING'",
            params![
                source_hash,
                i64::try_from(source_size).unwrap_or(i64::MAX),
                imported.snapshot_id,
                catalogue_authority_id,
                created_at,
                attempt_id,
                engagement_id,
                idempotency_key,
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "destination upload completion lost its durable state transition".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO destination_artifact_events (
                 attempt_id, state, event_hash, previous_event_hash, occurred_at
             ) VALUES (?1, 'READY', ?2, ?3, ?4)",
            params![attempt_id, event_hash, previous_event_hash, created_at],
        )?;
        transaction.commit()?;
        drop(file);
        let _ = fs::remove_file(&controlled_path);
        self.artifact_view(&catalogue_authority_id)
    }

    pub fn fail_edmx_upload(
        &self,
        attempt_id: &str,
        idempotency_key: &str,
        error_code: &str,
        interrupted: bool,
    ) -> Result<()> {
        validate_identifier(attempt_id, "destination artifact attempt")?;
        validate_idempotency(idempotency_key)?;
        validate_identifier(error_code, "destination upload error")?;
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (state, relative_path, stored_key): (String, String, String) =
            writer.connection.query_row(
                "SELECT state, partial_relative_path, idempotency_key
                 FROM destination_artifact_attempts WHERE attempt_id = ?1",
                [attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        if stored_key != idempotency_key {
            return Err(KernelError::Conflict(
                "destination upload failure differs from its reservation".to_owned(),
            ));
        }
        if state != "RECEIVING" {
            return Ok(());
        }
        let terminal = if interrupted {
            "INTERRUPTED"
        } else {
            "REJECTED"
        };
        let previous_event_hash: String = writer.connection.query_row(
            "SELECT event_hash FROM destination_artifact_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [attempt_id],
            |row| row.get(0),
        )?;
        let now = now_rfc3339();
        let event_hash =
            artifact_event_hash(attempt_id, terminal, Some(&previous_event_hash), &now)?;
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE destination_artifact_attempts
             SET state = ?1, error_code = ?2, updated_at = ?3, completed_at = ?3
             WHERE attempt_id = ?4 AND state = 'RECEIVING'",
            params![terminal, error_code, now, attempt_id],
        )?;
        transaction.execute(
            "INSERT INTO destination_artifact_events (
                 attempt_id, state, event_hash, previous_event_hash, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![attempt_id, terminal, event_hash, previous_event_hash, now],
        )?;
        transaction.commit()?;
        let path = checked_workspace_path(&self.workspace, &relative_path)?;
        let _ = fs::remove_file(path);
        Ok(())
    }

    pub fn create_manual_catalogue(
        &self,
        command: &CreateManualCatalogueCommand,
    ) -> Result<DestinationArtifactView> {
        require_schema(
            &command.schema_version,
            CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA,
        )?;
        validate_idempotency(&command.idempotency_key)?;
        validate_name(&command.artifact_label, "manual destination label", 256)?;
        validate_name(&command.actor, "manual destination actor", 256)?;
        validate_name(&command.reason, "manual destination reason", MAX_TEXT_BYTES)?;
        validate_manual_entities(&command.entities)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "CREATE_MANUAL_CATALOGUE",
            &command.idempotency_key,
            &request_hash,
        )? {
            return self.artifact_view(&result.reference);
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "manual destination creation used a stale engagement generation".to_owned(),
            ));
        }
        let catalogue_value = json!({
            "schemaVersion": "migration-stream/manual-technical-catalogue/v1",
            "origin": "MANUAL",
            "artifactLabel": command.artifact_label,
            "actor": command.actor,
            "reason": command.reason,
            "entities": command.entities,
        });
        let canonical_catalogue_json = canonical_json_string(&catalogue_value)?;
        let catalogue_hash = canonical_hash(&catalogue_value)?;
        let catalogue_authority_id = format!("destination-{}", &catalogue_hash[..32]);
        let report_value =
            serde_json::to_value(compatibility_report_from_manual(&catalogue_value)?)?;
        let report_json = canonical_json_string(&report_value)?;
        let report_hash = canonical_hash(&report_value)?;
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO destination_catalogue_authorities (
                 catalogue_authority_id, engagement_id, origin,
                 catalogue_payload_sha256, canonical_catalogue_json, catalogue_hash,
                 compatibility_report_json, compatibility_report_hash,
                 report_algorithm, artifact_label, created_at
             ) VALUES (?1, ?2, 'MANUAL', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                catalogue_authority_id,
                engagement_id,
                catalogue_hash,
                canonical_catalogue_json,
                catalogue_hash,
                report_json,
                report_hash,
                REPORT_ALGORITHM,
                command.artifact_label,
                now,
            ],
        )?;
        let result_generation = increment_generation(
            &transaction,
            &engagement_id,
            command.expected_engagement_generation,
            &now,
            "manual destination creation",
        )?;
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "CREATE_MANUAL_CATALOGUE",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &catalogue_authority_id,
            &catalogue_hash,
            &now,
        )?;
        transaction.commit()?;
        self.artifact_view(&catalogue_authority_id)
    }

    pub fn start_draft(
        &self,
        command: &StartDestinationDraftCommand,
    ) -> Result<DestinationDraftView> {
        require_schema(&command.schema_version, START_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency(&command.idempotency_key)?;
        validate_identifier(
            &command.catalogue_authority_id,
            "destination catalogue authority",
        )?;
        validate_sha256(
            &command.expected_catalogue_hash,
            "destination catalogue hash",
        )?;
        validate_technical_identity(&command.root_technical_entity, "root technical entity")?;
        if command.child_technical_entities.len() > MAX_ENTITIES.saturating_sub(1) {
            return Err(KernelError::PolicyInvalid(
                "destination draft exceeds the selected child-entity bound".to_owned(),
            ));
        }
        for child in &command.child_technical_entities {
            validate_technical_identity(child, "child technical entity")?;
        }
        validate_profile_id(&command.profile_id)?;
        validate_name(&command.title, "destination profile title", 512)?;
        validate_name(&command.owner, "destination profile owner", 256)?;
        validate_name(&command.actor, "destination draft actor", 256)?;
        validate_name(&command.reason, "destination draft reason", MAX_TEXT_BYTES)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "START_DRAFT",
            &command.idempotency_key,
            &request_hash,
        )? {
            return self.draft_revision_view(&result.reference, 1, &result.hash);
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination draft start used a stale engagement generation".to_owned(),
            ));
        }
        let authority =
            load_catalogue_authority(writer.connection(), &command.catalogue_authority_id)?;
        if authority.catalogue_hash != command.expected_catalogue_hash {
            return Err(KernelError::Conflict(
                "destination draft start differs from the exact catalogue hash".to_owned(),
            ));
        }
        let catalogue = catalogue_model(&authority)?;
        let mut selected = Vec::with_capacity(1 + command.child_technical_entities.len());
        selected.push(command.root_technical_entity.clone());
        selected.extend(command.child_technical_entities.iter().cloned());
        let selected_unique = selected.iter().collect::<HashSet<_>>();
        if selected_unique.len() != selected.len() {
            return Err(KernelError::PolicyInvalid(
                "destination entity scope contains duplicates".to_owned(),
            ));
        }
        let entities =
            initial_draft_entities(&catalogue, &selected, &command.root_technical_entity)?;
        let snapshot = DestinationDraftSnapshot {
            profile_id: command.profile_id.clone(),
            title: command.title.clone(),
            owner: command.owner.clone(),
            entities,
            relationships: Vec::new(),
            rules: Vec::new(),
            compatibility_acknowledgements: Vec::new(),
        };
        validate_draft_snapshot(&snapshot)?;
        let snapshot_value = serde_json::to_value(&snapshot)?;
        let snapshot_json = canonical_json_string(&snapshot_value)?;
        let snapshot_hash = canonical_hash(&snapshot_value)?;
        let draft_id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO destination_profile_draft_heads (
                 draft_id, engagement_id, catalogue_authority_id, catalogue_hash,
                 generation, state, head_revision_hash, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, 'EDITABLE', ?5, ?6, ?6)",
            params![
                draft_id,
                engagement_id,
                authority.id,
                authority.catalogue_hash,
                snapshot_hash,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO destination_profile_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, reason, created_at
             ) VALUES (?1, 1, ?2, NULL, ?3, ?4, ?5, ?6)",
            params![
                draft_id,
                snapshot_hash,
                snapshot_json,
                command.actor,
                command.reason,
                now
            ],
        )?;
        let result_generation = increment_generation(
            &transaction,
            &engagement_id,
            command.expected_engagement_generation,
            &now,
            "destination draft start",
        )?;
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "START_DRAFT",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &draft_id,
            &snapshot_hash,
            &now,
        )?;
        transaction.commit()?;
        self.draft_revision_view(&draft_id, 1, &snapshot_hash)
    }

    pub fn save_draft(
        &self,
        command: &SaveDestinationDraftCommand,
    ) -> Result<DestinationDraftView> {
        require_schema(&command.schema_version, SAVE_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency(&command.idempotency_key)?;
        validate_identifier(&command.draft_id, "destination draft")?;
        validate_sha256(&command.expected_draft_hash, "destination draft hash")?;
        validate_name(&command.actor, "destination draft actor", 256)?;
        validate_name(&command.reason, "destination draft reason", MAX_TEXT_BYTES)?;
        validate_draft_snapshot(&command.snapshot)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "SAVE_DRAFT",
            &command.idempotency_key,
            &request_hash,
        )? {
            return self.draft_revision_view(
                &result.reference,
                command.expected_draft_generation.saturating_add(1),
                &result.hash,
            );
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination draft save used a stale engagement generation".to_owned(),
            ));
        }
        let head = load_draft_head(writer.connection(), &command.draft_id)?;
        require_editable_draft_head(
            &head,
            command.expected_draft_generation,
            &command.expected_draft_hash,
        )?;
        let authority =
            load_catalogue_authority(writer.connection(), &head.catalogue_authority_id)?;
        let prior_properties =
            rebase_source_technical_properties(writer.connection(), &head.draft_id)?;
        validate_technical_change_reviews(writer.connection(), &head.draft_id, &command.snapshot)?;
        validate_draft_against_catalogue(&command.snapshot, &authority, &prior_properties)?;
        let value = serde_json::to_value(&command.snapshot)?;
        let revision_json = canonical_json_string(&value)?;
        if revision_json.len() > MAX_COMMAND_BYTES {
            return Err(KernelError::PolicyInvalid(
                "destination draft exceeds its canonical byte bound".to_owned(),
            ));
        }
        let revision_hash = canonical_hash(&value)?;
        let next_generation = head.generation.saturating_add(1);
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO destination_profile_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                command.draft_id,
                i64::try_from(next_generation).unwrap_or(i64::MAX),
                revision_hash,
                command.expected_draft_hash,
                revision_json,
                command.actor,
                command.reason,
                now,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE destination_profile_draft_heads
             SET generation = ?1, head_revision_hash = ?2, updated_at = ?3
             WHERE draft_id = ?4 AND state = 'EDITABLE'
               AND generation = ?5 AND head_revision_hash = ?6",
            params![
                i64::try_from(next_generation).unwrap_or(i64::MAX),
                revision_hash,
                now,
                command.draft_id,
                i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                command.expected_draft_hash,
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "destination draft save lost its compare-and-swap".to_owned(),
            ));
        }
        let result_generation = increment_generation(
            &transaction,
            &engagement_id,
            command.expected_engagement_generation,
            &now,
            "destination draft save",
        )?;
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "SAVE_DRAFT",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &command.draft_id,
            &revision_hash,
            &now,
        )?;
        transaction.commit()?;
        self.draft_revision_view(&command.draft_id, next_generation, &revision_hash)
    }

    pub fn freeze_draft(
        &self,
        command: &FreezeDestinationDraftCommand,
    ) -> Result<FrozenDestinationProfileView> {
        require_schema(&command.schema_version, FREEZE_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency(&command.idempotency_key)?;
        validate_identifier(&command.draft_id, "destination draft")?;
        validate_version(&command.profile_version)?;
        validate_sha256(&command.expected_draft_hash, "destination draft hash")?;
        validate_name(&command.actor, "destination reviewer", 256)?;
        validate_name(&command.reason, "destination review reason", MAX_TEXT_BYTES)?;
        if command.acknowledgement != DESTINATION_FREEZE_ACKNOWLEDGEMENT {
            return Err(KernelError::PolicyInvalid(format!(
                "destination freeze requires acknowledgement {DESTINATION_FREEZE_ACKNOWLEDGEMENT:?}"
            )));
        }
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "FREEZE_DRAFT",
            &command.idempotency_key,
            &request_hash,
        )? {
            let (profile_id, version, _profile_hash, origin_binding_id, origin_binding_hash): (
                String,
                String,
                String,
                String,
                String,
            ) = writer.connection.query_row(
                "SELECT profile_id, profile_version, profile_hash,
                        origin_binding_id, origin_binding_hash
                 FROM destination_profile_origin_bindings
                 WHERE draft_id = ?1 AND draft_generation = ?2",
                params![
                    result.reference,
                    i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
            return Ok(FrozenDestinationProfileView {
                schema_version: DESTINATION_REVIEWED_PROFILE_SCHEMA,
                engagement_generation: result.generation,
                profile: inspect_profile(&self.workspace, &profile_id, &version)?,
                origin_binding_id,
                origin_binding_hash,
                draft_id: result.reference,
                draft_generation: command.expected_draft_generation,
                draft_hash: profile_hash_of_draft(
                    writer.connection(),
                    &command.draft_id,
                    command.expected_draft_generation,
                )?,
            });
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination freeze used a stale engagement generation".to_owned(),
            ));
        }
        let head = load_draft_head(writer.connection(), &command.draft_id)?;
        require_editable_draft_head(
            &head,
            command.expected_draft_generation,
            &command.expected_draft_hash,
        )?;
        let snapshot = load_draft_snapshot(
            writer.connection(),
            &head.draft_id,
            head.generation,
            &head.head_revision_hash,
        )?;
        let authority =
            load_catalogue_authority(writer.connection(), &head.catalogue_authority_id)?;
        let prior_properties =
            rebase_source_technical_properties(writer.connection(), &head.draft_id)?;
        let blockers = freeze_blockers(
            writer.connection(),
            &head.draft_id,
            &snapshot,
            &authority,
            &prior_properties,
        )?;
        if !blockers.is_empty() {
            return Err(KernelError::ProfileSemantic(format!(
                "destination draft cannot freeze: {}",
                blockers.join("; ")
            )));
        }
        let profile_document =
            build_profile_document(&snapshot, &authority, &command.profile_version)?;
        validate_profile_document(&self.profile_schema, &profile_document)?;
        let origin_binding_id = Uuid::new_v4().to_string();
        let activate_new_destination: bool = writer.connection.query_row(
            "SELECT selected_profile_id IS NULL
             FROM engagement_active_context
             WHERE engagement_id = ?1 AND generation = ?2",
            params![
                engagement_id,
                i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )?;
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        // Registration, immutable origin binding, frozen draft head, and
        // active selection are one workspace-writer transaction. A returned
        // failure cannot leave an origin-less profile version behind.
        let profile = register_profile_document_transaction(
            &transaction,
            &self.profile_schema,
            &profile_document,
            &RegistryLimits::default(),
        )?;
        let origin_value =
            build_origin_binding_value(&engagement_id, &profile, &head, &authority, &snapshot)?;
        let origin_json = canonical_json_string(&origin_value)?;
        let origin_binding_hash = canonical_hash(&origin_value)?;
        let changed = transaction.execute(
            "UPDATE destination_profile_draft_heads
             SET state = 'FROZEN', frozen_profile_id = ?1,
                 frozen_profile_version = ?2, frozen_profile_hash = ?3,
                 updated_at = ?4
             WHERE draft_id = ?5 AND state = 'EDITABLE'
               AND generation = ?6 AND head_revision_hash = ?7",
            params![
                profile.profile_id,
                profile.profile_version,
                profile.profile_hash,
                now,
                command.draft_id,
                i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                command.expected_draft_hash,
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "destination freeze lost its draft-head compare-and-swap".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO destination_profile_origin_bindings (
                 origin_binding_id, engagement_id, profile_id, profile_version,
                 profile_hash, draft_id, draft_generation, draft_revision_hash,
                 catalogue_authority_id, catalogue_hash,
                 compatibility_report_hash, canonical_origin_json,
                 origin_binding_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14)",
            params![
                origin_binding_id,
                engagement_id,
                profile.profile_id,
                profile.profile_version,
                profile.profile_hash,
                command.draft_id,
                i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
                command.expected_draft_hash,
                authority.id,
                authority.catalogue_hash,
                authority.compatibility_report_hash,
                origin_json,
                origin_binding_hash,
                now,
            ],
        )?;
        let changed = if activate_new_destination {
            transaction.execute(
                "UPDATE engagement_active_context
                 SET selected_object_pack_id = ?1,
                     selected_profile_id = ?1, selected_profile_version = ?2,
                     selected_profile_hash = ?3, profile_stage = 'REVIEWED',
                     destination_origin = 'WORKSPACE_PROFILE',
                     destination_authority_id = ?4,
                     destination_origin_hash = ?5,
                     active_mapping_id = NULL, active_mapping_version = NULL,
                     active_mapping_hash = NULL, mapping_stage = 'NOT_PREPARED',
                     active_validation_run_id = NULL, validation_stage = 'NOT_LAUNCHED',
                     generation = generation + 1, updated_at = ?6
                 WHERE engagement_id = ?7 AND generation = ?8
                   AND selected_profile_id IS NULL",
                params![
                    profile.profile_id,
                    profile.profile_version,
                    profile.profile_hash,
                    origin_binding_id,
                    origin_binding_hash,
                    now,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?
        } else {
            transaction.execute(
                "UPDATE engagement_active_context
                 SET generation = generation + 1, updated_at = ?1
                 WHERE engagement_id = ?2 AND generation = ?3
                   AND selected_profile_id IS NOT NULL",
                params![
                    now,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?
        };
        if changed != 1 {
            return Err(KernelError::Conflict(
                "destination freeze lost its active-authority compare-and-swap".to_owned(),
            ));
        }
        let result_generation = command.expected_engagement_generation.saturating_add(1);
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "FREEZE_DRAFT",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &command.draft_id,
            &profile.profile_hash,
            &now,
        )?;
        transaction.commit()?;
        Ok(FrozenDestinationProfileView {
            schema_version: DESTINATION_REVIEWED_PROFILE_SCHEMA,
            engagement_generation: result_generation,
            profile,
            origin_binding_id,
            origin_binding_hash,
            draft_id: command.draft_id.clone(),
            draft_generation: command.expected_draft_generation,
            draft_hash: command.expected_draft_hash.clone(),
        })
    }

    pub fn approve_destination_profile(
        &self,
        command: &ApproveDestinationProfileCommand,
    ) -> Result<ApprovedDestinationProfileView> {
        require_schema(
            &command.schema_version,
            APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA,
        )?;
        validate_idempotency(&command.idempotency_key)?;
        validate_profile_id(&command.profile_id)?;
        validate_version(&command.profile_version)?;
        validate_sha256(&command.profile_hash, "destination profile hash")?;
        validate_sha256(
            &command.origin_binding_hash,
            "destination origin binding hash",
        )?;
        validate_name(&command.actor, "destination approver", 256)?;
        validate_name(
            &command.reason,
            "destination approval reason",
            MAX_TEXT_BYTES,
        )?;
        if command.authority_acknowledgement != LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT {
            return Err(KernelError::PolicyInvalid(format!(
                "destination approval requires acknowledgement {LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT:?}"
            )));
        }
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        let binding: (String, String) = writer.connection.query_row(
            "SELECT origin_binding_id, origin_binding_hash
             FROM destination_profile_origin_bindings
             WHERE engagement_id = ?1 AND profile_id = ?2
               AND profile_version = ?3 AND profile_hash = ?4",
            params![
                engagement_id,
                command.profile_id,
                command.profile_version,
                command.profile_hash
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if binding.1 != command.origin_binding_hash {
            return Err(KernelError::Conflict(
                "destination approval differs from the exact profile origin binding".to_owned(),
            ));
        }
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "APPROVE_PROFILE",
            &command.idempotency_key,
            &request_hash,
        )? {
            if result.reference != binding.0 {
                return Err(KernelError::IntegrityFailed(
                    "destination approval replay points to different origin evidence".to_owned(),
                ));
            }
            let profile = inspect_profile(
                &self.workspace,
                &command.profile_id,
                &command.profile_version,
            )?;
            if profile
                .approval
                .as_ref()
                .is_none_or(|approval| approval.approval_hash != result.hash)
            {
                return Err(KernelError::IntegrityFailed(
                    "destination approval replay differs from exact registry evidence".to_owned(),
                ));
            }
            return Ok(ApprovedDestinationProfileView {
                schema_version: DESTINATION_PROFILE_APPROVAL_SCHEMA,
                engagement_generation: result.generation,
                profile,
                destination_origin: DestinationOrigin::WorkspaceProfile,
                destination_authority_id: binding.0,
                destination_origin_hash: binding.1,
            });
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination approval used a stale engagement generation".to_owned(),
            ));
        }
        let (active_candidate, may_activate_without_mapping): (bool, bool) =
            writer.connection.query_row(
                "SELECT selected_profile_id = ?1
                    AND selected_profile_version = ?2
                    AND selected_profile_hash = ?3
                    AND destination_origin = 'WORKSPACE_PROFILE'
                    AND destination_authority_id = ?4
                    AND destination_origin_hash = ?5,
                    mapping_stage = 'NOT_PREPARED'
                    AND active_mapping_id IS NULL
                    AND active_mapping_version IS NULL
                    AND active_mapping_hash IS NULL
                    AND validation_stage = 'NOT_LAUNCHED'
                    AND active_validation_run_id IS NULL
             FROM engagement_active_context
             WHERE engagement_id = ?6 AND generation = ?7",
                params![
                    command.profile_id,
                    command.profile_version,
                    command.profile_hash,
                    binding.0,
                    binding.1,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        // Approval evidence and the engagement generation advance together.
        // A revised candidate may become active directly only while there is
        // no mapping or validation authority to preserve. Otherwise the exact
        // old profile/mapping remains active until explicit carry-forward.
        let approved = approve_profile_transaction(
            &transaction,
            &command.profile_id,
            &command.profile_version,
            &command.profile_hash,
            &command.actor,
            &command.reason,
            true,
            &RegistryLimits::default(),
        )?;
        let changed = if active_candidate {
            transaction.execute(
                "UPDATE engagement_active_context
                 SET profile_stage = 'APPROVED', generation = generation + 1,
                     updated_at = ?1
                 WHERE engagement_id = ?2 AND generation = ?3
                   AND selected_profile_id = ?4
                   AND selected_profile_version = ?5
                   AND selected_profile_hash = ?6
                   AND destination_origin = 'WORKSPACE_PROFILE'
                   AND destination_authority_id = ?7
                   AND destination_origin_hash = ?8",
                params![
                    now,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                    command.profile_id,
                    command.profile_version,
                    command.profile_hash,
                    binding.0,
                    binding.1,
                ],
            )?
        } else if may_activate_without_mapping {
            transaction.execute(
                "UPDATE engagement_active_context
                 SET selected_object_pack_id = ?1,
                     selected_profile_id = ?1,
                     selected_profile_version = ?2,
                     selected_profile_hash = ?3,
                     profile_stage = 'APPROVED',
                     destination_origin = 'WORKSPACE_PROFILE',
                     destination_authority_id = ?4,
                     destination_origin_hash = ?5,
                     generation = generation + 1, updated_at = ?6
                 WHERE engagement_id = ?7 AND generation = ?8
                   AND mapping_stage = 'NOT_PREPARED'
                   AND active_mapping_id IS NULL
                   AND active_mapping_version IS NULL
                   AND active_mapping_hash IS NULL
                   AND validation_stage = 'NOT_LAUNCHED'
                   AND active_validation_run_id IS NULL",
                params![
                    command.profile_id,
                    command.profile_version,
                    command.profile_hash,
                    binding.0,
                    binding.1,
                    now,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?
        } else {
            transaction.execute(
                "UPDATE engagement_active_context
                 SET generation = generation + 1, updated_at = ?1
                 WHERE engagement_id = ?2 AND generation = ?3",
                params![
                    now,
                    engagement_id,
                    i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
                ],
            )?
        };
        if changed != 1 {
            return Err(KernelError::Conflict(
                "destination approval lost its exact active-authority compare-and-swap".to_owned(),
            ));
        }
        let result_generation = command.expected_engagement_generation.saturating_add(1);
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "APPROVE_PROFILE",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &binding.0,
            approved
                .approval
                .as_ref()
                .map_or(command.profile_hash.as_str(), |approval| {
                    approval.approval_hash.as_str()
                }),
            &now,
        )?;
        transaction.commit()?;
        Ok(ApprovedDestinationProfileView {
            schema_version: DESTINATION_PROFILE_APPROVAL_SCHEMA,
            engagement_generation: result_generation,
            profile: approved,
            destination_origin: DestinationOrigin::WorkspaceProfile,
            destination_authority_id: binding.0,
            destination_origin_hash: binding.1,
        })
    }

    pub fn rebase_draft(
        &self,
        command: &RebaseDestinationDraftCommand,
    ) -> Result<DestinationRebaseView> {
        require_schema(&command.schema_version, REBASE_DRAFT_COMMAND_SCHEMA)?;
        validate_idempotency(&command.idempotency_key)?;
        validate_identifier(&command.draft_id, "destination draft")?;
        validate_identifier(
            &command.target_catalogue_authority_id,
            "target destination catalogue",
        )?;
        validate_sha256(&command.expected_draft_hash, "destination draft hash")?;
        validate_sha256(
            &command.expected_target_catalogue_hash,
            "target destination catalogue hash",
        )?;
        validate_name(&command.actor, "destination rebase actor", 256)?;
        validate_name(&command.reason, "destination rebase reason", MAX_TEXT_BYTES)?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "REBASE_DRAFT",
            &command.idempotency_key,
            &request_hash,
        )? {
            let (rebase_id, diff_json, diff_hash, result_revision_hash): (
                String,
                String,
                String,
                String,
            ) = writer.connection.query_row(
                "SELECT rebase_id, canonical_diff_json, diff_hash,
                            result_revision_hash
                     FROM destination_rebases WHERE result_draft_id = ?1",
                [&result.reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            if diff_hash != result.hash {
                return Err(KernelError::IntegrityFailed(
                    "destination rebase retry differs from immutable diff evidence".to_owned(),
                ));
            }
            let diff_value = parse_unique_json(diff_json.as_bytes())?;
            return Ok(DestinationRebaseView {
                schema_version: DESTINATION_REBASE_SCHEMA,
                engagement_generation: result.generation,
                rebase_id,
                diff_hash,
                counts: diff_counts(&diff_value)?,
                draft: self.draft_revision_view(&result.reference, 1, &result_revision_hash)?,
            });
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "destination rebase used a stale engagement generation".to_owned(),
            ));
        }
        let head = load_draft_head(writer.connection(), &command.draft_id)?;
        require_rebase_source_head(
            &head,
            command.expected_draft_generation,
            &command.expected_draft_hash,
        )?;
        let source_snapshot = load_draft_snapshot(
            writer.connection(),
            &head.draft_id,
            head.generation,
            &head.head_revision_hash,
        )?;
        let source_authority =
            load_catalogue_authority(writer.connection(), &head.catalogue_authority_id)?;
        let target_authority =
            load_catalogue_authority(writer.connection(), &command.target_catalogue_authority_id)?;
        if target_authority.catalogue_hash != command.expected_target_catalogue_hash {
            return Err(KernelError::Conflict(
                "destination rebase target catalogue hash changed".to_owned(),
            ));
        }
        if source_authority.id == target_authority.id {
            return Err(KernelError::Conflict(
                "destination rebase requires a different immutable catalogue".to_owned(),
            ));
        }
        let source_model = catalogue_model(&source_authority)?;
        let target_model = catalogue_model(&target_authority)?;
        let (rebased, diff_value) =
            rebase_snapshot(&source_snapshot, &source_model, &target_model)?;
        validate_draft_snapshot(&rebased)?;
        let draft_value = serde_json::to_value(&rebased)?;
        let draft_json = canonical_json_string(&draft_value)?;
        let draft_hash = canonical_hash(&draft_value)?;
        let diff_json = canonical_json_string(&diff_value)?;
        let diff_hash = canonical_hash(&diff_value)?;
        let rebase_id = Uuid::new_v4().to_string();
        let result_draft_id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO destination_profile_draft_heads (
                 draft_id, engagement_id, catalogue_authority_id, catalogue_hash,
                 generation, state, head_revision_hash, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, 'EDITABLE', ?5, ?6, ?6)",
            params![
                result_draft_id,
                engagement_id,
                target_authority.id,
                target_authority.catalogue_hash,
                draft_hash,
                now,
            ],
        )?;
        transaction.execute(
            "INSERT INTO destination_profile_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, reason, created_at
             ) VALUES (?1, 1, ?2, NULL, ?3, ?4, ?5, ?6)",
            params![
                result_draft_id,
                draft_hash,
                draft_json,
                command.actor,
                command.reason,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO destination_rebases (
                 rebase_id, engagement_id, from_draft_id, from_revision_hash,
                 to_catalogue_authority_id, to_catalogue_hash,
                 result_draft_id, result_revision_hash, algorithm,
                 canonical_diff_json, diff_hash, actor, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14)",
            params![
                rebase_id,
                engagement_id,
                command.draft_id,
                command.expected_draft_hash,
                target_authority.id,
                target_authority.catalogue_hash,
                result_draft_id,
                draft_hash,
                REBASE_ALGORITHM,
                diff_json,
                diff_hash,
                command.actor,
                command.reason,
                now,
            ],
        )?;
        let result_generation = increment_generation(
            &transaction,
            &engagement_id,
            command.expected_engagement_generation,
            &now,
            "destination rebase",
        )?;
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "REBASE_DRAFT",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &result_draft_id,
            &diff_hash,
            &now,
        )?;
        transaction.commit()?;
        Ok(DestinationRebaseView {
            schema_version: DESTINATION_REBASE_SCHEMA,
            engagement_generation: result_generation,
            rebase_id,
            diff_hash,
            counts: diff_counts(&diff_value)?,
            draft: self.draft_revision_view(&result_draft_id, 1, &draft_hash)?,
        })
    }

    pub fn carry_forward_mapping(
        &self,
        command: &CarryForwardMappingCommand,
    ) -> Result<DestinationCarryForwardView> {
        use crate::engagement::MappingDraftSnapshot;
        use crate::mapping::MappingDocument;

        require_schema(
            &command.schema_version,
            CARRY_FORWARD_MAPPING_COMMAND_SCHEMA,
        )?;
        validate_idempotency(&command.idempotency_key)?;
        validate_identifier(&command.source_mapping_id, "source mapping ID")?;
        validate_version(&command.source_mapping_version)?;
        validate_sha256(&command.source_mapping_hash, "source mapping hash")?;
        validate_profile_id(&command.target_profile_id)?;
        validate_version(&command.target_profile_version)?;
        validate_sha256(&command.target_profile_hash, "target profile hash")?;
        validate_name(&command.actor, "mapping carry-forward actor", 256)?;
        validate_name(
            &command.reason,
            "mapping carry-forward reason",
            MAX_TEXT_BYTES,
        )?;
        let canonical = canonical_command(command)?;
        let request_hash = sha256_hex(canonical.as_bytes());
        let writer = WorkspaceWriter::open(&self.workspace)?;
        let (engagement_id, generation) = active_engagement_identity(writer.connection())?;
        if let Some(result) = load_workshop_attempt(
            writer.connection(),
            &engagement_id,
            "CARRY_FORWARD_MAPPING",
            &command.idempotency_key,
            &request_hash,
        )? {
            let row: (String, i64, i64, String) = writer.connection.query_row(
                "SELECT result_hash,
                        json_array_length(json_extract(canonical_result_json, '$.clonedBindings')),
                        json_array_length(json_extract(canonical_result_json, '$.unresolvedTargets')),
                        json_extract(canonical_result_json, '$.unresolvedRelationships')
                 FROM destination_mapping_carry_forwards
                 WHERE carry_forward_id = ?1",
                [&result.reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            if row.0 != result.hash {
                return Err(KernelError::IntegrityFailed(
                    "mapping carry-forward retry differs from immutable result evidence".to_owned(),
                ));
            }
            let unresolved_relationships: Vec<UnresolvedCarryForwardRelationshipView> =
                serde_json::from_str(&row.3)?;
            return Ok(DestinationCarryForwardView {
                schema_version: DESTINATION_CARRY_FORWARD_SCHEMA,
                engagement_generation: result.generation,
                carry_forward_id: result.reference,
                result_hash: row.0,
                cloned_binding_count: usize::try_from(row.1).unwrap_or(usize::MAX),
                unresolved_target_count: usize::try_from(row.2).unwrap_or(usize::MAX),
                unresolved_relationship_count: unresolved_relationships.len(),
                unresolved_relationships,
            });
        }
        if generation != command.expected_engagement_generation {
            return Err(KernelError::Conflict(
                "mapping carry-forward used a stale engagement generation".to_owned(),
            ));
        }
        let eligible = eligible_carry_forward_source(writer.connection(), &engagement_id)?
            .ok_or_else(|| {
                KernelError::Conflict(
                    "mapping carry-forward requires one exact active approved mapping authority"
                        .to_owned(),
                )
            })?;
        if eligible.mapping_id != command.source_mapping_id
            || eligible.mapping_version != command.source_mapping_version
            || eligible.mapping_hash != command.source_mapping_hash
        {
            return Err(KernelError::Conflict(
                "mapping carry-forward source differs from the independently verified eligible mapping"
                .to_owned(),
            ));
        }
        let candidate =
            load_reviewed_candidate(writer.connection(), &self.workspace, &engagement_id)?
                .ok_or_else(|| {
                    KernelError::Conflict(
                        "mapping carry-forward requires one exact reviewed destination candidate"
                            .to_owned(),
                    )
                })?;
        if candidate.profile.profile_id != command.target_profile_id
            || candidate.profile.profile_version != command.target_profile_version
            || candidate.profile.profile_hash != command.target_profile_hash
        {
            return Err(KernelError::Conflict(
                "mapping carry-forward target differs from the current reviewed destination candidate"
                    .to_owned(),
            ));
        }
        if eligible.profile_id == command.target_profile_id
            && eligible.profile_version == command.target_profile_version
            && eligible.profile_hash == command.target_profile_hash
        {
            return Err(KernelError::Conflict(
                "mapping carry-forward requires a new destination profile authority".to_owned(),
            ));
        }
        let (source_document_json, source_profile_id, source_profile_version, source_profile_hash): (
            String,
            String,
            String,
            String,
        ) = writer.connection.query_row(
            "SELECT document_json, profile_id, profile_version, profile_hash
             FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2 AND mapping_hash = ?3",
            params![
                command.source_mapping_id,
                command.source_mapping_version,
                command.source_mapping_hash
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let source_mapping: MappingDocument = serde_json::from_str(&source_document_json)?;
        if source_profile_id != eligible.profile_id
            || source_profile_version != eligible.profile_version
            || source_profile_hash != eligible.profile_hash
        {
            return Err(KernelError::IntegrityFailed(
                "eligible mapping source profile differs from its registered mapping".to_owned(),
            ));
        }
        let source_profile = crate::profile_registry::load_verified_profile_document(
            writer.connection(),
            &source_profile_id,
            &source_profile_version,
            &source_profile_hash,
        )?;
        let target_profile = crate::profile_registry::load_verified_profile_document(
            writer.connection(),
            &command.target_profile_id,
            &command.target_profile_version,
            &command.target_profile_hash,
        )?;
        let target_view = inspect_profile(
            &self.workspace,
            &command.target_profile_id,
            &command.target_profile_version,
        )?;
        if target_view.authority != ProfileAuthority::LocalMappingValidation {
            return Err(KernelError::ProfileNotApproved(
                "mapping carry-forward target profile is not separately approved".to_owned(),
            ));
        }
        let origin: (String, String) = writer.connection.query_row(
            "SELECT origin_binding_id, origin_binding_hash
             FROM destination_profile_origin_bindings
             WHERE engagement_id = ?1 AND profile_id = ?2
               AND profile_version = ?3 AND profile_hash = ?4",
            params![
                engagement_id,
                command.target_profile_id,
                command.target_profile_version,
                command.target_profile_hash
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if candidate.origin_binding_id != origin.0 || candidate.origin_binding_hash != origin.1 {
            return Err(KernelError::IntegrityFailed(
                "reviewed destination candidate differs from its exact origin binding".to_owned(),
            ));
        }
        verify_origin_binding(
            writer.connection(),
            &origin.0,
            &origin.1,
            &command.target_profile_id,
            &command.target_profile_version,
            &command.target_profile_hash,
        )?;
        let carried = compute_carry_forward(&source_mapping, &source_profile, &target_profile)?;
        let snapshot = MappingDraftSnapshot {
            source_entity_assignments: carried.source_entity_assignments.clone(),
            bindings: carried.cloned_bindings.clone(),
            relationship_bindings: carried.cloned_relationships.clone(),
            // Workspace profiles do not carry controlled-pack lookup authority.
            // Lookup-dependent targets remain unresolved for explicit review.
            reference_tables: Vec::new(),
            author: Some(command.actor.clone()),
        };
        let snapshot_value = serde_json::to_value(&snapshot)?;
        let snapshot_json = canonical_json_string(&snapshot_value)?;
        let snapshot_hash = canonical_hash(&snapshot_value)?;
        let result_value = json!({
            "schemaVersion": DESTINATION_CARRY_FORWARD_SCHEMA,
            "algorithm": CARRY_FORWARD_ALGORITHM,
            "sourceMapping": {
                "mappingId": command.source_mapping_id,
                "mappingVersion": command.source_mapping_version,
                "mappingHash": command.source_mapping_hash,
                "mappingApprovalId": eligible.mapping_approval_id.clone(),
                "mappingApprovalHash": eligible.mapping_approval_hash.clone(),
                "activeJobId": eligible.active_job_id.clone(),
                "activeSourceHash": eligible.active_source_hash.clone(),
                "validatedJobId": eligible.validated_job_id.clone(),
                "validatedSourceHash": eligible.validated_source_hash.clone(),
                "profileId": source_profile_id,
                "profileVersion": source_profile_version,
                "profileHash": source_profile_hash,
            },
            "targetProfile": {
                "profileId": command.target_profile_id,
                "profileVersion": command.target_profile_version,
                "profileHash": command.target_profile_hash,
                "originBindingId": candidate.origin_binding_id,
                "originBindingHash": candidate.origin_binding_hash,
                "draftId": candidate.draft_id,
                "draftGeneration": candidate.draft_generation,
                "draftHash": candidate.draft_hash,
            },
            "clonedBindings": carried.cloned_bindings,
            "unresolvedTargets": carried.unresolved_targets,
            "unresolvedRelationships": carried.unresolved_relationships,
            "draftHash": snapshot_hash,
        });
        let result_json = canonical_json_string(&result_value)?;
        let result_hash = canonical_hash(&result_value)?;
        let carry_forward_id = Uuid::new_v4().to_string();
        let result_draft_id = Uuid::new_v4().to_string();
        let job_id = eligible.active_job_id.clone();
        let source_hash = eligible.active_source_hash.clone();
        let now = now_rfc3339();
        let transaction = writer.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO engagement_mapping_draft_heads (
                 draft_id, engagement_id, source_job_id, source_hash,
                 object_pack_id, object_pack_version,
                 profile_id, profile_version, profile_hash,
                 generation, state, head_revision_hash, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7,
                       1, 'EDITABLE', ?8, ?9, ?9)",
            params![
                result_draft_id,
                engagement_id,
                job_id,
                source_hash,
                command.target_profile_id,
                command.target_profile_version,
                command.target_profile_hash,
                snapshot_hash,
                now,
            ],
        )?;
        transaction.execute(
            "INSERT INTO engagement_mapping_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, created_at
             ) VALUES (?1, 1, ?2, NULL, ?3, ?4, ?5)",
            params![
                result_draft_id,
                snapshot_hash,
                snapshot_json,
                command.actor,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO destination_mapping_carry_forwards (
                 carry_forward_id, engagement_id,
                 source_mapping_id, source_mapping_version, source_mapping_hash,
                 source_profile_id, source_profile_version, source_profile_hash,
                 target_profile_id, target_profile_version, target_profile_hash,
                 result_draft_id, result_draft_hash, algorithm,
                 canonical_result_json, result_hash, actor, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                carry_forward_id,
                engagement_id,
                command.source_mapping_id,
                command.source_mapping_version,
                command.source_mapping_hash,
                source_profile_id,
                source_profile_version,
                source_profile_hash,
                command.target_profile_id,
                command.target_profile_version,
                command.target_profile_hash,
                result_draft_id,
                snapshot_hash,
                CARRY_FORWARD_ALGORITHM,
                result_json,
                result_hash,
                command.actor,
                command.reason,
                now,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE engagement_active_context
             SET selected_object_pack_id = ?1,
                 selected_profile_id = ?1, selected_profile_version = ?2,
                 selected_profile_hash = ?3, profile_stage = 'APPROVED',
                 destination_origin = 'WORKSPACE_PROFILE',
                 destination_authority_id = ?4,
                 destination_origin_hash = ?5,
                 active_mapping_id = NULL, active_mapping_version = NULL,
                 active_mapping_hash = NULL, mapping_stage = 'NOT_PREPARED',
                 active_validation_run_id = NULL, validation_stage = 'NOT_LAUNCHED',
                 generation = generation + 1, updated_at = ?6
             WHERE engagement_id = ?7 AND generation = ?8",
            params![
                command.target_profile_id,
                command.target_profile_version,
                command.target_profile_hash,
                origin.0,
                origin.1,
                now,
                engagement_id,
                i64::try_from(command.expected_engagement_generation).unwrap_or(i64::MAX),
            ],
        )?;
        if changed != 1 {
            return Err(KernelError::Conflict(
                "mapping carry-forward lost its active-authority compare-and-swap".to_owned(),
            ));
        }
        let result_generation = command.expected_engagement_generation.saturating_add(1);
        insert_workshop_attempt(
            &transaction,
            &engagement_id,
            "CARRY_FORWARD_MAPPING",
            &command.idempotency_key,
            &request_hash,
            &canonical,
            command.expected_engagement_generation,
            result_generation,
            &carry_forward_id,
            &result_hash,
            &now,
        )?;
        transaction.commit()?;
        Ok(DestinationCarryForwardView {
            schema_version: DESTINATION_CARRY_FORWARD_SCHEMA,
            engagement_generation: result_generation,
            carry_forward_id,
            result_hash,
            cloned_binding_count: carried.cloned_bindings.len(),
            unresolved_target_count: carried.unresolved_targets.len(),
            unresolved_relationship_count: carried.unresolved_relationships.len(),
            unresolved_relationships: carried.unresolved_relationships,
        })
    }

    pub fn workshop(&self) -> Result<DestinationWorkshopView> {
        let workspace = self.require_active_workspace()?;
        let connection = self.open_read()?;
        let (engagement_id, generation) = active_engagement_identity(&connection)?;
        let active = load_active_destination(&connection, &self.workspace)?;
        let reviewed_candidate =
            load_reviewed_candidate(&connection, &self.workspace, &engagement_id)?;
        let eligible_carry_forward_source =
            eligible_carry_forward_source(&connection, &engagement_id)?;
        let destination_origin = active.as_ref().map(|value| value.origin);
        let latest_authority_id: Option<String> = connection
            .query_row(
                "SELECT catalogue_authority_id
                 FROM destination_catalogue_authorities
                 WHERE engagement_id = ?1
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [&engagement_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut latest_artifact = latest_authority_id
            .as_deref()
            .map(|id| self.artifact_view(id))
            .transpose()?;
        let latest_draft_id: Option<String> = connection
            .query_row(
                "SELECT draft_id FROM destination_profile_draft_heads
                 WHERE engagement_id = ?1
                 ORDER BY updated_at DESC, rowid DESC LIMIT 1",
                [&engagement_id],
                |row| row.get(0),
            )
            .optional()?;
        let draft = latest_draft_id
            .as_deref()
            .map(|id| self.draft_view(id))
            .transpose()?;
        if let (Some(artifact), Some(draft)) = (&mut latest_artifact, &draft)
            && artifact.catalogue_authority_id == draft.catalogue_authority_id
            && artifact.catalogue_hash == draft.catalogue_hash
        {
            let authority =
                load_catalogue_authority(&connection, &artifact.catalogue_authority_id)?;
            artifact.compatibility = scoped_compatibility_report(&draft.snapshot, &authority)?;
        }
        Ok(DestinationWorkshopView {
            schema_version: DESTINATION_WORKSHOP_SCHEMA,
            workspace_id: workspace.workspace_id,
            workspace_descriptor_hash: workspace.descriptor_hash,
            engagement_id,
            engagement_generation: generation,
            destination_origin,
            active_destination: active,
            reviewed_candidate,
            eligible_carry_forward_source,
            latest_artifact,
            draft,
            allowed_origins: vec!["CONTROLLED_PACK".to_owned(), "WORKSPACE_PROFILE".to_owned()],
            limits: DestinationWorkshopLimits {
                maximum_edmx_bytes: MAX_EDMX_BYTES,
                maximum_entities: MAX_ENTITIES,
                maximum_fields: MAX_FIELDS,
                maximum_relationships: MAX_RELATIONSHIPS,
                maximum_rules: MAX_RULES,
                maximum_page_size: MAX_PAGE_SIZE,
            },
        })
    }

    pub fn catalogue_entities(
        &self,
        catalogue_authority_id: &str,
        query: Option<&str>,
        page_size: usize,
        cursor: Option<&str>,
    ) -> Result<DestinationCatalogueEntitiesPage> {
        validate_identifier(catalogue_authority_id, "destination catalogue authority")?;
        let page_size = validate_page_size(page_size)?;
        let connection = self.open_read()?;
        let authority = load_catalogue_authority(&connection, catalogue_authority_id)?;
        let filter = normalized_filter(query)?;
        let filter_hash = canonical_hash(&json!({"query": filter}))?;
        let after = cursor
            .map(|value| {
                decode_cursor(
                    value,
                    ENTITY_CURSOR,
                    &authority.catalogue_hash,
                    &filter_hash,
                )
                .and_then(|payload| cursor_text(&payload.last_key))
            })
            .transpose()?;
        let draft = latest_draft_for_authority(&connection, catalogue_authority_id)?;
        let selected = draft
            .as_ref()
            .map(|(_, snapshot)| {
                snapshot
                    .entities
                    .iter()
                    .filter_map(|entity| {
                        entity
                            .technical_entity
                            .as_ref()
                            .map(|technical| (technical.clone(), entity.root))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let model = catalogue_model(&authority)?;
        let mut items = model
            .entities
            .into_iter()
            .filter(|entity| {
                filter.is_empty() || entity.technical_id.to_ascii_lowercase().contains(&filter)
            })
            .filter(|entity| {
                after
                    .as_ref()
                    .is_none_or(|after| entity.technical_id > *after)
            })
            .map(|entity| DestinationCatalogueEntityView {
                technical_entity: entity.technical_id.clone(),
                namespace: entity.namespace,
                name: entity.name,
                key_properties: entity.key,
                property_count: entity.properties.len(),
                navigation_count: entity.navigation_count,
                selected: selected.contains_key(&entity.technical_id),
                root: selected.get(&entity.technical_id).copied().unwrap_or(false),
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.technical_entity.cmp(&right.technical_entity));
        let has_more = items.len() > page_size;
        items.truncate(page_size);
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| {
                    encode_cursor(
                        ENTITY_CURSOR,
                        &authority.catalogue_hash,
                        &filter_hash,
                        json!([item.technical_entity]),
                    )
                })
                .transpose()?
        } else {
            None
        };
        let (draft_id, draft_generation, draft_hash) =
            draft.as_ref().map_or((None, None, None), |(head, _)| {
                (
                    Some(head.draft_id.clone()),
                    Some(head.generation),
                    Some(head.head_revision_hash.clone()),
                )
            });
        Ok(DestinationCatalogueEntitiesPage {
            schema_version: DESTINATION_CATALOGUE_ENTITIES_SCHEMA,
            catalogue_authority_id: authority.id,
            catalogue_hash: authority.catalogue_hash,
            draft_id,
            draft_generation,
            draft_hash,
            items,
            next_cursor,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn catalogue_properties(
        &self,
        catalogue_authority_id: &str,
        technical_entity: &str,
        query: Option<&str>,
        state_filter: Option<DestinationFieldState>,
        page_size: usize,
        cursor: Option<&str>,
    ) -> Result<DestinationCataloguePropertiesPage> {
        validate_identifier(catalogue_authority_id, "destination catalogue authority")?;
        validate_technical_identity(technical_entity, "technical entity")?;
        let page_size = validate_page_size(page_size)?;
        let connection = self.open_read()?;
        let authority = load_catalogue_authority(&connection, catalogue_authority_id)?;
        let filter = normalized_filter(query)?;
        let filter_hash = canonical_hash(&json!({
            "entity": technical_entity,
            "query": filter,
            "state": state_filter,
        }))?;
        let after = cursor
            .map(|value| {
                decode_cursor(
                    value,
                    PROPERTY_CURSOR,
                    &authority.catalogue_hash,
                    &filter_hash,
                )
                .and_then(|payload| cursor_text(&payload.last_key))
            })
            .transpose()?;
        let draft = latest_draft_for_authority(&connection, catalogue_authority_id)?;
        let drafted_fields = draft
            .as_ref()
            .map(|(_, snapshot)| {
                snapshot
                    .entities
                    .iter()
                    .flat_map(|entity| entity.fields.iter())
                    .filter_map(|field| {
                        field
                            .technical_property
                            .as_ref()
                            .map(|technical| (technical.clone(), field.clone()))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let model = catalogue_model(&authority)?;
        let entity = model
            .entities
            .into_iter()
            .find(|entity| entity.technical_id == technical_entity)
            .ok_or_else(|| KernelError::NotFound(format!("technical entity {technical_entity}")))?;
        let key = entity.key.iter().cloned().collect::<HashSet<_>>();
        let mut items = entity
            .properties
            .into_iter()
            .filter(|property| {
                filter.is_empty()
                    || property.name.to_ascii_lowercase().contains(&filter)
                    || property.edm_type.to_ascii_lowercase().contains(&filter)
            })
            .filter(|property| {
                after
                    .as_ref()
                    .is_none_or(|after| property.technical_id > *after)
            })
            .filter_map(|property| {
                let field = drafted_fields.get(&property.technical_id).cloned();
                let state = field
                    .as_ref()
                    .map_or(DestinationFieldState::Available, |field| field.state);
                if state_filter.is_some_and(|expected| expected != state) {
                    return None;
                }
                Some(DestinationCataloguePropertyView {
                    technical_property: property.technical_id,
                    technical_entity: technical_entity.to_owned(),
                    name: property.name.clone(),
                    edm_type: property.edm_type,
                    nullable: property.nullable,
                    max_length: property.max_length,
                    precision: property.precision,
                    scale: property.scale,
                    key_candidate: key.contains(&property.name),
                    required_candidate: property.nullable == Some(false),
                    state,
                    field,
                })
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.technical_property.cmp(&right.technical_property));
        let has_more = items.len() > page_size;
        items.truncate(page_size);
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| {
                    encode_cursor(
                        PROPERTY_CURSOR,
                        &authority.catalogue_hash,
                        &filter_hash,
                        json!([item.technical_property]),
                    )
                })
                .transpose()?
        } else {
            None
        };
        let (draft_id, draft_generation, draft_hash) =
            draft.as_ref().map_or((None, None, None), |(head, _)| {
                (
                    Some(head.draft_id.clone()),
                    Some(head.generation),
                    Some(head.head_revision_hash.clone()),
                )
            });
        Ok(DestinationCataloguePropertiesPage {
            schema_version: DESTINATION_CATALOGUE_PROPERTIES_SCHEMA,
            catalogue_authority_id: authority.id,
            catalogue_hash: authority.catalogue_hash,
            technical_entity: technical_entity.to_owned(),
            draft_id,
            draft_generation,
            draft_hash,
            items,
            next_cursor,
        })
    }

    /// Verify one workspace-local destination profile and its exact frozen
    /// origin without granting mapping authority.
    pub fn verified_profile_context(
        &self,
        destination_origin: &str,
        destination_authority_id: &str,
        destination_origin_hash: &str,
        profile_id: &str,
        profile_version: &str,
        profile_hash: &str,
    ) -> Result<Option<DestinationMappingContext>> {
        if destination_origin != DestinationOrigin::WorkspaceProfile.as_str() {
            return Ok(None);
        }
        let connection = self.open_read()?;
        verify_origin_binding(
            &connection,
            destination_authority_id,
            destination_origin_hash,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        let profile_view = crate::profile_registry::inspect_profile_connection(
            &connection,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        if profile_view.declared_status != "REVIEWED_DRAFT"
            || !matches!(
                profile_view.lifecycle_status,
                ProfileLifecycleStatus::ReviewedDraft | ProfileLifecycleStatus::Approved
            )
        {
            return Err(KernelError::IntegrityFailed(
                "workspace destination profile is not a current frozen REVIEWED_DRAFT authority"
                    .to_owned(),
            ));
        }
        let profile = crate::profile_registry::load_verified_profile_document(
            &connection,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        let root_entity_id = profile
            .get("entities")
            .and_then(Value::as_array)
            .and_then(|entities| {
                entities
                    .iter()
                    .find(|entity| entity.get("root").and_then(Value::as_bool) == Some(true))
            })
            .and_then(|entity| entity.get("entityId"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "workspace destination profile has no exact root entity".to_owned(),
                )
            })?
            .to_owned();
        Ok(Some(DestinationMappingContext {
            origin: DestinationOrigin::WorkspaceProfile,
            authority_id: destination_authority_id.to_owned(),
            authority_version: profile_version.to_owned(),
            authority_hash: destination_origin_hash.to_owned(),
            root_entity_id,
        }))
    }

    pub fn mapping_context(
        &self,
        destination_origin: &str,
        destination_authority_id: &str,
        destination_origin_hash: &str,
        profile_id: &str,
        profile_version: &str,
        profile_hash: &str,
    ) -> Result<Option<DestinationMappingContext>> {
        let Some(context) = self.verified_profile_context(
            destination_origin,
            destination_authority_id,
            destination_origin_hash,
            profile_id,
            profile_version,
            profile_hash,
        )?
        else {
            return Ok(None);
        };
        let connection = self.open_read()?;
        let profile_view = crate::profile_registry::inspect_profile_connection(
            &connection,
            profile_id,
            profile_version,
            profile_hash,
        )?;
        if profile_view.lifecycle_status != ProfileLifecycleStatus::Approved
            || profile_view.authority != ProfileAuthority::LocalMappingValidation
            || profile_view.approval.is_none()
        {
            return Err(KernelError::ProfileNotApproved(
                "workspace destination mapping requires one exact approved profile hash".to_owned(),
            ));
        }
        Ok(Some(context))
    }

    fn artifact_view(&self, catalogue_authority_id: &str) -> Result<DestinationArtifactView> {
        let connection = self.open_read()?;
        let row = load_catalogue_authority(&connection, catalogue_authority_id)?;
        let model = catalogue_model(&row)?;
        let provenance = load_artifact_provenance(&connection, &row)?;
        let compatibility: CompatibilityReportView =
            serde_json::from_str(&row.compatibility_report_json)?;
        let (snapshot_bytes, parser_name, parser_version) = if let Some(snapshot_id) =
            row.snapshot_id.as_deref()
        {
            let bytes: i64 = connection.query_row(
                "SELECT byte_size FROM metadata_snapshots WHERE id = ?1",
                [snapshot_id],
                |record| record.get(0),
            )?;
            let parser: (String, String) = connection.query_row(
                "SELECT parser_name, parser_version FROM parser_runs
                     WHERE snapshot_id = ?1 AND catalogue_path = ?2
                       AND catalogue_sha256 = ?3
                     ORDER BY completed_at DESC LIMIT 1",
                params![snapshot_id, row.catalogue_path, row.catalogue_file_sha256],
                |record| Ok((record.get(0)?, record.get(1)?)),
            )?;
            (
                Some(u64::try_from(bytes).map_err(|_| {
                    KernelError::IntegrityFailed("destination snapshot size is negative".to_owned())
                })?),
                Some(parser.0),
                Some(parser.1),
            )
        } else {
            (None, None, None)
        };
        Ok(DestinationArtifactView {
            schema_version: DESTINATION_ARTIFACT_SCHEMA,
            catalogue_authority_id: row.id,
            origin: row.origin,
            artifact_label: row.artifact_label,
            system: provenance.system,
            client_or_tenant: provenance.client_or_tenant,
            release: provenance.release,
            manual_actor: provenance.manual_actor,
            manual_reason: provenance.manual_reason,
            snapshot_id: row.snapshot_id,
            snapshot_sha256: row.snapshot_sha256,
            snapshot_bytes,
            catalogue_hash: row.catalogue_hash,
            catalogue_payload_sha256: row.catalogue_payload_sha256,
            catalogue_file_sha256: row.catalogue_file_sha256,
            parser_name,
            parser_version,
            odata_generation: row.odata_generation,
            namespace_count: u64::try_from(model.namespaces.len()).unwrap_or(u64::MAX),
            namespaces: model.namespaces.clone(),
            entity_count: u64::try_from(model.entities.len()).unwrap_or(u64::MAX),
            property_count: u64::try_from(
                model
                    .entities
                    .iter()
                    .map(|entity| entity.properties.len())
                    .sum::<usize>(),
            )
            .unwrap_or(u64::MAX),
            navigation_count: u64::try_from(
                model
                    .entities
                    .iter()
                    .map(|entity| entity.navigation_count)
                    .sum::<usize>(),
            )
            .unwrap_or(u64::MAX),
            compatibility,
            created_at: row.created_at,
        })
    }

    fn draft_view(&self, draft_id: &str) -> Result<DestinationDraftView> {
        let connection = self.open_read()?;
        let head = load_draft_head(&connection, draft_id)?;
        let snapshot = load_draft_snapshot(
            &connection,
            &head.draft_id,
            head.generation,
            &head.head_revision_hash,
        )?;
        let authority = load_catalogue_authority(&connection, &head.catalogue_authority_id)?;
        let prior_properties = rebase_source_technical_properties(&connection, &head.draft_id)?;
        let freeze_blockers = freeze_blockers(
            &connection,
            &head.draft_id,
            &snapshot,
            &authority,
            &prior_properties,
        )?;
        Ok(DestinationDraftView {
            schema_version: DESTINATION_DRAFT_SCHEMA,
            draft_id: head.draft_id,
            catalogue_authority_id: head.catalogue_authority_id,
            catalogue_hash: head.catalogue_hash,
            generation: head.generation,
            draft_hash: head.head_revision_hash,
            state: head.state,
            freeze_blockers,
            snapshot,
            frozen_profile_id: head.frozen_profile_id,
            frozen_profile_version: head.frozen_profile_version,
            frozen_profile_hash: head.frozen_profile_hash,
            updated_at: head.updated_at,
        })
    }

    fn draft_revision_view(
        &self,
        draft_id: &str,
        generation: u64,
        revision_hash: &str,
    ) -> Result<DestinationDraftView> {
        let connection = self.open_read()?;
        let head = load_draft_head(&connection, draft_id)?;
        if generation == 0 || generation > head.generation {
            return Err(KernelError::IntegrityFailed(
                "destination retry references a revision outside the immutable draft chain"
                    .to_owned(),
            ));
        }
        let snapshot = load_draft_snapshot(&connection, draft_id, generation, revision_hash)?;
        let updated_at: String = connection.query_row(
            "SELECT created_at FROM destination_profile_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2 AND revision_hash = ?3",
            params![
                draft_id,
                i64::try_from(generation).unwrap_or(i64::MAX),
                revision_hash
            ],
            |row| row.get(0),
        )?;
        let authority = load_catalogue_authority(&connection, &head.catalogue_authority_id)?;
        let prior_properties = rebase_source_technical_properties(&connection, draft_id)?;
        let freeze_blockers = freeze_blockers(
            &connection,
            draft_id,
            &snapshot,
            &authority,
            &prior_properties,
        )?;
        Ok(DestinationDraftView {
            schema_version: DESTINATION_DRAFT_SCHEMA,
            draft_id: draft_id.to_owned(),
            catalogue_authority_id: head.catalogue_authority_id,
            catalogue_hash: head.catalogue_hash,
            generation,
            draft_hash: revision_hash.to_owned(),
            state: "EDITABLE".to_owned(),
            freeze_blockers,
            snapshot,
            frozen_profile_id: None,
            frozen_profile_version: None,
            frozen_profile_hash: None,
            updated_at,
        })
    }

    fn require_active_workspace(&self) -> Result<ObjectWorkspaceRecord> {
        ObjectWorkspaceService::new(&self.workspace)?.require_active()
    }

    fn require_workspace_identity(
        &self,
        expected_id: &str,
        expected_hash: &str,
    ) -> Result<ObjectWorkspaceRecord> {
        let workspace = self.require_active_workspace()?;
        if workspace.workspace_id != expected_id || workspace.descriptor_hash != expected_hash {
            return Err(KernelError::Conflict(
                "destination command differs from the exact object workspace identity".to_owned(),
            ));
        }
        Ok(workspace)
    }

    fn open_read(&self) -> Result<Connection> {
        open_read_only(&self.workspace.join(DATABASE_FILENAME))
    }
}

#[derive(Clone, Debug)]
struct CatalogueModel {
    namespaces: Vec<String>,
    entities: Vec<CatalogueEntity>,
}

#[derive(Clone, Debug)]
struct CatalogueEntity {
    technical_id: String,
    namespace: String,
    name: String,
    key: Vec<String>,
    properties: Vec<CatalogueProperty>,
    navigation_count: usize,
    navigation_identities: Vec<String>,
    navigation_relationships: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct CatalogueProperty {
    technical_id: String,
    name: String,
    edm_type: String,
    nullable: Option<bool>,
    max_length: Option<String>,
    precision: Option<String>,
    scale: Option<String>,
}

fn catalogue_model(authority: &CatalogueAuthorityRow) -> Result<CatalogueModel> {
    let value = parse_unique_json(authority.canonical_catalogue_json.as_bytes())?;
    if authority.origin == "EDMX" {
        let artifact: TechnicalCatalogueArtifact = serde_json::from_value(value)?;
        artifact.verify_contract()?;
        let mut namespaces = artifact
            .catalogue
            .schemas
            .iter()
            .map(|schema| schema.namespace.clone())
            .collect::<Vec<_>>();
        namespaces.sort();
        namespaces.dedup();
        let mut entities = Vec::new();
        for schema in artifact.catalogue.schemas {
            for entity in schema.entity_types {
                let technical_id = format!("{}.{}", schema.namespace, entity.name);
                let navigation_identities = entity
                    .navigation_properties
                    .iter()
                    .map(|navigation| format!("{technical_id}.{}", navigation.name))
                    .collect::<Vec<_>>();
                let navigation_relationships = entity
                    .navigation_properties
                    .iter()
                    .filter_map(|navigation| {
                        navigation.relationship.as_ref().map(|relationship| {
                            (
                                format!("{technical_id}.{}", navigation.name),
                                relationship.clone(),
                            )
                        })
                    })
                    .collect();
                let properties = entity
                    .properties
                    .into_iter()
                    .map(|property| CatalogueProperty {
                        technical_id: format!("{technical_id}.{}", property.name),
                        name: property.name,
                        edm_type: property.edm_type,
                        nullable: property.nullable,
                        max_length: property.max_length,
                        precision: property.precision,
                        scale: property.scale,
                    })
                    .collect();
                entities.push(CatalogueEntity {
                    technical_id,
                    namespace: schema.namespace.clone(),
                    name: entity.name,
                    key: entity.key,
                    properties,
                    navigation_count: entity.navigation_properties.len(),
                    navigation_identities,
                    navigation_relationships,
                });
            }
        }
        entities.sort_by(|left, right| left.technical_id.cmp(&right.technical_id));
        return Ok(CatalogueModel {
            namespaces,
            entities,
        });
    }
    let manual: CreateManualCatalogueProjection = serde_json::from_value(value)?;
    let mut namespaces = manual
        .entities
        .iter()
        .map(|entity| entity.namespace.clone())
        .collect::<Vec<_>>();
    namespaces.sort();
    namespaces.dedup();
    let mut entities = manual
        .entities
        .into_iter()
        .map(|entity| {
            let technical_id = format!("{}.{}", entity.namespace, entity.name);
            let key = entity
                .properties
                .iter()
                .filter(|property| property.key_candidate)
                .map(|property| property.name.clone())
                .collect();
            let properties = entity
                .properties
                .into_iter()
                .map(|property| CatalogueProperty {
                    technical_id: format!("{technical_id}.{}", property.name),
                    name: property.name,
                    edm_type: property.edm_type,
                    nullable: Some(property.nullable),
                    max_length: property.max_length.map(|value| value.to_string()),
                    precision: None,
                    scale: None,
                })
                .collect();
            CatalogueEntity {
                technical_id,
                namespace: entity.namespace,
                name: entity.name,
                key,
                properties,
                navigation_count: 0,
                navigation_identities: Vec::new(),
                navigation_relationships: BTreeMap::new(),
            }
        })
        .collect::<Vec<_>>();
    entities.sort_by(|left, right| left.technical_id.cmp(&right.technical_id));
    Ok(CatalogueModel {
        namespaces,
        entities,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateManualCatalogueProjection {
    entities: Vec<ManualEntityDefinition>,
}

fn compatibility_report_from_edmx(
    bytes: &[u8],
    artifact: &TechnicalCatalogueArtifact,
) -> Result<CompatibilityReportView> {
    let report = crate::edmx::scan_edmx_compatibility(bytes, &EdmxLimits::default())?;
    if report.snapshot_sha256 != artifact.catalogue.snapshot.sha256
        || report.byte_length != artifact.catalogue.snapshot.byte_length
    {
        return Err(KernelError::IntegrityFailed(
            "EDMX compatibility report differs from the exact imported snapshot".to_owned(),
        ));
    }
    let mut findings = Vec::new();
    for qualified in &report.qualified_attributes {
        let value = json!({
            "code": "QUALIFIED_EXTENSION_ATTRIBUTE",
            "element": qualified.element,
            "namespace": qualified.namespace,
            "name": qualified.name,
            "count": qualified.occurrences,
        });
        findings.push(CompatibilityFindingView {
            code: "QUALIFIED_EXTENSION_ATTRIBUTE".to_owned(),
            classification: "NON_STRUCTURAL_WARNING".to_owned(),
            technical_location: format!(
                "{}@{}:{}",
                qualified.element, qualified.namespace, qualified.name
            ),
            count: qualified.occurrences,
            blocking: false,
            finding_hash: canonical_hash(&value)?,
        });
    }
    for unsupported in &report.unsupported_constructs {
        let code = format!("{:?}", unsupported.kind).to_ascii_uppercase();
        let value = serde_json::to_value(unsupported)?;
        findings.push(CompatibilityFindingView {
            code,
            classification: "SCOPE_NEUTRAL_INVENTORY".to_owned(),
            technical_location: unsupported.location.clone(),
            count: unsupported.occurrences,
            blocking: false,
            finding_hash: canonical_hash(&value)?,
        });
    }
    findings.sort_by(|left, right| {
        (
            left.code.as_str(),
            left.technical_location.as_str(),
            left.finding_hash.as_str(),
        )
            .cmp(&(
                right.code.as_str(),
                right.technical_location.as_str(),
                right.finding_hash.as_str(),
            ))
    });
    let safely_extracted_count = report
        .extracted
        .namespace_count
        .saturating_add(report.extracted.entity_count)
        .saturating_add(report.extracted.property_count)
        .saturating_add(report.extracted.navigation_count);
    let unsupported_inventory_count = report
        .unsupported_constructs
        .iter()
        .map(|finding| finding.occurrences)
        .sum();
    let unsupported_irrelevant_count = 0;
    let report_value = json!({
        "schemaVersion": "migration-stream/destination-compatibility/v1",
        "algorithm": REPORT_ALGORITHM,
        "odataGeneration": report.odata_generation,
        "safelyExtractedCount": safely_extracted_count,
        "unsupportedInventoryCount": unsupported_inventory_count,
        "unsupportedIrrelevantCount": unsupported_irrelevant_count,
        "unresolvedSelectedScopeCount": 0,
        "findings": findings,
    });
    Ok(CompatibilityReportView {
        report_hash: canonical_hash(&report_value)?,
        scoped_completeness_hash: None,
        algorithm: REPORT_ALGORITHM.to_owned(),
        odata_generation: Some(match report.odata_generation {
            crate::edmx::EdmxODataGeneration::V2 => "V2".to_owned(),
            crate::edmx::EdmxODataGeneration::V4 => "V4".to_owned(),
        }),
        safely_extracted_count,
        unsupported_inventory_count,
        unsupported_irrelevant_count,
        unresolved_selected_scope_count: 0,
        findings,
    })
}

fn compatibility_report_from_manual(catalogue: &Value) -> Result<CompatibilityReportView> {
    if required_json_string(catalogue, "schemaVersion")?
        != "migration-stream/manual-technical-catalogue/v1"
        || required_json_string(catalogue, "origin")? != "MANUAL"
    {
        return Err(KernelError::IntegrityFailed(
            "manual destination catalogue has an unsupported authority shape".to_owned(),
        ));
    }
    let projection: CreateManualCatalogueProjection = serde_json::from_value(catalogue.clone())?;
    validate_manual_entities(&projection.entities)?;
    let safely_extracted_count = projection
        .entities
        .iter()
        .map(|entity| 1_u64 + u64::try_from(entity.properties.len()).unwrap_or(u64::MAX))
        .sum::<u64>();
    let report_basis = json!({
        "schemaVersion": "migration-stream/destination-compatibility/v1",
        "algorithm": REPORT_ALGORITHM,
        "odataGeneration": Value::Null,
        "safelyExtractedCount": safely_extracted_count,
        "unsupportedInventoryCount": 0,
        "unsupportedIrrelevantCount": 0,
        "unresolvedSelectedScopeCount": 0,
        "findings": [],
    });
    Ok(CompatibilityReportView {
        report_hash: canonical_hash(&report_basis)?,
        scoped_completeness_hash: None,
        algorithm: REPORT_ALGORITHM.to_owned(),
        odata_generation: None,
        safely_extracted_count,
        unsupported_inventory_count: 0,
        unsupported_irrelevant_count: 0,
        unresolved_selected_scope_count: 0,
        findings: Vec::new(),
    })
}

fn scoped_compatibility_report(
    snapshot: &DestinationDraftSnapshot,
    authority: &CatalogueAuthorityRow,
) -> Result<CompatibilityReportView> {
    let mut report: CompatibilityReportView =
        serde_json::from_str(&authority.compatibility_report_json)?;
    let catalogue = catalogue_model(authority)?;
    let selected_entities = snapshot
        .entities
        .iter()
        .filter_map(|entity| entity.technical_entity.clone())
        .collect::<BTreeSet<_>>();
    let selected_properties = snapshot
        .entities
        .iter()
        .flat_map(|entity| entity.fields.iter())
        .filter(|field| field.state != DestinationFieldState::Available)
        .filter_map(|field| field.technical_property.clone())
        .collect::<BTreeSet<_>>();
    let selected_navigations = snapshot
        .relationships
        .iter()
        .filter_map(|relationship| relationship.technical_navigation.clone())
        .collect::<BTreeSet<_>>();
    let navigation_relationships = catalogue
        .entities
        .iter()
        .flat_map(|entity| entity.navigation_relationships.iter())
        .map(|(navigation, relationship)| (navigation.clone(), relationship.clone()))
        .collect::<BTreeMap<_, _>>();
    let selected_associations = selected_navigations
        .iter()
        .filter_map(|navigation| navigation_relationships.get(navigation).cloned())
        .collect::<BTreeSet<_>>();
    let relationship_source_entities = snapshot
        .relationships
        .iter()
        .filter_map(|relationship| {
            snapshot
                .entities
                .iter()
                .find(|entity| entity.entity_id == relationship.from_entity_id)
                .and_then(|entity| entity.technical_entity.clone())
        })
        .collect::<BTreeSet<_>>();
    let technical_types = catalogue
        .entities
        .iter()
        .flat_map(|entity| {
            entity
                .properties
                .iter()
                .map(|property| (property.technical_id.clone(), property.edm_type.clone()))
        })
        .collect::<HashMap<_, _>>();
    let selected_types = selected_properties
        .iter()
        .filter_map(|property| technical_types.get(property))
        .collect::<BTreeSet<_>>();
    let mut resolved_navigations = BTreeSet::new();
    let mut resolved_associations = BTreeSet::new();
    let mut relationship_resolutions = Vec::new();
    for relationship in snapshot
        .relationships
        .iter()
        .filter(|relationship| relationship.confirmed && !relationship.key_pairs.is_empty())
    {
        let Some(technical_navigation) = relationship.technical_navigation.as_ref() else {
            continue;
        };
        let Some(technical_association) = navigation_relationships.get(technical_navigation) else {
            continue;
        };
        let safe_key_pairs = relationship.key_pairs.iter().all(|pair| {
            relationship_key_field_is_safe(
                snapshot,
                &relationship.from_entity_id,
                &pair.from_field_id,
                true,
            ) && relationship_key_field_is_safe(
                snapshot,
                &relationship.to_entity_id,
                &pair.to_field_id,
                false,
            )
        });
        if !safe_key_pairs {
            continue;
        }
        resolved_navigations.insert(technical_navigation.clone());
        resolved_associations.insert(technical_association.clone());
        relationship_resolutions.push((
            relationship.relationship_id.clone(),
            json!({
                "relationshipId": relationship.relationship_id,
                "technicalNavigation": technical_navigation,
                "technicalAssociation": technical_association,
                "fromEntityId": relationship.from_entity_id,
                "toEntityId": relationship.to_entity_id,
                "cardinality": relationship.cardinality,
                "keyPairs": relationship.key_pairs,
            }),
        ));
    }
    relationship_resolutions.sort_by(|left, right| left.0.cmp(&right.0));
    let relationship_resolutions = relationship_resolutions
        .into_iter()
        .map(|(_, resolution)| resolution)
        .collect::<Vec<_>>();

    for finding in &mut report.findings {
        let touches_selected = finding_touches_selected_scope(
            finding,
            &selected_entities,
            &selected_properties,
            &selected_navigations,
            &selected_associations,
            &relationship_source_entities,
            &selected_types,
        );
        let structural_blocker = matches!(
            finding.code.as_str(),
            "INHERITANCE"
                | "CONTAINMENT"
                | "REFERENTIALCONSTRAINT"
                | "COMPLEXTYPE"
                | "ENUMTYPE"
                | "TYPEDEFINITION"
        ) && touches_selected;
        let operator_resolved = finding.code == "REFERENTIALCONSTRAINT"
            && structural_blocker
            && resolved_navigations
                .iter()
                .chain(resolved_associations.iter())
                .any(|identity| technical_location_contains(&finding.technical_location, identity));
        finding.blocking = structural_blocker && !operator_resolved;
        let classification = if finding.blocking {
            "UNRESOLVED_SELECTED_SCOPE"
        } else if operator_resolved {
            "SELECTED_SCOPE_OPERATOR_RESOLVED"
        } else if finding.code == "QUALIFIED_EXTENSION_ATTRIBUTE" {
            "NON_STRUCTURAL_WARNING"
        } else if finding.code == "ANNOTATION" && touches_selected {
            "SELECTED_SCOPE_WARNING"
        } else {
            "UNSUPPORTED_IRRELEVANT"
        };
        classification.clone_into(&mut finding.classification);
    }
    report.unresolved_selected_scope_count = report
        .findings
        .iter()
        .filter(|finding| finding.blocking)
        .map(|finding| finding.count)
        .sum();
    report.unsupported_irrelevant_count = report
        .findings
        .iter()
        .filter(|finding| finding.classification == "UNSUPPORTED_IRRELEVANT")
        .map(|finding| finding.count)
        .sum();
    let mut acknowledgements = snapshot.compatibility_acknowledgements.clone();
    acknowledgements.sort_by(|left, right| left.finding_hash.cmp(&right.finding_hash));
    let scope_value = json!({
        "schemaVersion": SCOPED_COMPLETENESS_SCHEMA,
        "catalogueAuthorityId": authority.id,
        "catalogueHash": authority.catalogue_hash,
        "inventoryReportHash": authority.compatibility_report_hash,
        "selectedEntities": selected_entities,
        "selectedProperties": selected_properties,
        "selectedNavigations": selected_navigations,
        "selectedAssociations": selected_associations,
        "relationshipResolutions": relationship_resolutions,
        "findings": report.findings,
        "acknowledgements": acknowledgements,
    });
    report.scoped_completeness_hash = Some(canonical_hash(&scope_value)?);
    Ok(report)
}

fn finding_touches_selected_scope(
    finding: &CompatibilityFindingView,
    selected_entities: &BTreeSet<String>,
    selected_properties: &BTreeSet<String>,
    selected_navigations: &BTreeSet<String>,
    selected_associations: &BTreeSet<String>,
    relationship_source_entities: &BTreeSet<String>,
    selected_types: &BTreeSet<&String>,
) -> bool {
    match finding.code.as_str() {
        "INHERITANCE" => selected_entities
            .iter()
            .any(|entity| technical_location_contains(&finding.technical_location, entity)),
        "CONTAINMENT" => selected_navigations
            .iter()
            .any(|navigation| technical_location_contains(&finding.technical_location, navigation)),
        "REFERENTIALCONSTRAINT" => selected_navigations
            .iter()
            .chain(selected_associations.iter())
            .chain(relationship_source_entities.iter())
            .any(|identity| technical_location_contains(&finding.technical_location, identity)),
        "COMPLEXTYPE" | "ENUMTYPE" | "TYPEDEFINITION" => {
            selected_types.iter().any(|technical_type| {
                technical_type_references(technical_type, &finding.technical_location)
            })
        }
        "ANNOTATION" => selected_entities
            .iter()
            .chain(selected_properties.iter())
            .chain(selected_navigations.iter())
            .any(|identity| technical_location_contains(&finding.technical_location, identity)),
        _ => false,
    }
}

fn technical_location_contains(location: &str, identity: &str) -> bool {
    location == identity
        || location
            .strip_prefix(identity)
            .is_some_and(|remainder| remainder.starts_with('.') || remainder.starts_with('/'))
}

fn technical_type_references(technical_type: &str, location: &str) -> bool {
    technical_type == location
        || technical_type
            .strip_prefix("Collection(")
            .and_then(|value| value.strip_suffix(')'))
            == Some(location)
}

fn relationship_key_field_is_safe(
    snapshot: &DestinationDraftSnapshot,
    entity_id: &str,
    field_id: &str,
    require_canonical_key: bool,
) -> bool {
    snapshot
        .entities
        .iter()
        .find(|entity| entity.entity_id == entity_id)
        .and_then(|entity| {
            entity
                .fields
                .iter()
                .find(|field| field.field_id == field_id)
        })
        .is_some_and(|field| {
            field.state == DestinationFieldState::Active
                && field.business_required
                && (!require_canonical_key || field.key)
        })
}

fn initial_draft_entities(
    catalogue: &CatalogueModel,
    selected: &[String],
    root: &str,
) -> Result<Vec<DestinationEntityDraft>> {
    let selected_set = selected.iter().collect::<HashSet<_>>();
    if selected_set.len() != selected.len() {
        return Err(KernelError::PolicyInvalid(
            "destination entity selection is ambiguous".to_owned(),
        ));
    }
    let mut ordered_technical_entities = selected.to_vec();
    ordered_technical_entities.sort();
    let mut used_entity_ids = HashSet::new();
    let mut canonical_entity_ids = HashMap::new();
    for technical in ordered_technical_entities {
        let entity = catalogue
            .entities
            .iter()
            .find(|entity| entity.technical_id == technical)
            .ok_or_else(|| KernelError::NotFound(format!("technical entity {technical:?}")))?;
        canonical_entity_ids.insert(
            technical,
            unique_suggested_canonical_id(
                &entity.name,
                &entity.technical_id,
                &mut used_entity_ids,
            )?,
        );
    }
    let mut entities = Vec::with_capacity(selected.len());
    for technical in selected {
        let entity = catalogue
            .entities
            .iter()
            .find(|entity| &entity.technical_id == technical)
            .ok_or_else(|| KernelError::NotFound(format!("technical entity {technical:?}")))?;
        let key_set = entity.key.iter().collect::<HashSet<_>>();
        let mut suggested_properties = entity
            .properties
            .iter()
            .filter(|property| key_set.contains(&property.name) || property.nullable == Some(false))
            .collect::<Vec<_>>();
        suggested_properties.sort_by(|left, right| left.technical_id.cmp(&right.technical_id));
        let mut used_field_ids = HashSet::new();
        let mut fields = suggested_properties
            .into_iter()
            .map(|property| {
                Ok(DestinationFieldDraft {
                    field_id: unique_suggested_canonical_id(
                        &property.name,
                        &property.technical_id,
                        &mut used_field_ids,
                    )?,
                    label: property.name.clone(),
                    logical_type: suggested_logical_type(&property.edm_type).to_owned(),
                    business_required: false,
                    sensitive: false,
                    key: key_set.contains(&property.name),
                    state: DestinationFieldState::NeedsReview,
                    technical_property: Some(property.technical_id.clone()),
                    manual_supplement: None,
                    technical_change_review: None,
                    confirm_max_length: false,
                    max_length: parse_max_length(property.max_length.as_deref()),
                    diff_state: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        fields.sort_by(|left, right| left.field_id.cmp(&right.field_id));
        entities.push(DestinationEntityDraft {
            entity_id: canonical_entity_ids
                .get(technical)
                .cloned()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "destination entity suggestion lost its deterministic identity".to_owned(),
                    )
                })?,
            label: entity.name.clone(),
            root: technical == root,
            technical_entity: Some(entity.technical_id.clone()),
            fields,
        });
    }
    entities.sort_by(|left, right| {
        right
            .root
            .cmp(&left.root)
            .then(left.entity_id.cmp(&right.entity_id))
    });
    Ok(entities)
}

fn suggested_logical_type(edm_type: &str) -> &'static str {
    match edm_type {
        "Edm.Boolean" => "boolean",
        "Edm.Byte" | "Edm.Int16" | "Edm.Int32" | "Edm.Int64" | "Edm.SByte" => "integer",
        "Edm.Decimal" | "Edm.Double" | "Edm.Single" => "decimal",
        "Edm.Date" => "date",
        "Edm.DateTime" | "Edm.DateTimeOffset" => "datetime",
        "Edm.Guid" => "identifier",
        _ => "text",
    }
}

fn parse_max_length(value: Option<&str>) -> Option<u64> {
    value.and_then(|value| value.parse().ok())
}

fn canonical_id(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_alphanumeric() {
            if index == 0 {
                output.push(character.to_ascii_lowercase());
            } else {
                output.push(character);
            }
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }
    if output.is_empty() {
        "field".to_owned()
    } else {
        output.truncate(128);
        output
    }
}

fn unique_suggested_canonical_id(
    simple_name: &str,
    technical_identity: &str,
    used: &mut HashSet<String>,
) -> Result<String> {
    let base = canonical_id(simple_name);
    if used.insert(base.clone()) {
        return Ok(base);
    }
    let digest = sha256_hex(technical_identity.as_bytes());
    let maximum_attempts = MAX_FIELDS.saturating_add(MAX_ENTITIES).max(2);
    for ordinal in 0..maximum_attempts {
        let suffix = if ordinal == 0 {
            format!("_{}", &digest[..12])
        } else {
            format!("_{}_{ordinal}", &digest[..12])
        };
        let retained = 128_usize.saturating_sub(suffix.len());
        let mut candidate = base[..base.len().min(retained)].to_owned();
        candidate.push_str(&suffix);
        if used.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
    Err(KernelError::PolicyInvalid(
        "destination canonical ID collision exceeds its deterministic bound".to_owned(),
    ))
}

fn validate_draft_snapshot(snapshot: &DestinationDraftSnapshot) -> Result<()> {
    validate_profile_id(&snapshot.profile_id)?;
    validate_name(&snapshot.title, "destination profile title", 512)?;
    validate_name(&snapshot.owner, "destination profile owner", 256)?;
    if snapshot.entities.is_empty() || snapshot.entities.len() > MAX_ENTITIES {
        return Err(KernelError::ProfileSemantic(format!(
            "destination draft must contain 1..={MAX_ENTITIES} entities"
        )));
    }
    if snapshot.relationships.len() > MAX_RELATIONSHIPS || snapshot.rules.len() > MAX_RULES {
        return Err(KernelError::ProfileSemantic(
            "destination draft exceeds its relationship or rule bound".to_owned(),
        ));
    }
    if snapshot.compatibility_acknowledgements.len() > MAX_RULES {
        return Err(KernelError::ProfileSemantic(
            "destination draft exceeds its compatibility-acknowledgement bound".to_owned(),
        ));
    }
    let mut acknowledgement_hashes = HashSet::new();
    for acknowledgement in &snapshot.compatibility_acknowledgements {
        validate_sha256(&acknowledgement.finding_hash, "compatibility finding hash")?;
        validate_name(
            &acknowledgement.actor,
            "compatibility acknowledgement actor",
            256,
        )?;
        validate_name(
            &acknowledgement.reason,
            "compatibility acknowledgement reason",
            MAX_TEXT_BYTES,
        )?;
        if !acknowledgement_hashes.insert(acknowledgement.finding_hash.as_str()) {
            return Err(KernelError::ProfileSemantic(
                "destination draft repeats a compatibility acknowledgement".to_owned(),
            ));
        }
    }
    let mut entity_ids = HashSet::new();
    let mut total_fields = 0_usize;
    let mut roots = 0_usize;
    for entity in &snapshot.entities {
        validate_identifier(&entity.entity_id, "canonical entity ID")?;
        if entity.entity_id.contains('.') {
            return Err(KernelError::PolicyInvalid(
                "canonical entity ID must not contain '.' because rule targets use entity.field"
                    .to_owned(),
            ));
        }
        validate_name(&entity.label, "canonical entity label", 512)?;
        if entity.root {
            roots = roots.saturating_add(1);
        }
        if !entity_ids.insert(entity.entity_id.as_str()) {
            return Err(KernelError::ProfileSemantic(
                "destination draft repeats a canonical entity ID".to_owned(),
            ));
        }
        if let Some(technical) = entity.technical_entity.as_deref() {
            validate_technical_identity(technical, "technical entity")?;
        }
        if entity.fields.is_empty() {
            return Err(KernelError::ProfileSemantic(format!(
                "destination entity {:?} has no activated fields",
                entity.entity_id
            )));
        }
        let mut field_ids = HashSet::new();
        for field in &entity.fields {
            total_fields = total_fields.saturating_add(1);
            validate_identifier(&field.field_id, "canonical field ID")?;
            validate_name(&field.label, "canonical field label", 512)?;
            validate_logical_type(&field.logical_type)?;
            if !field_ids.insert(field.field_id.as_str()) {
                return Err(KernelError::ProfileSemantic(format!(
                    "destination entity {:?} repeats field {:?}",
                    entity.entity_id, field.field_id
                )));
            }
            match (
                field.technical_property.as_deref(),
                field.manual_supplement.as_ref(),
            ) {
                (Some(technical), None) => {
                    validate_technical_identity(technical, "technical property")?;
                }
                (None, Some(manual)) => {
                    validate_identifier(&manual.supplement_id, "manual supplement ID")?;
                    validate_name(&manual.actor, "manual supplement actor", 256)?;
                    validate_name(&manual.reason, "manual supplement reason", MAX_TEXT_BYTES)?;
                }
                (Some(technical), Some(manual)) => {
                    validate_technical_identity(technical, "linked technical property")?;
                    validate_identifier(&manual.supplement_id, "manual supplement ID")?;
                    validate_name(&manual.actor, "manual supplement actor", 256)?;
                    validate_name(&manual.reason, "manual supplement reason", MAX_TEXT_BYTES)?;
                }
                _ => {
                    return Err(KernelError::ProfileSemantic(format!(
                        "destination field {:?}.{:?} requires technical or explicit manual provenance",
                        entity.entity_id, field.field_id
                    )));
                }
            }
            if let Some(review) = &field.technical_change_review {
                validate_name(&review.actor, "technical-change reviewer", 256)?;
                validate_name(
                    &review.reason,
                    "technical-change review reason",
                    MAX_TEXT_BYTES,
                )?;
                if field.diff_state != Some(DestinationDiffState::TechnicallyChanged)
                    || field.state != DestinationFieldState::Active
                    || field.technical_property.is_none()
                {
                    return Err(KernelError::ProfileSemantic(format!(
                        "destination field {:?}.{:?} has inapplicable revised-technical-evidence review",
                        entity.entity_id, field.field_id
                    )));
                }
            }
            if field.key && field.state == DestinationFieldState::Active && !field.business_required
            {
                return Err(KernelError::ProfileSemantic(format!(
                    "destination key {:?}.{:?} must be confirmed business-required",
                    entity.entity_id, field.field_id
                )));
            }
            if field.confirm_max_length && field.max_length.is_none() {
                return Err(KernelError::ProfileSemantic(format!(
                    "destination field {:?}.{:?} confirms no maximum length",
                    entity.entity_id, field.field_id
                )));
            }
        }
    }
    if roots != 1 || total_fields > MAX_FIELDS {
        return Err(KernelError::ProfileSemantic(format!(
            "destination draft requires one root and at most {MAX_FIELDS} fields"
        )));
    }
    let entity_field_sets = snapshot
        .entities
        .iter()
        .map(|entity| {
            (
                entity.entity_id.as_str(),
                entity
                    .fields
                    .iter()
                    .map(|field| field.field_id.as_str())
                    .collect::<HashSet<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let root_entity_id = snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .map(|entity| entity.entity_id.as_str())
        .ok_or_else(|| {
            KernelError::ProfileSemantic("destination draft has no root entity".to_owned())
        })?;
    let mut relationship_ids = HashSet::new();
    for relationship in &snapshot.relationships {
        validate_identifier(&relationship.relationship_id, "relationship ID")?;
        if !relationship_ids.insert(relationship.relationship_id.as_str())
            || !matches!(relationship.cardinality.as_str(), "0..*" | "1..*")
            || relationship.key_pairs.is_empty()
            || relationship.key_pairs.len() > 16
            || !entity_field_sets.contains_key(relationship.from_entity_id.as_str())
            || !entity_field_sets.contains_key(relationship.to_entity_id.as_str())
            || relationship.from_entity_id != root_entity_id
            || relationship.to_entity_id == root_entity_id
        {
            return Err(KernelError::ProfileSemantic(
                "destination relationship is incomplete or outside bounded direct-child scope"
                    .to_owned(),
            ));
        }
        for pair in &relationship.key_pairs {
            if !entity_field_sets[relationship.from_entity_id.as_str()]
                .contains(pair.from_field_id.as_str())
                || !entity_field_sets[relationship.to_entity_id.as_str()]
                    .contains(pair.to_field_id.as_str())
            {
                return Err(KernelError::ProfileSemantic(
                    "destination relationship key pair does not resolve".to_owned(),
                ));
            }
        }
    }
    let mut rule_ids = HashSet::new();
    let mut reference_table_versions = HashMap::<(&str, &str), &str>::new();
    for rule in &snapshot.rules {
        validate_identifier(&rule.rule_id, "destination rule ID")?;
        if !rule_ids.insert(rule.rule_id.as_str())
            || !matches!(
                rule.kind.as_str(),
                "required"
                    | "max_length"
                    | "unique"
                    | "allowed_values"
                    | "pattern"
                    | "conditional_required"
                    | "reference_lookup"
            )
        {
            return Err(KernelError::ProfileSemantic(
                "destination rule kind or ID is outside the bounded editor".to_owned(),
            ));
        }
        if !matches!(rule.severity.as_str(), "error" | "warning" | "information") {
            return Err(KernelError::ProfileSemantic(format!(
                "destination rule {:?} has unsupported severity",
                rule.rule_id
            )));
        }
        validate_name(&rule.message, "destination rule message", MAX_TEXT_BYTES)?;
        let (target_entity_id, _) = resolve_destination_rule_field(snapshot, &rule.target)
            .ok_or_else(|| {
                KernelError::ProfileSemantic(format!(
                    "destination rule {:?} target must resolve to one exact Entity.field",
                    rule.rule_id
                ))
            })?;
        match rule.kind.as_str() {
            "conditional_required" => {
                let parameters = exact_rule_parameters(rule, &["equals", "whenField"])?;
                let when_field = parameters
                    .get("whenField")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(format!(
                            "destination rule {:?} whenField must be a string",
                            rule.rule_id
                        ))
                    })?;
                let equals = parameters
                    .get("equals")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(format!(
                            "destination rule {:?} equals must be a string",
                            rule.rule_id
                        ))
                    })?;
                if equals.len() > MAX_TEXT_BYTES {
                    return Err(KernelError::ProfileSemantic(format!(
                        "destination rule {:?} equals exceeds its 4096-byte bound",
                        rule.rule_id
                    )));
                }
                let (when_entity_id, _) =
                    resolve_destination_rule_field(snapshot, when_field).ok_or_else(|| {
                        KernelError::ProfileSemantic(format!(
                            "destination rule {:?} whenField must resolve to one exact Entity.field",
                            rule.rule_id
                        ))
                    })?;
                if when_entity_id != target_entity_id {
                    return Err(KernelError::ProfileSemantic(format!(
                        "destination rule {:?} whenField must target the same entity as the rule",
                        rule.rule_id
                    )));
                }
            }
            "reference_lookup" => {
                let parameters = exact_rule_parameters(rule, &["tableId", "tableVersion"])?;
                let table_id = parameters
                    .get("tableId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(format!(
                            "destination rule {:?} tableId must be a string",
                            rule.rule_id
                        ))
                    })?;
                let table_version = parameters
                    .get("tableVersion")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ProfileSemantic(format!(
                            "destination rule {:?} tableVersion must be a string",
                            rule.rule_id
                        ))
                    })?;
                validate_reference_table_id(table_id, &rule.rule_id)?;
                validate_reference_table_version(table_version, &rule.rule_id)?;
                if let Some(existing_version) =
                    reference_table_versions.insert((target_entity_id, table_id), table_version)
                    && existing_version != table_version
                {
                    return Err(KernelError::ProfileSemantic(format!(
                        "destination reference rules for {target_entity_id}.{table_id} declare conflicting table versions"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_destination_rule_field<'a>(
    snapshot: &'a DestinationDraftSnapshot,
    target: &str,
) -> Option<(&'a str, &'a DestinationFieldDraft)> {
    let mut parts = target.split('.');
    let (Some(entity_id), Some(field_id), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if entity_id.is_empty() || field_id.is_empty() {
        return None;
    }
    let entity = snapshot
        .entities
        .iter()
        .find(|entity| entity.entity_id == entity_id)?;
    let field = entity
        .fields
        .iter()
        .find(|field| field.field_id == field_id)?;
    Some((entity.entity_id.as_str(), field))
}

fn exact_rule_parameters<'a>(
    rule: &'a DestinationRuleDraft,
    expected: &[&str],
) -> Result<&'a serde_json::Map<String, Value>> {
    let parameters = rule.parameters.as_object().ok_or_else(|| {
        KernelError::ProfileSemantic(format!(
            "destination rule {:?} parameters must be an object",
            rule.rule_id
        ))
    })?;
    let actual = parameters
        .keys()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let expected = expected.iter().copied().collect::<HashSet<_>>();
    if actual != expected {
        return Err(KernelError::ProfileSemantic(format!(
            "destination rule {:?} parameter keys must be exactly {:?}",
            rule.rule_id, expected
        )));
    }
    Ok(parameters)
}

fn validate_reference_table_id(table_id: &str, rule_id: &str) -> Result<()> {
    if table_id.is_empty()
        || table_id.len() > 128
        || !table_id.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'-'))
        })
    {
        return Err(KernelError::ProfileSemantic(format!(
            "destination rule {rule_id:?} tableId must use the bounded lowercase mapping identifier form"
        )));
    }
    Ok(())
}

fn validate_reference_table_version(table_version: &str, rule_id: &str) -> Result<()> {
    let components = table_version.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || table_version.len() > 32
        || components
            .iter()
            .any(|component| component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(KernelError::ProfileSemantic(format!(
            "destination rule {rule_id:?} tableVersion must be numeric major.minor.patch"
        )));
    }
    Ok(())
}

fn validate_draft_against_catalogue(
    snapshot: &DestinationDraftSnapshot,
    authority: &CatalogueAuthorityRow,
    prior_technical_properties: &HashSet<String>,
) -> Result<()> {
    let model = catalogue_model(authority)?;
    let entities = model
        .entities
        .iter()
        .map(|entity| (entity.technical_id.as_str(), entity))
        .collect::<HashMap<_, _>>();
    let mut seen_technical_entities = HashSet::new();
    for entity in &snapshot.entities {
        if let Some(technical_entity) = entity.technical_entity.as_deref() {
            if !seen_technical_entities.insert(technical_entity) {
                return Err(KernelError::ProfileSemantic(
                    "one technical entity cannot be selected more than once".to_owned(),
                ));
            }
            let properties = entities
                .get(technical_entity)
                .map(|catalogue_entity| {
                    catalogue_entity
                        .properties
                        .iter()
                        .map(|property| property.technical_id.as_str())
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default();
            for field in &entity.fields {
                let Some(technical_property) = field.technical_property.as_deref() else {
                    continue;
                };
                let retained_removed_provenance = field.diff_state
                    == Some(DestinationDiffState::Removed)
                    && prior_technical_properties.contains(technical_property);
                if !technical_property.starts_with(&format!("{technical_entity}."))
                    || (!properties.contains(technical_property) && !retained_removed_provenance)
                {
                    return Err(KernelError::ProfileSemantic(format!(
                        "technical property {technical_property:?} does not resolve on {technical_entity:?}"
                    )));
                }
            }
            if !entities.contains_key(technical_entity)
                && !entity.fields.iter().all(|field| {
                    field.technical_property.as_ref().is_some_and(|property| {
                        field.diff_state == Some(DestinationDiffState::Removed)
                            && prior_technical_properties.contains(property)
                    })
                })
            {
                return Err(KernelError::ProfileSemantic(format!(
                    "technical entity {technical_entity:?} does not resolve to the exact catalogue"
                )));
            }
        }
    }
    for relationship in &snapshot.relationships {
        let Some(technical_navigation) = relationship.technical_navigation.as_deref() else {
            continue;
        };
        let from_entity = snapshot
            .entities
            .iter()
            .find(|entity| entity.entity_id == relationship.from_entity_id)
            .and_then(|entity| entity.technical_entity.as_deref())
            .ok_or_else(|| {
                KernelError::ProfileSemantic(format!(
                    "relationship {} has no technical source entity",
                    relationship.relationship_id
                ))
            })?;
        let catalogue_entity = entities.get(from_entity).ok_or_else(|| {
            KernelError::ProfileSemantic(format!(
                "relationship {} source entity does not resolve",
                relationship.relationship_id
            ))
        })?;
        if !catalogue_entity
            .navigation_identities
            .iter()
            .any(|identity| identity == technical_navigation)
        {
            return Err(KernelError::ProfileSemantic(format!(
                "technical navigation {technical_navigation:?} does not resolve on {from_entity:?}"
            )));
        }
    }
    Ok(())
}

fn rebase_source_technical_properties(
    connection: &Connection,
    result_draft_id: &str,
) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(
        "SELECT rebase.from_draft_id, rebase.from_revision_hash,
                source.catalogue_authority_id, source.catalogue_hash
         FROM destination_rebases AS rebase
         JOIN destination_profile_draft_heads AS source
           ON source.draft_id = rebase.from_draft_id
         WHERE rebase.result_draft_id = ?1
         ORDER BY rebase.rebase_id",
    )?;
    let rows = statement
        .query_map([result_draft_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let Some((source_draft_id, source_revision_hash, authority_id, authority_hash)) = rows.first()
    else {
        return Ok(HashSet::new());
    };
    if rows.len() != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "destination rebase draft {result_draft_id:?} has ambiguous source provenance"
        )));
    }
    let source_generation: i64 = connection.query_row(
        "SELECT generation FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 AND revision_hash = ?2",
        params![source_draft_id, source_revision_hash],
        |row| row.get(0),
    )?;
    let source_generation = u64::try_from(source_generation).map_err(|_| {
        KernelError::IntegrityFailed("destination rebase source generation is negative".to_owned())
    })?;
    load_draft_snapshot(
        connection,
        source_draft_id,
        source_generation,
        source_revision_hash,
    )?;
    let authority = load_catalogue_authority(connection, authority_id)?;
    if authority.catalogue_hash != *authority_hash {
        return Err(KernelError::IntegrityFailed(
            "destination rebase source catalogue hash mismatch".to_owned(),
        ));
    }
    Ok(catalogue_model(&authority)?
        .entities
        .into_iter()
        .flat_map(|entity| {
            entity
                .properties
                .into_iter()
                .map(|property| property.technical_id)
        })
        .collect())
}

fn validate_technical_change_reviews(
    connection: &Connection,
    draft_id: &str,
    snapshot: &DestinationDraftSnapshot,
) -> Result<()> {
    let reviewed_properties = snapshot
        .entities
        .iter()
        .flat_map(|entity| entity.fields.iter())
        .filter(|field| field.technical_change_review.is_some())
        .map(|field| {
            field.technical_property.as_deref().ok_or_else(|| {
                KernelError::ProfileSemantic(
                    "technical-change review has no exact technical property".to_owned(),
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if reviewed_properties.is_empty() {
        return Ok(());
    }
    let mut statement = connection.prepare(
        "SELECT canonical_diff_json, diff_hash
         FROM destination_rebases
         WHERE result_draft_id = ?1
         ORDER BY rebase_id",
    )?;
    let rows = statement
        .query_map([draft_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let [(canonical_diff_json, diff_hash)] = rows.as_slice() else {
        return Err(KernelError::ProfileSemantic(
            "technical-change review requires one exact immutable rebase diff".to_owned(),
        ));
    };
    let diff = parse_unique_json(canonical_diff_json.as_bytes())?;
    if canonical_json_string(&diff)? != *canonical_diff_json || canonical_hash(&diff)? != *diff_hash
    {
        return Err(KernelError::IntegrityFailed(
            "technical-change review rebase diff failed canonical replay".to_owned(),
        ));
    }
    let items = diff.get("items").and_then(Value::as_array).ok_or_else(|| {
        KernelError::IntegrityFailed(
            "technical-change rebase diff has no item inventory".to_owned(),
        )
    })?;
    let mut states = HashMap::new();
    for item in items {
        let technical_identity = required_json_string(item, "technicalIdentity")?;
        let state = required_json_string(item, "state")?;
        if states.insert(technical_identity, state).is_some() {
            return Err(KernelError::IntegrityFailed(
                "technical-change rebase diff repeats a technical identity".to_owned(),
            ));
        }
    }
    for technical_property in reviewed_properties {
        if states.get(technical_property).copied() != Some("TECHNICALLY_CHANGED") {
            return Err(KernelError::ProfileSemantic(format!(
                "technical-change review for {technical_property:?} is not supported by the exact immutable rebase diff"
            )));
        }
    }
    Ok(())
}

fn freeze_blockers(
    connection: &Connection,
    draft_id: &str,
    snapshot: &DestinationDraftSnapshot,
    authority: &CatalogueAuthorityRow,
    prior_technical_properties: &HashSet<String>,
) -> Result<Vec<String>> {
    let mut blockers = Vec::new();
    if let Err(error) = validate_draft_snapshot(snapshot) {
        blockers.push(error.to_string());
        return Ok(blockers);
    }
    if let Err(error) = validate_technical_change_reviews(connection, draft_id, snapshot) {
        blockers.push(error.to_string());
    }
    if let Err(error) =
        validate_draft_against_catalogue(snapshot, authority, prior_technical_properties)
    {
        blockers.push(error.to_string());
    }
    let scoped_compatibility = scoped_compatibility_report(snapshot, authority)?;
    for finding in scoped_compatibility
        .findings
        .iter()
        .filter(|finding| finding.blocking)
    {
        blockers.push(format!(
            "selected destination scope has unresolved {} evidence at {}",
            finding.code.to_ascii_lowercase(),
            finding.technical_location
        ));
    }
    for acknowledgement in &snapshot.compatibility_acknowledgements {
        match scoped_compatibility
            .findings
            .iter()
            .find(|finding| finding.finding_hash == acknowledgement.finding_hash)
        {
            None => blockers.push(format!(
                "compatibility acknowledgement {} does not resolve to the exact catalogue report",
                acknowledgement.finding_hash
            )),
            Some(finding) if finding.blocking => blockers.push(format!(
                "selected-scope blocker {} at {} cannot be acknowledged away",
                finding.code.to_ascii_lowercase(),
                finding.technical_location
            )),
            Some(_) => {}
        }
    }
    let technical_types = if authority.origin == "EDMX" {
        catalogue_model(authority)?
            .entities
            .into_iter()
            .flat_map(|entity| {
                entity
                    .properties
                    .into_iter()
                    .map(|property| (property.technical_id, property.edm_type))
            })
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    for entity in &snapshot.entities {
        for field in &entity.fields {
            if field.state == DestinationFieldState::NeedsReview {
                blockers.push(format!(
                    "{}.{} still needs practitioner review",
                    entity.entity_id, field.field_id
                ));
            }
            if field.state == DestinationFieldState::Available {
                continue;
            }
            if field.business_required
                && !snapshot.rules.iter().any(|rule| {
                    rule.kind == "required"
                        && rule.target == format!("{}.{}", entity.entity_id, field.field_id)
                })
            {
                blockers.push(format!(
                    "{}.{} is business-required but has no explicit required rule",
                    entity.entity_id, field.field_id
                ));
            }
            if field.confirm_max_length
                && !snapshot.rules.iter().any(|rule| {
                    rule.kind == "max_length"
                        && rule.target == format!("{}.{}", entity.entity_id, field.field_id)
                        && rule.parameters.get("maximum").and_then(Value::as_u64)
                            == field.max_length
                })
            {
                blockers.push(format!(
                    "{}.{} confirms maximum-length enforcement but has no exact max_length rule",
                    entity.entity_id, field.field_id
                ));
            }
            let unresolved_rebase_evidence = field.diff_state
                == Some(DestinationDiffState::Removed)
                || (field.diff_state == Some(DestinationDiffState::TechnicallyChanged)
                    && field.technical_change_review.is_none());
            if field.state == DestinationFieldState::Active && unresolved_rebase_evidence {
                blockers.push(format!(
                    "{}.{} has unresolved revised-catalogue evidence",
                    entity.entity_id, field.field_id
                ));
            }
            if field.state == DestinationFieldState::Active
                && field
                    .technical_property
                    .as_ref()
                    .and_then(|technical| technical_types.get(technical))
                    .is_some_and(|edm_type| !edm_type.starts_with("Edm."))
            {
                blockers.push(format!(
                    "{}.{} selects an unresolved non-primitive EDMX type",
                    entity.entity_id, field.field_id
                ));
            }
        }
    }
    for rule in &snapshot.rules {
        let target_field = rule
            .target
            .split_once('.')
            .and_then(|(entity_id, field_id)| {
                snapshot
                    .entities
                    .iter()
                    .find(|entity| entity.entity_id == entity_id)
                    .and_then(|entity| {
                        entity
                            .fields
                            .iter()
                            .find(|field| field.field_id == field_id)
                    })
            });
        match rule.kind.as_str() {
            "required"
                if !target_field.is_some_and(|field| {
                    field.state == DestinationFieldState::Active && field.business_required
                }) =>
            {
                blockers.push(format!(
                    "required rule {} must target an active business-required field",
                    rule.rule_id
                ));
            }
            "max_length"
                if !target_field.is_some_and(|field| {
                    field.state == DestinationFieldState::Active
                        && field.confirm_max_length
                        && field.max_length.is_some_and(|maximum| {
                            (1..=1_000_000).contains(&maximum)
                                && rule.parameters.get("maximum").and_then(Value::as_u64)
                                    == Some(maximum)
                        })
                }) =>
            {
                blockers.push(format!(
                    "max_length rule {} must match an active field's exact confirmed bounded length",
                    rule.rule_id
                ));
            }
            "conditional_required" => {
                if !target_field.is_some_and(|field| field.state == DestinationFieldState::Active) {
                    blockers.push(format!(
                        "conditional_required rule {} must target an active field",
                        rule.rule_id
                    ));
                }
                let when_field = rule
                    .parameters
                    .get("whenField")
                    .and_then(Value::as_str)
                    .and_then(|target| {
                        resolve_destination_rule_field(snapshot, target).map(|(_, field)| field)
                    });
                if !when_field.is_some_and(|field| field.state == DestinationFieldState::Active) {
                    blockers.push(format!(
                        "conditional_required rule {} must use an active same-entity condition field",
                        rule.rule_id
                    ));
                }
            }
            "reference_lookup"
                if !target_field
                    .is_some_and(|field| field.state == DestinationFieldState::Active) =>
            {
                blockers.push(format!(
                    "reference_lookup rule {} must target an active field",
                    rule.rule_id
                ));
            }
            _ => {}
        }
    }
    let root_entity_id = snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .map(|entity| entity.entity_id.as_str());
    for child in snapshot.entities.iter().filter(|entity| !entity.root) {
        let relationship_count = snapshot
            .relationships
            .iter()
            .filter(|relationship| {
                relationship.confirmed
                    && Some(relationship.from_entity_id.as_str()) == root_entity_id
                    && relationship.to_entity_id == child.entity_id
            })
            .count();
        if relationship_count != 1 {
            blockers.push(format!(
                "child entity {} requires one confirmed keyed relationship from the root",
                child.entity_id
            ));
        }
    }
    for relationship in &snapshot.relationships {
        if !relationship.confirmed {
            blockers.push(format!(
                "relationship {} is not explicitly confirmed",
                relationship.relationship_id
            ));
            continue;
        }
        for pair in &relationship.key_pairs {
            for (entity_id, field_id, require_canonical_key) in [
                (&relationship.from_entity_id, &pair.from_field_id, true),
                (&relationship.to_entity_id, &pair.to_field_id, false),
            ] {
                let safe_key = relationship_key_field_is_safe(
                    snapshot,
                    entity_id,
                    field_id,
                    require_canonical_key,
                );
                if !safe_key {
                    blockers.push(format!(
                        "relationship {} {} key {}.{} must be active, business-required{}",
                        relationship.relationship_id,
                        if require_canonical_key {
                            "source"
                        } else {
                            "child"
                        },
                        entity_id,
                        field_id,
                        if require_canonical_key {
                            ", and a canonical entity key"
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
    }
    blockers.sort();
    blockers.dedup();
    Ok(blockers)
}

fn build_profile_document(
    snapshot: &DestinationDraftSnapshot,
    authority: &CatalogueAuthorityRow,
    version: &str,
) -> Result<Value> {
    if !snapshot.entities.iter().any(|entity| entity.root) {
        return Err(KernelError::ProfileSemantic(
            "destination draft has no root entity".to_owned(),
        ));
    }
    let entities = snapshot
        .entities
        .iter()
        .map(|entity| {
            let fields = entity
                .fields
                .iter()
                .filter(|field| field.state == DestinationFieldState::Active)
                .map(|field| {
                    let mut value = json!({
                        "fieldId": field.field_id,
                        "label": field.label,
                        "logicalType": field.logical_type,
                        "required": field.business_required,
                        "sensitive": field.sensitive,
                    });
                    if let Some(technical) = field.technical_property.as_ref() {
                        value["technicalProperty"] = json!(technical);
                    }
                    if field.confirm_max_length {
                        value["maxLength"] = json!(field.max_length);
                    }
                    value
                })
                .collect::<Vec<_>>();
            let key = entity
                .fields
                .iter()
                .filter(|field| field.state == DestinationFieldState::Active && field.key)
                .map(|field| field.field_id.clone())
                .collect::<Vec<_>>();
            let mut value = json!({
                "entityId": entity.entity_id,
                "root": entity.root,
                "key": key,
                "fields": fields,
            });
            if let Some(technical) = entity.technical_entity.as_ref() {
                value["technicalEntity"] = json!(technical);
            }
            value
        })
        .collect::<Vec<_>>();
    let relationships = snapshot
        .relationships
        .iter()
        .filter(|relationship| relationship.confirmed)
        .map(|relationship| {
            let mut value = json!({
                "relationshipId": relationship.relationship_id,
                "from": relationship.from_entity_id,
                "to": relationship.to_entity_id,
                "cardinality": relationship.cardinality,
                "keyPairs": relationship.key_pairs,
            });
            if let Some(technical) = relationship.technical_navigation.as_ref() {
                value["technicalNavigation"] = json!(technical);
            }
            value
        })
        .collect::<Vec<_>>();
    let rules = snapshot
        .rules
        .iter()
        .map(|rule| {
            json!({
                "ruleId": rule.rule_id,
                "severity": rule.severity,
                "kind": rule.kind,
                "target": rule.target,
                "message": rule.message,
                "parameters": rule.parameters,
            })
        })
        .collect::<Vec<_>>();
    let technical_source = if authority.origin == "EDMX" {
        json!({
            "kind": "odata_edmx",
            "artifact": authority.artifact_label,
            "sha256": authority.snapshot_sha256,
        })
    } else {
        json!({
            "kind": "manual_catalogue",
            "artifact": authority.artifact_label,
            "sha256": authority.catalogue_hash,
        })
    };
    Ok(json!({
        "profileId": snapshot.profile_id,
        "version": version,
        "status": "REVIEWED_DRAFT",
        "title": snapshot.title,
        "owner": snapshot.owner,
        "technicalSources": [technical_source],
        "entities": entities,
        "relationships": relationships,
        "rules": rules,
        "publicationAdapters": [],
    }))
}

fn build_origin_binding_value(
    engagement_id: &str,
    profile: &ProfileVersionView,
    head: &DraftHeadRow,
    authority: &CatalogueAuthorityRow,
    snapshot: &DestinationDraftSnapshot,
) -> Result<Value> {
    let scoped_completeness_hash = scoped_compatibility_report(snapshot, authority)?
        .scoped_completeness_hash
        .ok_or_else(|| {
            KernelError::IntegrityFailed(
                "destination scoped completeness evidence has no canonical hash".to_owned(),
            )
        })?;
    let provenance = snapshot
        .entities
        .iter()
        .flat_map(|entity| {
            entity.fields.iter().map(move |field| {
                json!({
                    "entityId": entity.entity_id,
                    "fieldId": field.field_id,
                    "technicalEntity": entity.technical_entity,
                    "technicalProperty": field.technical_property,
                    "manualSupplement": field.manual_supplement,
                    "technicalChangeReview": field.technical_change_review,
                })
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schemaVersion": ORIGIN_BINDING_SCHEMA,
        "engagementId": engagement_id,
        "profileId": profile.profile_id,
        "profileVersion": profile.profile_version,
        "profileHash": profile.profile_hash,
        "draftId": head.draft_id,
        "draftGeneration": head.generation,
        "draftRevisionHash": head.head_revision_hash,
        "catalogueAuthorityId": authority.id,
        "catalogueHash": authority.catalogue_hash,
        "compatibilityReportHash": authority.compatibility_report_hash,
        "scopedCompletenessHash": scoped_completeness_hash,
        "provenance": provenance,
        "acknowledgements": snapshot.compatibility_acknowledgements,
    }))
}

fn rebase_snapshot(
    source_draft: &DestinationDraftSnapshot,
    source_catalogue: &CatalogueModel,
    target_catalogue: &CatalogueModel,
) -> Result<(DestinationDraftSnapshot, Value)> {
    let source_entities = source_catalogue
        .entities
        .iter()
        .map(|entity| (entity.technical_id.as_str(), entity))
        .collect::<HashMap<_, _>>();
    let target_entities = target_catalogue
        .entities
        .iter()
        .map(|entity| (entity.technical_id.as_str(), entity))
        .collect::<HashMap<_, _>>();
    let mut rebased = source_draft.clone();
    let mut items = Vec::new();
    for entity in &mut rebased.entities {
        let Some(technical_entity) = entity.technical_entity.as_deref() else {
            continue;
        };
        let source_entity = source_entities.get(technical_entity).ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "destination rebase source catalogue lacks selected entity {technical_entity:?}"
            ))
        })?;
        let source_properties = source_entity
            .properties
            .iter()
            .map(|property| (property.technical_id.as_str(), property))
            .collect::<HashMap<_, _>>();
        let target_entity = target_entities.get(technical_entity).copied();
        let target_properties = target_entity.map_or_else(HashMap::new, |target_entity| {
            target_entity
                .properties
                .iter()
                .map(|property| (property.technical_id.as_str(), property))
                .collect::<HashMap<_, _>>()
        });
        let mut manual_matches = BTreeMap::new();
        for field in &mut entity.fields {
            field.technical_change_review = None;
            if let Some(technical_property) = field.technical_property.as_deref() {
                let source_property =
                    source_properties.get(technical_property).ok_or_else(|| {
                        KernelError::IntegrityFailed(format!(
                            "destination rebase draft property {technical_property:?} is absent from its source catalogue"
                        ))
                    })?;
                match target_properties.get(technical_property) {
                    Some(target_property)
                        if catalogue_property_signature(source_entity, source_property)?
                            == catalogue_property_signature(
                                target_entity.expect("target property requires target entity"),
                                target_property,
                            )? =>
                    {
                        field.diff_state = Some(DestinationDiffState::Unchanged);
                    }
                    Some(_) => {
                        field.diff_state = Some(DestinationDiffState::TechnicallyChanged);
                        field.state = DestinationFieldState::NeedsReview;
                    }
                    None => {
                        field.diff_state = Some(DestinationDiffState::Removed);
                        field.state = DestinationFieldState::NeedsReview;
                    }
                }
            } else if field.manual_supplement.is_some() {
                let potential = target_entity.and_then(|target_entity| {
                    target_entity.properties.iter().find(|property| {
                        !source_properties.contains_key(property.technical_id.as_str())
                            && (property.name == field.field_id || property.name == field.label)
                    })
                });
                if let Some(property) = potential {
                    field.diff_state = Some(DestinationDiffState::ManualPotentialMatch);
                    field.state = DestinationFieldState::NeedsReview;
                    manual_matches.insert(property.technical_id.clone(), field.field_id.clone());
                }
            }
        }

        for source_property in &source_entity.properties {
            let state = match target_properties.get(source_property.technical_id.as_str()) {
                Some(target_property)
                    if catalogue_property_signature(source_entity, source_property)?
                        == catalogue_property_signature(
                            target_entity.expect("target property requires target entity"),
                            target_property,
                        )? =>
                {
                    "UNCHANGED"
                }
                Some(_) => "TECHNICALLY_CHANGED",
                None => "REMOVED",
            };
            items.push(json!({
                "technicalIdentity": source_property.technical_id,
                "state": state,
            }));
        }

        if let Some(target_entity) = target_entity {
            let key_set = target_entity.key.iter().collect::<HashSet<_>>();
            let mut target_properties = target_entity.properties.iter().collect::<Vec<_>>();
            target_properties.sort_by(|left, right| left.technical_id.cmp(&right.technical_id));
            let mut used_field_ids = entity
                .fields
                .iter()
                .map(|field| field.field_id.clone())
                .collect::<HashSet<_>>();
            for property in target_properties {
                if source_properties.contains_key(property.technical_id.as_str()) {
                    continue;
                }
                if let Some(field_id) = manual_matches.get(&property.technical_id) {
                    items.push(json!({
                        "technicalIdentity": property.technical_id,
                        "state": "MANUAL_POTENTIAL_MATCH",
                        "canonicalFieldId": field_id,
                    }));
                }
                let suggested =
                    key_set.contains(&property.name) || property.nullable == Some(false);
                items.push(json!({
                    "technicalIdentity": property.technical_id,
                    "state": "ADDED",
                    "preactivated": suggested,
                }));
                if suggested && !manual_matches.contains_key(&property.technical_id) {
                    entity.fields.push(DestinationFieldDraft {
                        field_id: unique_suggested_canonical_id(
                            &property.name,
                            &property.technical_id,
                            &mut used_field_ids,
                        )?,
                        label: property.name.clone(),
                        logical_type: suggested_logical_type(&property.edm_type).to_owned(),
                        business_required: false,
                        sensitive: false,
                        key: key_set.contains(&property.name),
                        state: DestinationFieldState::NeedsReview,
                        technical_property: Some(property.technical_id.clone()),
                        manual_supplement: None,
                        technical_change_review: None,
                        confirm_max_length: false,
                        max_length: parse_max_length(property.max_length.as_deref()),
                        diff_state: Some(DestinationDiffState::Added),
                    });
                }
            }
        }
        entity
            .fields
            .sort_by(|left, right| left.field_id.cmp(&right.field_id));
    }
    items.sort_by(|left, right| {
        left.get("technicalIdentity")
            .and_then(Value::as_str)
            .cmp(&right.get("technicalIdentity").and_then(Value::as_str))
    });
    Ok((
        rebased,
        json!({
            "schemaVersion": DESTINATION_REBASE_SCHEMA,
            "algorithm": REBASE_ALGORITHM,
            "items": items
        }),
    ))
}

fn catalogue_property_signature(
    entity: &CatalogueEntity,
    property: &CatalogueProperty,
) -> Result<String> {
    canonical_hash(&json!({
        "technicalIdentity": property.technical_id,
        "edmType": property.edm_type,
        "nullable": property.nullable,
        "maxLength": property.max_length,
        "precision": property.precision,
        "scale": property.scale,
        "keyCandidate": entity.key.iter().any(|name| name == &property.name),
    }))
}

fn diff_counts(value: &Value) -> Result<BTreeMap<String, u64>> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::IntegrityFailed("destination rebase diff has no items".to_owned())
        })?;
    let mut counts = BTreeMap::new();
    for item in items {
        let state = item.get("state").and_then(Value::as_str).ok_or_else(|| {
            KernelError::IntegrityFailed("destination rebase item has no state".to_owned())
        })?;
        *counts.entry(state.to_owned()).or_default() += 1;
    }
    Ok(counts)
}

fn load_catalogue_authority(
    connection: &Connection,
    catalogue_authority_id: &str,
) -> Result<CatalogueAuthorityRow> {
    let row = connection
        .query_row(
            "SELECT catalogue_authority_id, origin, snapshot_id, snapshot_sha256,
                    catalogue_path, catalogue_file_sha256,
                    catalogue_payload_sha256, canonical_catalogue_json,
                    catalogue_hash, compatibility_report_json,
                    compatibility_report_hash, report_algorithm, artifact_label,
                    odata_generation, created_at
             FROM destination_catalogue_authorities
             WHERE catalogue_authority_id = ?1",
            [catalogue_authority_id],
            |row| {
                Ok(CatalogueAuthorityRow {
                    id: row.get(0)?,
                    origin: row.get(1)?,
                    snapshot_id: row.get(2)?,
                    snapshot_sha256: row.get(3)?,
                    catalogue_path: row.get(4)?,
                    catalogue_file_sha256: row.get(5)?,
                    catalogue_payload_sha256: row.get(6)?,
                    canonical_catalogue_json: row.get(7)?,
                    catalogue_hash: row.get(8)?,
                    compatibility_report_json: row.get(9)?,
                    compatibility_report_hash: row.get(10)?,
                    report_algorithm: row.get(11)?,
                    artifact_label: row.get(12)?,
                    odata_generation: row.get(13)?,
                    created_at: row.get(14)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::NotFound(format!(
                "destination catalogue authority {catalogue_authority_id}"
            ))
        })?;
    verify_catalogue_authority(connection, &row)?;
    Ok(row)
}

fn verify_catalogue_authority(connection: &Connection, row: &CatalogueAuthorityRow) -> Result<()> {
    validate_sha256(&row.catalogue_payload_sha256, "catalogue payload hash")?;
    validate_sha256(&row.catalogue_hash, "catalogue authority hash")?;
    validate_sha256(&row.compatibility_report_hash, "compatibility report hash")?;
    let catalogue_value = parse_unique_json(row.canonical_catalogue_json.as_bytes())?;
    if canonical_json_string(&catalogue_value)? != row.canonical_catalogue_json
        || canonical_hash(&catalogue_value)? != row.catalogue_hash
    {
        return Err(KernelError::IntegrityFailed(format!(
            "destination catalogue authority {:?} canonical bytes/hash mismatch",
            row.id
        )));
    }
    let report_value = parse_unique_json(row.compatibility_report_json.as_bytes())?;
    if canonical_json_string(&report_value)? != row.compatibility_report_json
        || canonical_hash(&report_value)? != row.compatibility_report_hash
    {
        return Err(KernelError::IntegrityFailed(format!(
            "destination catalogue authority {:?} compatibility report mismatch",
            row.id
        )));
    }
    if row.report_algorithm != REPORT_ALGORITHM {
        return Err(KernelError::IntegrityFailed(format!(
            "destination catalogue authority {:?} uses unsupported compatibility algorithm {:?}",
            row.id, row.report_algorithm
        )));
    }
    let stored_report: CompatibilityReportView = serde_json::from_value(report_value.clone())
        .map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "destination catalogue authority {:?} compatibility report is invalid: {error}",
                row.id
            ))
        })?;
    if stored_report.algorithm != row.report_algorithm {
        return Err(KernelError::IntegrityFailed(format!(
            "destination catalogue authority {:?} compatibility algorithm links disagree",
            row.id
        )));
    }
    let expected_report = if row.origin == "EDMX" {
        let (Some(snapshot_id), Some(snapshot_sha256), Some(catalogue_path), Some(file_hash)) = (
            row.snapshot_id.as_deref(),
            row.snapshot_sha256.as_deref(),
            row.catalogue_path.as_deref(),
            row.catalogue_file_sha256.as_deref(),
        ) else {
            return Err(KernelError::IntegrityFailed(
                "EDMX destination catalogue lacks exact snapshot/file identity".to_owned(),
            ));
        };
        let registered_snapshot: String = connection.query_row(
            "SELECT sha256 FROM metadata_snapshots WHERE id = ?1",
            [snapshot_id],
            |record| record.get(0),
        )?;
        if registered_snapshot != snapshot_sha256 {
            return Err(KernelError::IntegrityFailed(
                "destination catalogue snapshot differs from metadata authority".to_owned(),
            ));
        }
        let parser_exists: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM parser_runs
                 WHERE snapshot_id = ?1 AND catalogue_path = ?2
                   AND catalogue_sha256 = ?3
             )",
            params![snapshot_id, catalogue_path, file_hash],
            |record| record.get(0),
        )?;
        if !parser_exists {
            return Err(KernelError::IntegrityFailed(
                "destination catalogue lacks exact parser-run evidence".to_owned(),
            ));
        }
        let artifact: TechnicalCatalogueArtifact = serde_json::from_value(catalogue_value.clone())?;
        artifact.verify_contract()?;
        if artifact.catalogue.snapshot.sha256 != snapshot_sha256
            || artifact.evidence.sha256 != row.catalogue_payload_sha256
        {
            return Err(KernelError::IntegrityFailed(
                "destination catalogue payload differs from exact snapshot/parser evidence"
                    .to_owned(),
            ));
        }
        let (immutable_path, byte_size, registered_hash): (String, i64, String) = connection
            .query_row(
                "SELECT immutable_path, byte_size, sha256
                 FROM metadata_snapshots WHERE id = ?1",
                [snapshot_id],
                |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?)),
            )?;
        let workspace = workspace_root_from_connection(connection)?;
        let snapshot_path = checked_workspace_path(&workspace, &immutable_path)?;
        let bytes = crate::mapping::read_regular_bounded_mapping_input(
            &snapshot_path,
            usize::try_from(MAX_EDMX_BYTES).unwrap_or(usize::MAX),
            "destination compatibility replay snapshot",
        )?;
        if i64::try_from(bytes.len()).ok() != Some(byte_size) {
            return Err(KernelError::IntegrityFailed(
                "destination compatibility replay snapshot size differs from immutable evidence"
                    .to_owned(),
            ));
        }
        if sha256_hex(&bytes) != registered_hash || registered_hash != snapshot_sha256 {
            return Err(KernelError::IntegrityFailed(
                "destination compatibility replay snapshot hash differs from immutable evidence"
                    .to_owned(),
            ));
        }
        compatibility_report_from_edmx(&bytes, &artifact)?
    } else if row.origin != "MANUAL" || row.catalogue_payload_sha256 != row.catalogue_hash {
        return Err(KernelError::IntegrityFailed(
            "manual destination catalogue contract is invalid".to_owned(),
        ));
    } else {
        compatibility_report_from_manual(&catalogue_value)?
    };
    let expected_report_value = serde_json::to_value(&expected_report)?;
    if canonical_json_string(&expected_report_value)? != row.compatibility_report_json
        || canonical_hash(&expected_report_value)? != row.compatibility_report_hash
        || expected_report != stored_report
        || expected_report.odata_generation != row.odata_generation
    {
        return Err(KernelError::IntegrityFailed(format!(
            "destination catalogue authority {:?} compatibility report does not recompute from immutable technical evidence",
            row.id
        )));
    }
    Ok(())
}

fn workspace_root_from_connection(connection: &Connection) -> Result<PathBuf> {
    let database_path: String = connection.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    let database_path = PathBuf::from(database_path);
    if database_path.file_name().and_then(|name| name.to_str()) != Some(DATABASE_FILENAME) {
        return Err(KernelError::IntegrityFailed(
            "destination integrity replay cannot resolve the exact workspace database".to_owned(),
        ));
    }
    database_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            KernelError::IntegrityFailed(
                "destination integrity replay database has no workspace parent".to_owned(),
            )
        })
}

struct ArtifactProvenance {
    system: Option<String>,
    client_or_tenant: Option<String>,
    release: Option<String>,
    manual_actor: Option<String>,
    manual_reason: Option<String>,
}

fn load_artifact_provenance(
    connection: &Connection,
    authority: &CatalogueAuthorityRow,
) -> Result<ArtifactProvenance> {
    if authority.origin == "MANUAL" {
        let catalogue = parse_unique_json(authority.canonical_catalogue_json.as_bytes())?;
        if required_json_string(&catalogue, "origin")? != "MANUAL" {
            return Err(KernelError::IntegrityFailed(
                "manual destination provenance has a conflicting origin".to_owned(),
            ));
        }
        let actor = required_json_string(&catalogue, "actor")?.to_owned();
        let reason = required_json_string(&catalogue, "reason")?.to_owned();
        validate_name(&actor, "manual destination actor", 256)?;
        validate_name(&reason, "manual destination reason", MAX_TEXT_BYTES)?;
        return Ok(ArtifactProvenance {
            system: None,
            client_or_tenant: None,
            release: None,
            manual_actor: Some(actor),
            manual_reason: Some(reason),
        });
    }
    let snapshot_id = authority.snapshot_id.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed("EDMX destination provenance has no snapshot ID".to_owned())
    })?;
    let snapshot_sha256 = authority.snapshot_sha256.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed("EDMX destination provenance has no snapshot hash".to_owned())
    })?;
    let mut statement = connection.prepare(
        "SELECT attempt_id, system_context, client_context, release_context
         FROM destination_artifact_attempts
         WHERE catalogue_authority_id = ?1 AND state = 'READY'
           AND snapshot_id = ?2 AND source_sha256 = ?3
           AND artifact_label = ?4 AND completed_at = ?5
         ORDER BY attempt_id",
    )?;
    let rows = statement
        .query_map(
            params![
                authority.id,
                snapshot_id,
                snapshot_sha256,
                authority.artifact_label,
                authority.created_at,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let [(attempt_id, system, client_or_tenant, release)] = rows.as_slice() else {
        return Err(KernelError::IntegrityFailed(
            "EDMX destination authority lacks one exact READY provenance attempt".to_owned(),
        ));
    };
    let mut checked = 0_usize;
    verify_artifact_attempt(connection, attempt_id, &mut checked)?;
    validate_optional_text(system.as_deref(), "system provenance")?;
    validate_optional_text(client_or_tenant.as_deref(), "client or tenant provenance")?;
    validate_optional_text(release.as_deref(), "release provenance")?;
    Ok(ArtifactProvenance {
        system: system.clone(),
        client_or_tenant: client_or_tenant.clone(),
        release: release.clone(),
        manual_actor: None,
        manual_reason: None,
    })
}

fn load_draft_head(connection: &Connection, draft_id: &str) -> Result<DraftHeadRow> {
    let row = connection
        .query_row(
            "SELECT draft_id, catalogue_authority_id, catalogue_hash,
                    generation, state, head_revision_hash,
                    frozen_profile_id, frozen_profile_version,
                    frozen_profile_hash, updated_at
             FROM destination_profile_draft_heads WHERE draft_id = ?1",
            [draft_id],
            |row| {
                let generation: i64 = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    generation,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| KernelError::NotFound(format!("destination draft {draft_id}")))?;
    let generation = u64::try_from(row.3).map_err(|_| {
        KernelError::IntegrityFailed("destination draft generation is negative".to_owned())
    })?;
    let head = DraftHeadRow {
        draft_id: row.0,
        catalogue_authority_id: row.1,
        catalogue_hash: row.2,
        generation,
        state: row.4,
        head_revision_hash: row.5,
        frozen_profile_id: row.6,
        frozen_profile_version: row.7,
        frozen_profile_hash: row.8,
        updated_at: row.9,
    };
    verify_destination_draft_chain(connection, &head)?;
    Ok(head)
}

fn verify_destination_draft_chain(connection: &Connection, head: &DraftHeadRow) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT generation, revision_hash, parent_revision_hash,
                canonical_draft_json
         FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 ORDER BY generation",
    )?;
    let revisions = statement
        .query_map([&head.draft_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if revisions.len() != usize::try_from(head.generation).unwrap_or(usize::MAX) {
        return Err(KernelError::IntegrityFailed(format!(
            "destination draft {:?} revision chain is not contiguous through its head",
            head.draft_id
        )));
    }
    let mut prior_hash: Option<String> = None;
    for (index, (stored_generation, revision_hash, parent_hash, canonical_json)) in
        revisions.iter().enumerate()
    {
        let generation = index.saturating_add(1);
        if *stored_generation != i64::try_from(generation).unwrap_or(i64::MAX)
            || parent_hash != &prior_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "destination draft {:?} revision {generation} has a broken generation/parent link",
                head.draft_id
            )));
        }
        let value = parse_unique_json(canonical_json.as_bytes())?;
        if canonical_json_string(&value)? != *canonical_json
            || canonical_hash(&value)? != *revision_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "destination draft {:?} revision {generation} canonical bytes/hash mismatch",
                head.draft_id
            )));
        }
        let snapshot: DestinationDraftSnapshot = serde_json::from_value(value)?;
        validate_draft_snapshot(&snapshot)?;
        if generation > 1 {
            let attempts: i64 = connection.query_row(
                "SELECT COUNT(*) FROM destination_workshop_attempts
                 WHERE command_kind = 'SAVE_DRAFT'
                   AND result_reference = ?1 AND result_hash = ?2",
                params![head.draft_id, revision_hash],
                |row| row.get(0),
            )?;
            if attempts != 1 {
                return Err(KernelError::IntegrityFailed(format!(
                    "destination draft {:?} revision {generation} lacks one exact save attempt",
                    head.draft_id
                )));
            }
        }
        prior_hash = Some(revision_hash.clone());
    }
    if prior_hash.as_deref() != Some(head.head_revision_hash.as_str()) {
        return Err(KernelError::IntegrityFailed(format!(
            "destination draft {:?} head hash is not the latest immutable revision",
            head.draft_id
        )));
    }
    let origins: i64 = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM destination_workshop_attempts
              WHERE command_kind = 'START_DRAFT'
                AND result_reference = ?1 AND result_hash = ?2)
           + (SELECT COUNT(*) FROM destination_rebases
              WHERE result_draft_id = ?1 AND result_revision_hash = ?2)",
        params![head.draft_id, revisions[0].1],
        |row| row.get(0),
    )?;
    if origins != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "destination draft {:?} lacks one exact immutable creation authority",
            head.draft_id
        )));
    }
    Ok(())
}

fn load_draft_snapshot(
    connection: &Connection,
    draft_id: &str,
    generation: u64,
    expected_hash: &str,
) -> Result<DestinationDraftSnapshot> {
    let (revision_hash, parent_hash, canonical_json): (String, Option<String>, String) = connection
        .query_row(
            "SELECT revision_hash, parent_revision_hash, canonical_draft_json
             FROM destination_profile_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![draft_id, i64::try_from(generation).unwrap_or(i64::MAX)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if revision_hash != expected_hash {
        return Err(KernelError::IntegrityFailed(
            "destination draft head differs from its exact revision".to_owned(),
        ));
    }
    let value = parse_unique_json(canonical_json.as_bytes())?;
    if canonical_json_string(&value)? != canonical_json || canonical_hash(&value)? != revision_hash
    {
        return Err(KernelError::IntegrityFailed(
            "destination draft revision canonical hash mismatch".to_owned(),
        ));
    }
    if generation == 1 && parent_hash.is_some() || generation > 1 && parent_hash.is_none() {
        return Err(KernelError::IntegrityFailed(
            "destination draft parent chain is malformed".to_owned(),
        ));
    }
    if generation > 1 {
        let previous: String = connection.query_row(
            "SELECT revision_hash FROM destination_profile_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![
                draft_id,
                i64::try_from(generation.saturating_sub(1)).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )?;
        if parent_hash.as_deref() != Some(previous.as_str()) {
            return Err(KernelError::IntegrityFailed(
                "destination draft revision parent hash mismatch".to_owned(),
            ));
        }
    }
    let snapshot: DestinationDraftSnapshot = serde_json::from_value(value)?;
    validate_draft_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn latest_draft_for_authority(
    connection: &Connection,
    catalogue_authority_id: &str,
) -> Result<Option<(DraftHeadRow, DestinationDraftSnapshot)>> {
    let draft_id: Option<String> = connection
        .query_row(
            "SELECT draft_id FROM destination_profile_draft_heads
             WHERE catalogue_authority_id = ?1
             ORDER BY updated_at DESC, rowid DESC LIMIT 1",
            [catalogue_authority_id],
            |row| row.get(0),
        )
        .optional()?;
    draft_id
        .map(|draft_id| {
            let head = load_draft_head(connection, &draft_id)?;
            let snapshot = load_draft_snapshot(
                connection,
                &head.draft_id,
                head.generation,
                &head.head_revision_hash,
            )?;
            Ok((head, snapshot))
        })
        .transpose()
}

fn require_editable_draft_head(
    head: &DraftHeadRow,
    expected_generation: u64,
    expected_hash: &str,
) -> Result<()> {
    if head.state != "EDITABLE"
        || head.generation != expected_generation
        || head.head_revision_hash != expected_hash
    {
        return Err(KernelError::Conflict(
            "destination draft command used a stale or frozen draft head".to_owned(),
        ));
    }
    Ok(())
}

fn require_rebase_source_head(
    head: &DraftHeadRow,
    expected_generation: u64,
    expected_hash: &str,
) -> Result<()> {
    if !matches!(head.state.as_str(), "EDITABLE" | "FROZEN")
        || head.generation != expected_generation
        || head.head_revision_hash != expected_hash
    {
        return Err(KernelError::Conflict(
            "destination rebase used a stale or unverified source draft head".to_owned(),
        ));
    }
    Ok(())
}

fn active_engagement_identity(connection: &Connection) -> Result<(String, u64)> {
    let (engagement_id, generation): (String, i64) = connection
        .query_row(
            "SELECT engagement_id, generation FROM engagement_active_context",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::Conflict("destination workshop requires an active engagement".to_owned())
        })?;
    Ok((
        engagement_id,
        u64::try_from(generation).map_err(|_| {
            KernelError::IntegrityFailed("engagement generation is negative".to_owned())
        })?,
    ))
}

fn load_reviewed_candidate(
    connection: &Connection,
    workspace: &Path,
    engagement_id: &str,
) -> Result<Option<FrozenDestinationProfileView>> {
    let draft_id: Option<String> = connection
        .query_row(
            "SELECT draft_id
             FROM destination_profile_draft_heads
             WHERE engagement_id = ?1 AND state = 'FROZEN'
             ORDER BY updated_at DESC, rowid DESC LIMIT 1",
            [engagement_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(draft_id) = draft_id else {
        return Ok(None);
    };
    let head = load_draft_head(connection, &draft_id)?;
    if head.state != "FROZEN" {
        return Err(KernelError::IntegrityFailed(
            "reviewed destination candidate is not an exact frozen engagement draft".to_owned(),
        ));
    }
    let profile_id = head.frozen_profile_id.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed("reviewed destination candidate has no profile ID".to_owned())
    })?;
    let profile_version = head.frozen_profile_version.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed(
            "reviewed destination candidate has no profile version".to_owned(),
        )
    })?;
    let profile_hash = head.frozen_profile_hash.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed(
            "reviewed destination candidate has no profile hash".to_owned(),
        )
    })?;
    let snapshot = load_draft_snapshot(
        connection,
        &head.draft_id,
        head.generation,
        &head.head_revision_hash,
    )?;
    let authority = load_catalogue_authority(connection, &head.catalogue_authority_id)?;
    let profile = inspect_profile(workspace, profile_id, profile_version)?;
    if profile.profile_hash != profile_hash {
        return Err(KernelError::IntegrityFailed(
            "reviewed destination candidate differs from registry profile authority".to_owned(),
        ));
    }
    let (origin_binding_id, origin_binding_hash): (String, String) = connection.query_row(
        "SELECT origin_binding_id, origin_binding_hash
         FROM destination_profile_origin_bindings
         WHERE engagement_id = ?1 AND profile_id = ?2
           AND profile_version = ?3 AND profile_hash = ?4
           AND draft_id = ?5 AND draft_generation = ?6
           AND draft_revision_hash = ?7
           AND catalogue_authority_id = ?8 AND catalogue_hash = ?9",
        params![
            engagement_id,
            profile_id,
            profile_version,
            profile_hash,
            head.draft_id,
            i64::try_from(head.generation).unwrap_or(i64::MAX),
            head.head_revision_hash,
            head.catalogue_authority_id,
            head.catalogue_hash,
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    verify_origin_binding(
        connection,
        &origin_binding_id,
        &origin_binding_hash,
        profile_id,
        profile_version,
        profile_hash,
    )?;
    let rebuilt_origin =
        build_origin_binding_value(engagement_id, &profile, &head, &authority, &snapshot)?;
    if canonical_hash(&rebuilt_origin)? != origin_binding_hash {
        return Err(KernelError::IntegrityFailed(
            "reviewed destination candidate origin does not replay exactly".to_owned(),
        ));
    }
    let mut statement = connection.prepare(
        "SELECT result_generation
         FROM destination_workshop_attempts
         WHERE engagement_id = ?1 AND command_kind = 'FREEZE_DRAFT'
           AND result_reference = ?2 AND result_hash = ?3
         ORDER BY attempt_id",
    )?;
    let generations = statement
        .query_map(params![engagement_id, head.draft_id, profile_hash], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let [result_generation] = generations.as_slice() else {
        return Err(KernelError::IntegrityFailed(
            "reviewed destination candidate lacks one exact freeze attempt".to_owned(),
        ));
    };
    let engagement_generation = u64::try_from(*result_generation).map_err(|_| {
        KernelError::IntegrityFailed(
            "reviewed destination candidate freeze generation is negative".to_owned(),
        )
    })?;
    Ok(Some(FrozenDestinationProfileView {
        schema_version: DESTINATION_REVIEWED_PROFILE_SCHEMA,
        engagement_generation,
        profile,
        origin_binding_id,
        origin_binding_hash,
        draft_id: head.draft_id,
        draft_generation: head.generation,
        draft_hash: head.head_revision_hash,
    }))
}

fn load_active_destination(
    connection: &Connection,
    _workspace: &Path,
) -> Result<Option<ActiveDestinationView>> {
    let row: (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT profile_stage, destination_origin,
                    destination_authority_id, destination_origin_hash,
                    selected_profile_id, selected_profile_version
             FROM engagement_active_context",
        [],
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
    )?;
    if row.0 == "NOT_SELECTED" {
        if row.1.is_some()
            || row.2.is_some()
            || row.3.is_some()
            || row.4.is_some()
            || row.5.is_some()
        {
            return Err(KernelError::IntegrityFailed(
                "unselected destination retains partial active identity".to_owned(),
            ));
        }
        return Ok(None);
    }
    let origin = match row.1.as_deref() {
        Some("CONTROLLED_PACK") => DestinationOrigin::ControlledPack,
        Some("WORKSPACE_PROFILE") => DestinationOrigin::WorkspaceProfile,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "active destination origin is missing or unsupported".to_owned(),
            ));
        }
    };
    let authority_id = row.2.ok_or_else(|| {
        KernelError::IntegrityFailed("active destination authority ID is missing".to_owned())
    })?;
    let origin_hash = row.3.ok_or_else(|| {
        KernelError::IntegrityFailed("active destination origin hash is missing".to_owned())
    })?;
    let profile_id = row.4.ok_or_else(|| {
        KernelError::IntegrityFailed("active destination profile ID is missing".to_owned())
    })?;
    let profile_version = row.5.ok_or_else(|| {
        KernelError::IntegrityFailed("active destination profile version is missing".to_owned())
    })?;
    let profile_hash: String = connection.query_row(
        "SELECT selected_profile_hash FROM engagement_active_context",
        [],
        |record| record.get(0),
    )?;
    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        &profile_id,
        &profile_version,
        &profile_hash,
    )?;
    if origin == DestinationOrigin::WorkspaceProfile {
        verify_origin_binding(
            connection,
            &authority_id,
            &origin_hash,
            &profile_id,
            &profile_version,
            &profile_hash,
        )?;
        if row.0 == "APPROVED"
            && (profile.authority != ProfileAuthority::LocalMappingValidation
                || profile.approval.is_none())
        {
            return Err(KernelError::IntegrityFailed(
                "active approved workspace destination lacks exact registry approval authority"
                    .to_owned(),
            ));
        }
    } else if origin_hash != profile_hash {
        return Err(KernelError::IntegrityFailed(
            "controlled-pack destination origin hash differs from exact profile".to_owned(),
        ));
    }
    Ok(Some(ActiveDestinationView {
        origin,
        destination_authority_id: authority_id,
        destination_origin_hash: origin_hash,
        profile_id,
        profile_version,
        profile_hash,
        profile_authority: profile.authority,
        approval_id: profile
            .approval
            .as_ref()
            .map(|approval| approval.approval_id.clone()),
        approval_hash: profile
            .approval
            .as_ref()
            .map(|approval| approval.approval_hash.clone()),
    }))
}

fn verify_origin_binding(
    connection: &Connection,
    origin_binding_id: &str,
    expected_origin_hash: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<()> {
    let (
        stored_profile_id,
        stored_version,
        stored_profile_hash,
        draft_id,
        draft_generation,
        draft_revision_hash,
        catalogue_id,
        catalogue_hash,
        report_hash,
        origin_json,
        origin_hash,
        binding_engagement_id,
    ): (
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT profile_id, profile_version, profile_hash,
                draft_id, draft_generation, draft_revision_hash,
                catalogue_authority_id, catalogue_hash,
                compatibility_report_hash, canonical_origin_json,
                origin_binding_hash, engagement_id
         FROM destination_profile_origin_bindings
         WHERE origin_binding_id = ?1",
        [origin_binding_id],
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
    if stored_profile_id != profile_id
        || stored_version != profile_version
        || stored_profile_hash != profile_hash
        || origin_hash != expected_origin_hash
    {
        return Err(KernelError::IntegrityFailed(
            "destination profile origin differs from exact active profile".to_owned(),
        ));
    }
    let origin_value = parse_unique_json(origin_json.as_bytes())?;
    if canonical_json_string(&origin_value)? != origin_json
        || canonical_hash(&origin_value)? != origin_hash
    {
        return Err(KernelError::IntegrityFailed(
            "destination profile origin canonical hash mismatch".to_owned(),
        ));
    }
    let draft_generation = u64::try_from(draft_generation).map_err(|_| {
        KernelError::IntegrityFailed("destination origin draft generation is negative".to_owned())
    })?;
    let head = load_draft_head(connection, &draft_id)?;
    if head.state != "FROZEN"
        || head.generation != draft_generation
        || head.head_revision_hash != draft_revision_hash
        || head.catalogue_authority_id != catalogue_id
        || head.catalogue_hash != catalogue_hash
        || head.frozen_profile_id.as_deref() != Some(profile_id)
        || head.frozen_profile_version.as_deref() != Some(profile_version)
        || head.frozen_profile_hash.as_deref() != Some(profile_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "destination profile origin differs from frozen draft head".to_owned(),
        ));
    }
    let snapshot = load_draft_snapshot(
        connection,
        &draft_id,
        draft_generation,
        &draft_revision_hash,
    )?;
    let authority = load_catalogue_authority(connection, &catalogue_id)?;
    if authority.catalogue_hash != catalogue_hash
        || authority.compatibility_report_hash != report_hash
    {
        return Err(KernelError::IntegrityFailed(
            "destination profile origin differs from catalogue/report authority".to_owned(),
        ));
    }
    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        profile_id,
        profile_version,
        profile_hash,
    )?;
    let registered_document = crate::profile_registry::load_verified_profile_document(
        connection,
        profile_id,
        profile_version,
        profile_hash,
    )?;
    let rebuilt_document = build_profile_document(&snapshot, &authority, profile_version)?;
    if canonical_json_string(&rebuilt_document)? != canonical_json_string(&registered_document)?
        || canonical_hash(&rebuilt_document)? != profile_hash
    {
        return Err(KernelError::IntegrityFailed(
            "destination reviewed profile does not rebuild from its exact frozen draft and catalogue"
                .to_owned(),
        ));
    }
    let expected_origin = build_origin_binding_value(
        &binding_engagement_id,
        &profile,
        &head,
        &authority,
        &snapshot,
    )?;
    if canonical_json_string(&expected_origin)? != origin_json {
        return Err(KernelError::IntegrityFailed(
            "destination profile origin differs from independently replayed scope/provenance"
                .to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkshopAttemptResult {
    reference: String,
    generation: u64,
    hash: String,
}

fn load_workshop_attempt(
    connection: &Connection,
    engagement_id: &str,
    command_kind: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<WorkshopAttemptResult>> {
    let row: Option<(String, String, i64, String)> = connection
        .query_row(
            "SELECT request_hash, result_reference, result_generation, result_hash
             FROM destination_workshop_attempts
             WHERE engagement_id = ?1 AND command_kind = ?2
               AND idempotency_key = ?3",
            params![engagement_id, command_kind, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    match row {
        Some((stored_hash, _, _, _)) if stored_hash != request_hash => Err(KernelError::Conflict(
            "destination command retry reused its identity with different canonical bytes"
                .to_owned(),
        )),
        Some((_, result_reference, result_generation, result_hash)) => {
            Ok(Some(WorkshopAttemptResult {
                reference: result_reference,
                generation: u64::try_from(result_generation).map_err(|_| {
                    KernelError::IntegrityFailed(
                        "destination command result generation is negative".to_owned(),
                    )
                })?,
                hash: result_hash,
            }))
        }
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_workshop_attempt(
    transaction: &rusqlite::Transaction<'_>,
    engagement_id: &str,
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
        "INSERT INTO destination_workshop_attempts (
             attempt_id, engagement_id, command_kind, idempotency_key,
             request_hash, canonical_request_json, expected_generation,
             result_generation, result_reference, result_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            Uuid::new_v4().to_string(),
            engagement_id,
            command_kind,
            idempotency_key,
            request_hash,
            canonical_request_json,
            i64::try_from(expected_generation).unwrap_or(i64::MAX),
            i64::try_from(result_generation).unwrap_or(i64::MAX),
            result_reference,
            result_hash,
            created_at,
        ],
    )?;
    Ok(())
}

fn increment_generation(
    transaction: &rusqlite::Transaction<'_>,
    engagement_id: &str,
    expected: u64,
    now: &str,
    label: &str,
) -> Result<u64> {
    let changed = transaction.execute(
        "UPDATE engagement_active_context
         SET generation = generation + 1, updated_at = ?1
         WHERE engagement_id = ?2 AND generation = ?3",
        params![
            now,
            engagement_id,
            i64::try_from(expected).unwrap_or(i64::MAX)
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::Conflict(format!(
            "{label} lost its engagement-generation compare-and-swap"
        )));
    }
    Ok(expected.saturating_add(1))
}

fn artifact_event_hash(
    attempt_id: &str,
    state: &str,
    previous: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "schemaVersion": ARTIFACT_EVENT_SCHEMA,
        "attemptId": attempt_id,
        "state": state,
        "previousEventHash": previous,
        "occurredAt": occurred_at,
    }))
}

fn profile_field_signatures(document: &Value) -> Result<BTreeMap<(String, String), String>> {
    let entities = document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::IntegrityFailed("profile document has no entities".to_owned())
        })?;
    let rules = document
        .get("rules")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("profile document has no rules".to_owned()))?;
    let mut output = BTreeMap::new();
    for entity in entities {
        let entity_id = required_json_string(entity, "entityId")?;
        let key = entity
            .get("key")
            .and_then(Value::as_array)
            .ok_or_else(|| KernelError::IntegrityFailed("profile entity has no key".to_owned()))?;
        let fields = entity
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::IntegrityFailed("profile entity has no fields".to_owned())
            })?;
        for field in fields {
            let field_id = required_json_string(field, "fieldId")?;
            let target = format!("{entity_id}.{field_id}");
            let mut field_rules = rules
                .iter()
                .filter(|rule| {
                    let exact_target = rule.get("target").and_then(Value::as_str);
                    exact_target == Some(target.as_str())
                        || (rule.get("kind").and_then(Value::as_str) == Some("unique")
                            && exact_target
                                .and_then(|value| value.split_once('.'))
                                .is_some_and(|(rule_entity, _)| rule_entity == entity_id)
                            && rule
                                .get("parameters")
                                .and_then(|parameters| parameters.get("scopeFields"))
                                .and_then(Value::as_array)
                                .is_some_and(|scope| {
                                    scope
                                        .iter()
                                        .any(|candidate| candidate.as_str() == Some(field_id))
                                }))
                })
                .map(|rule| Ok((canonical_json_string(rule)?, rule.clone())))
                .collect::<Result<Vec<_>>>()?;
            field_rules.sort_by(|left, right| left.0.cmp(&right.0));
            let field_rules = field_rules
                .into_iter()
                .map(|(_, rule)| rule)
                .collect::<Vec<_>>();
            let signature = canonical_hash(&json!({
                "entityId": entity_id,
                "entityRoot": entity.get("root"),
                "technicalEntity": entity.get("technicalEntity"),
                "field": field,
                "entityKey": key,
                "fieldRules": field_rules,
            }))?;
            output.insert((entity_id.to_owned(), field_id.to_owned()), signature);
        }
    }
    Ok(output)
}

fn profile_field_compatibility(
    source: &Value,
    target: &Value,
) -> Result<(HashSet<(String, String)>, HashSet<(String, String)>)> {
    let source_signatures = profile_field_signatures(source)?;
    let target_signatures = profile_field_signatures(target)?;
    let target_fields = target_signatures.keys().cloned().collect::<HashSet<_>>();
    let compatible = target_signatures
        .iter()
        .filter(|&(identity, target_signature)| {
            source_signatures
                .get(identity)
                .is_some_and(|source_signature| source_signature == target_signature)
        })
        .map(|(identity, _)| identity.clone())
        .collect();
    Ok((target_fields, compatible))
}

fn profile_entity_signatures(document: &Value) -> Result<HashMap<String, String>> {
    document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("profile document has no entities".to_owned()))?
        .iter()
        .map(|entity| {
            let entity_id = required_json_string(entity, "entityId")?.to_owned();
            let signature = canonical_hash(&json!({
                "entityId": entity_id,
                "root": entity.get("root"),
                "technicalEntity": entity.get("technicalEntity"),
            }))?;
            Ok((entity_id, signature))
        })
        .collect()
}

type RelationshipIdentity = (String, String, String, String, Vec<(String, String)>);

fn profile_relationship_identities(document: &Value) -> Result<HashSet<RelationshipIdentity>> {
    let relationships = document
        .get("relationships")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::IntegrityFailed("profile document has no relationships".to_owned())
        })?;
    relationships
        .iter()
        .map(|relationship| {
            let pairs = relationship
                .get("keyPairs")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    KernelError::IntegrityFailed("profile relationship has no key pairs".to_owned())
                })?
                .iter()
                .map(|pair| {
                    Ok((
                        required_json_string(pair, "fromFieldId")?.to_owned(),
                        required_json_string(pair, "toFieldId")?.to_owned(),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((
                required_json_string(relationship, "relationshipId")?.to_owned(),
                required_json_string(relationship, "from")?.to_owned(),
                required_json_string(relationship, "to")?.to_owned(),
                required_json_string(relationship, "cardinality")?.to_owned(),
                pairs,
            ))
        })
        .collect()
}

fn eligible_carry_forward_source(
    connection: &Connection,
    engagement_id: &str,
) -> Result<Option<EligibleCarryForwardSourceView>> {
    let active: (
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
    ) = connection.query_row(
        "SELECT profile_stage, mapping_stage, destination_origin, active_job_id,
                selected_profile_id, selected_profile_version,
                selected_profile_hash, active_mapping_id,
                active_mapping_version, active_mapping_hash
         FROM engagement_active_context WHERE engagement_id = ?1",
        [engagement_id],
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
            ))
        },
    )?;
    if active.1 != "VALIDATED" {
        return Ok(None);
    }
    if active.0 != "APPROVED" {
        return Err(KernelError::IntegrityFailed(
            "validated active mapping has no approved destination profile".to_owned(),
        ));
    }
    let destination_origin = active.2.as_deref();
    let active_job_id = active.3.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no active source job".to_owned())
    })?;
    let profile_id = active.4.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no profile ID".to_owned())
    })?;
    let profile_version = active.5.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no profile version".to_owned())
    })?;
    let profile_hash = active.6.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no profile hash".to_owned())
    })?;
    let mapping_id = active.7.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no mapping ID".to_owned())
    })?;
    let mapping_version = active.8.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no mapping version".to_owned())
    })?;
    let mapping_hash = active.9.ok_or_else(|| {
        KernelError::IntegrityFailed("validated active mapping has no mapping hash".to_owned())
    })?;
    let (
        registered_profile_id,
        registered_profile_version,
        registered_profile_hash,
        validated_source_hash,
        validated_job_id,
    ): (String, String, String, String, String) = connection.query_row(
        "SELECT profile_id, profile_version, profile_hash,
                source_hash, validated_job_id
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
            ))
        },
    )?;
    if registered_profile_id != profile_id
        || registered_profile_version != profile_version
        || registered_profile_hash != profile_hash
    {
        return Err(KernelError::IntegrityFailed(
            "active mapping differs from the exact active destination profile".to_owned(),
        ));
    }
    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        &profile_id,
        &profile_version,
        &profile_hash,
    )?;
    if profile.authority != ProfileAuthority::LocalMappingValidation || profile.approval.is_none() {
        return Err(KernelError::IntegrityFailed(
            "active mapping source profile lacks exact registry approval".to_owned(),
        ));
    }
    let active_source_hash: String = connection.query_row(
        "SELECT source_hash FROM jobs WHERE id = ?1 AND state = 'COMPLETE'",
        [&active_job_id],
        |row| row.get(0),
    )?;
    let approval = crate::engagement::verified_operator_mapping_approval_identity(
        connection,
        engagement_id,
        &mapping_id,
        &mapping_version,
        &mapping_hash,
    )?;
    let Some((mapping_approval_id, mapping_approval_hash)) = approval else {
        if destination_origin == Some(DestinationOrigin::WorkspaceProfile.as_str()) {
            return Err(KernelError::IntegrityFailed(
                "active workspace mapping has no exact attributable approval authority".to_owned(),
            ));
        }
        return Ok(None);
    };
    let view = EligibleCarryForwardSourceView {
        mapping_id,
        mapping_version,
        mapping_hash,
        mapping_approval_id,
        mapping_approval_hash,
        active_job_id,
        active_source_hash,
        validated_job_id,
        validated_source_hash,
        profile_id,
        profile_version,
        profile_hash,
    };
    verify_carry_forward_source_context(connection, engagement_id, &view)?;
    Ok(Some(view))
}

fn verify_carry_forward_source_context(
    connection: &Connection,
    engagement_id: &str,
    source: &EligibleCarryForwardSourceView,
) -> Result<()> {
    let mapping: (String, String, String, String, String) = connection.query_row(
        "SELECT profile_id, profile_version, profile_hash,
                validated_job_id, source_hash
         FROM mapping_versions
         WHERE mapping_id = ?1 AND mapping_version = ?2 AND mapping_hash = ?3",
        params![
            source.mapping_id,
            source.mapping_version,
            source.mapping_hash
        ],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let active_source_hash: String = connection.query_row(
        "SELECT source_hash FROM jobs WHERE id = ?1 AND state = 'COMPLETE'",
        [&source.active_job_id],
        |row| row.get(0),
    )?;
    if mapping.0 != source.profile_id
        || mapping.1 != source.profile_version
        || mapping.2 != source.profile_hash
        || mapping.3 != source.validated_job_id
        || mapping.4 != source.validated_source_hash
        || active_source_hash != source.active_source_hash
    {
        return Err(KernelError::IntegrityFailed(
            "mapping carry-forward source context differs from immutable mapping/job/profile evidence"
                .to_owned(),
        ));
    }
    crate::engagement::verify_operator_mapping_approval_identity(
        connection,
        engagement_id,
        &source.mapping_id,
        &source.mapping_version,
        &source.mapping_hash,
        &source.mapping_approval_id,
        &source.mapping_approval_hash,
        false,
    )?;
    if source.active_job_id == source.validated_job_id
        && source.active_source_hash == source.validated_source_hash
    {
        return Ok(());
    }
    let mut statement = connection.prepare(
        "SELECT source_reuse_id, source_reuse_hash
         FROM validation_runs
         WHERE job_id = ?1 AND source_hash = ?2
           AND profile_id = ?3 AND profile_version = ?4 AND profile_hash = ?5
           AND mapping_id = ?6 AND mapping_version = ?7 AND mapping_hash = ?8
           AND state = 'COMPLETE'
           AND source_reuse_id IS NOT NULL AND source_reuse_hash IS NOT NULL
         ORDER BY id",
    )?;
    let reuse_rows = statement
        .query_map(
            params![
                source.active_job_id,
                source.active_source_hash,
                source.profile_id,
                source.profile_version,
                source.profile_hash,
                source.mapping_id,
                source.mapping_version,
                source.mapping_hash,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if !reuse_rows.iter().any(|(reuse_id, reuse_hash)| {
        crate::validation::verify_mapping_source_reuse_authority(
            connection,
            reuse_id,
            reuse_hash,
            &source.active_job_id,
            &source.active_source_hash,
            &source.profile_id,
            &source.profile_version,
            &source.profile_hash,
            &source.mapping_id,
            &source.mapping_version,
            &source.mapping_hash,
        )
        .is_ok()
    }) {
        return Err(KernelError::IntegrityFailed(
            "mapping carry-forward active source lacks exact immutable mapping-reuse authority"
                .to_owned(),
        ));
    }
    Ok(())
}

struct CarryForwardComputation {
    source_entity_assignments: Vec<crate::mapping::SourceEntityAssignment>,
    cloned_bindings: Vec<crate::mapping::MappingBinding>,
    cloned_relationships: Vec<crate::mapping::MappingRelationshipBinding>,
    unresolved_targets: Vec<(String, String)>,
    unresolved_relationships: Vec<UnresolvedCarryForwardRelationshipView>,
}

fn carry_forward_source_from_result(result: &Value) -> Result<EligibleCarryForwardSourceView> {
    let source = result.get("sourceMapping").ok_or_else(|| {
        KernelError::IntegrityFailed(
            "mapping carry-forward result has no source authority".to_owned(),
        )
    })?;
    Ok(EligibleCarryForwardSourceView {
        mapping_id: required_json_string(source, "mappingId")?.to_owned(),
        mapping_version: required_json_string(source, "mappingVersion")?.to_owned(),
        mapping_hash: required_json_string(source, "mappingHash")?.to_owned(),
        mapping_approval_id: required_json_string(source, "mappingApprovalId")?.to_owned(),
        mapping_approval_hash: required_json_string(source, "mappingApprovalHash")?.to_owned(),
        active_job_id: required_json_string(source, "activeJobId")?.to_owned(),
        active_source_hash: required_json_string(source, "activeSourceHash")?.to_owned(),
        validated_job_id: required_json_string(source, "validatedJobId")?.to_owned(),
        validated_source_hash: required_json_string(source, "validatedSourceHash")?.to_owned(),
        profile_id: required_json_string(source, "profileId")?.to_owned(),
        profile_version: required_json_string(source, "profileVersion")?.to_owned(),
        profile_hash: required_json_string(source, "profileHash")?.to_owned(),
    })
}

fn compute_carry_forward(
    source_mapping: &crate::mapping::MappingDocument,
    source_profile: &Value,
    target_profile: &Value,
) -> Result<CarryForwardComputation> {
    use crate::mapping::MappingKind;

    let (target_fields, compatible_target_fields) =
        profile_field_compatibility(source_profile, target_profile)?;
    let source_entity_signatures = profile_entity_signatures(source_profile)?;
    let target_entity_signatures = profile_entity_signatures(target_profile)?;
    let compatible_entities = target_entity_signatures
        .iter()
        .filter(|(entity_id, signature)| {
            source_entity_signatures.get(*entity_id) == Some(*signature)
        })
        .map(|(entity_id, _)| entity_id.clone())
        .collect::<HashSet<_>>();
    let mut source_entity_assignments = source_mapping
        .source_entity_assignments
        .iter()
        .filter(|assignment| compatible_entities.contains(&assignment.entity_id))
        .cloned()
        .collect::<Vec<_>>();
    source_entity_assignments.sort_by(|left, right| {
        (left.entity_id.as_str(), left.source_file.as_str())
            .cmp(&(right.entity_id.as_str(), right.source_file.as_str()))
    });
    let mut cloned_bindings = source_mapping
        .bindings
        .iter()
        .filter(|binding| {
            let entity = binding
                .target_entity_id
                .as_deref()
                .unwrap_or(&source_mapping.root_entity_id);
            compatible_target_fields.contains(&(entity.to_owned(), binding.target_field_id.clone()))
                && binding.kind != MappingKind::Lookup
        })
        .cloned()
        .collect::<Vec<_>>();
    cloned_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    let cloned_targets = cloned_bindings
        .iter()
        .map(|binding| {
            (
                binding
                    .target_entity_id
                    .clone()
                    .unwrap_or_else(|| source_mapping.root_entity_id.clone()),
                binding.target_field_id.clone(),
            )
        })
        .collect::<HashSet<_>>();
    let mut unresolved_targets = target_fields
        .difference(&cloned_targets)
        .cloned()
        .collect::<Vec<_>>();
    unresolved_targets.sort();

    let target_relationships = profile_relationship_identities(target_profile)?;
    let source_relationship_ids = source_mapping
        .relationship_bindings
        .iter()
        .map(|relationship| relationship.relationship_id.as_str())
        .collect::<HashSet<_>>();
    let direct_targets = cloned_bindings
        .iter()
        .filter(|binding| binding.kind == MappingKind::Direct)
        .map(|binding| {
            (
                binding
                    .target_entity_id
                    .clone()
                    .unwrap_or_else(|| source_mapping.root_entity_id.clone()),
                binding.target_field_id.clone(),
            )
        })
        .collect::<HashSet<_>>();
    let mut cloned_relationships = Vec::new();
    let mut unresolved_relationships = Vec::new();
    let mut ordered_target_relationships = target_relationships.into_iter().collect::<Vec<_>>();
    ordered_target_relationships.sort();
    for identity in ordered_target_relationships {
        let exact = source_mapping
            .relationship_bindings
            .iter()
            .find(|relationship| {
                identity
                    == (
                        relationship.relationship_id.clone(),
                        relationship.from_entity_id.clone(),
                        relationship.to_entity_id.clone(),
                        relationship.cardinality.clone(),
                        relationship
                            .key_pairs
                            .iter()
                            .map(|pair| (pair.from_field_id.clone(), pair.to_field_id.clone()))
                            .collect::<Vec<_>>(),
                    )
            });
        let keys_resolved = identity.4.iter().all(|(from_field, to_field)| {
            direct_targets.contains(&(identity.1.clone(), from_field.clone()))
                && direct_targets.contains(&(identity.2.clone(), to_field.clone()))
        });
        if let Some(relationship) = exact.filter(|_| keys_resolved) {
            cloned_relationships.push(relationship.clone());
        } else {
            let reason = if exact.is_some() {
                "RELATIONSHIP_KEY_BINDING_UNRESOLVED"
            } else if source_relationship_ids.contains(identity.0.as_str()) {
                "RELATIONSHIP_CHANGED"
            } else {
                "RELATIONSHIP_NOT_PRESENT"
            };
            unresolved_relationships.push(UnresolvedCarryForwardRelationshipView {
                relationship_id: identity.0,
                reason: reason.to_owned(),
            });
        }
    }
    cloned_relationships.sort_by(|left, right| left.relationship_id.cmp(&right.relationship_id));
    unresolved_relationships.sort_by(|left, right| {
        (left.relationship_id.as_str(), left.reason.as_str())
            .cmp(&(right.relationship_id.as_str(), right.reason.as_str()))
    });
    Ok(CarryForwardComputation {
        source_entity_assignments,
        cloned_bindings,
        cloned_relationships,
        unresolved_targets,
        unresolved_relationships,
    })
}

fn required_json_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        KernelError::IntegrityFailed(format!("canonical JSON lacks string field {key:?}"))
    })
}

fn profile_hash_of_draft(
    connection: &Connection,
    draft_id: &str,
    generation: u64,
) -> Result<String> {
    connection
        .query_row(
            "SELECT revision_hash FROM destination_profile_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
            params![draft_id, i64::try_from(generation).unwrap_or(i64::MAX)],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn canonical_command<T: Serialize>(command: &T) -> Result<String> {
    let value = serde_json::to_value(command)?;
    let canonical = canonical_json_string(&value)?;
    if canonical.len() > MAX_COMMAND_BYTES {
        return Err(KernelError::PolicyInvalid(
            "destination command exceeds its canonical byte bound".to_owned(),
        ));
    }
    Ok(canonical)
}

fn require_schema(actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        return Err(KernelError::PolicyInvalid(format!(
            "unsupported destination command schema {actual:?}; expected {expected:?}"
        )));
    }
    Ok(())
}

fn validate_idempotency(value: &str) -> Result<()> {
    if !(16..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(KernelError::PolicyInvalid(
            "destination retry identity must be a bounded ASCII token".to_owned(),
        ));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be a bounded ASCII identifier"
        )));
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> Result<()> {
    if value.len() < 3
        || value.len() > 128
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'-'))
        })
    {
        return Err(KernelError::PolicyInvalid(
            "destination profile ID must use the canonical lowercase dotted identifier form"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    let components = value.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || value.len() > 32
        || components
            .iter()
            .any(|component| component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(KernelError::PolicyInvalid(
            "destination profile/mapping version must be numeric major.minor.patch".to_owned(),
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
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be lowercase SHA-256"
        )));
    }
    Ok(())
}

fn validate_name(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be nonblank, control-free, and at most {maximum} bytes"
        )));
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, label: &str) -> Result<()> {
    if let Some(value) = value {
        validate_name(value, label, 512)?;
    }
    Ok(())
}

fn validate_technical_identity(value: &str, label: &str) -> Result<()> {
    if value.len() < 3
        || value.len() > 768
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be a bounded fully-qualified technical identity"
        )));
    }
    Ok(())
}

fn validate_logical_type(value: &str) -> Result<()> {
    if matches!(
        value,
        "text"
            | "code"
            | "integer"
            | "decimal"
            | "boolean"
            | "date"
            | "datetime"
            | "identifier"
            | "country_code"
            | "currency"
            | "email"
            | "phone"
    ) {
        Ok(())
    } else {
        Err(KernelError::ProfileSemantic(format!(
            "unsupported destination logical type {value:?}"
        )))
    }
}

fn validate_manual_entities(entities: &[ManualEntityDefinition]) -> Result<()> {
    if entities.is_empty() || entities.len() > MAX_ENTITIES {
        return Err(KernelError::PolicyInvalid(format!(
            "manual destination must define 1..={MAX_ENTITIES} entities"
        )));
    }
    let mut technical_entities = HashSet::new();
    let mut fields = 0_usize;
    for entity in entities {
        validate_identifier(&entity.namespace, "manual namespace")?;
        validate_identifier(&entity.name, "manual entity name")?;
        let technical = format!("{}.{}", entity.namespace, entity.name);
        if !technical_entities.insert(technical) || entity.properties.is_empty() {
            return Err(KernelError::PolicyInvalid(
                "manual destination entities must be unique and nonempty".to_owned(),
            ));
        }
        let mut property_names = HashSet::new();
        for property in &entity.properties {
            fields = fields.saturating_add(1);
            validate_identifier(&property.name, "manual property name")?;
            validate_name(&property.edm_type, "manual technical type", 256)?;
            if !property_names.insert(property.name.as_str())
                || property.max_length == Some(0)
                || property.max_length.is_some_and(|value| value > 1_000_000)
            {
                return Err(KernelError::PolicyInvalid(
                    "manual destination property is repeated or outside its bounds".to_owned(),
                ));
            }
        }
    }
    if fields > MAX_FIELDS {
        return Err(KernelError::PolicyInvalid(format!(
            "manual destination exceeds {MAX_FIELDS} properties"
        )));
    }
    Ok(())
}

fn checked_workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || !relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(KernelError::IntegrityFailed(
            "destination evidence path escapes the object workspace".to_owned(),
        ));
    }
    Ok(workspace.join(relative_path))
}

fn relative_workspace_path(workspace: &Path, path: &Path) -> Result<String> {
    let invalid_path = || {
        KernelError::IntegrityFailed(
            "destination evidence path is outside the object workspace".to_owned(),
        )
    };
    let relative = path
        .strip_prefix(workspace)
        .ok()
        .filter(|relative| {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
        })
        .ok_or_else(invalid_path)?;
    relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|components| components.join("/"))
        .ok_or_else(invalid_path)
}

fn ensure_real_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| KernelError::io(path, error))?;
    require_real_directory(path)
}

fn require_real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(KernelError::IntegrityFailed(
            "destination evidence directory must be one real non-link directory".to_owned(),
        ));
    }
    Ok(())
}

fn normalized_filter(value: Option<&str>) -> Result<String> {
    let value = value.unwrap_or_default().trim().to_ascii_lowercase();
    if value.len() > 256 || value.chars().any(char::is_control) {
        return Err(KernelError::PolicyInvalid(
            "destination catalogue filter exceeds its safe bound".to_owned(),
        ));
    }
    Ok(value)
}

fn validate_page_size(page_size: usize) -> Result<usize> {
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(KernelError::PolicyInvalid(format!(
            "destination catalogue page size must be in 1..={MAX_PAGE_SIZE}"
        )));
    }
    Ok(page_size)
}

fn cursor_text(value: &Value) -> Result<String> {
    let values = value.as_array().ok_or_else(|| {
        KernelError::IntegrityFailed("destination cursor key is malformed".to_owned())
    })?;
    if values.len() != 1 {
        return Err(KernelError::IntegrityFailed(
            "destination cursor key has wrong arity".to_owned(),
        ));
    }
    values[0].as_str().map(str::to_owned).ok_or_else(|| {
        KernelError::IntegrityFailed("destination cursor key is not text".to_owned())
    })
}

/// Independently replay the complete destination authority chain.
///
/// Returns the number of verified destination authority records. Failures are
/// appended to the caller's global integrity issue list.
pub(crate) fn verify_integrity(connection: &Connection, issues: &mut Vec<String>) -> Result<usize> {
    let required_triggers = [
        "engagement_active_destination_insert_guard",
        "engagement_active_destination_update_guard",
        "destination_artifact_events_no_update",
        "destination_artifact_events_no_delete",
        "destination_catalogue_authorities_no_update",
        "destination_catalogue_authorities_no_delete",
        "destination_profile_draft_revisions_insert_guard",
        "destination_profile_draft_revisions_no_update",
        "destination_profile_draft_revisions_no_delete",
        "destination_profile_draft_heads_transition_guard",
        "destination_profile_draft_heads_no_delete",
        "destination_workshop_attempts_no_update",
        "destination_workshop_attempts_no_delete",
        "destination_profile_origin_bindings_no_update",
        "destination_profile_origin_bindings_no_delete",
        "destination_rebases_no_update",
        "destination_rebases_no_delete",
        "destination_mapping_carry_forwards_no_update",
        "destination_mapping_carry_forwards_no_delete",
    ];
    for trigger in required_triggers {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'trigger' AND name = ?1
             )",
            [trigger],
            |row| row.get(0),
        )?;
        if !exists {
            issues.push(format!(
                "destination authority guard {trigger:?} is missing"
            ));
        }
    }
    let mut checked = 0_usize;
    let mut statement = connection.prepare(
        "SELECT catalogue_authority_id FROM destination_catalogue_authorities
         ORDER BY catalogue_authority_id",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let id = row?;
        checked = checked.saturating_add(1);
        if let Err(error) = load_catalogue_authority(connection, &id) {
            issues.push(format!(
                "destination catalogue authority {id:?} failed replay: {error}"
            ));
        }
    }
    drop(statement);
    let mut draft_statement = connection
        .prepare("SELECT draft_id FROM destination_profile_draft_heads ORDER BY draft_id")?;
    let drafts = draft_statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in drafts {
        let draft_id = row?;
        checked = checked.saturating_add(1);
        match load_draft_head(connection, &draft_id) {
            Ok(head) => {
                if let Err(error) = load_draft_snapshot(
                    connection,
                    &head.draft_id,
                    head.generation,
                    &head.head_revision_hash,
                ) {
                    issues.push(format!(
                        "destination draft {draft_id:?} failed replay: {error}"
                    ));
                }
            }
            Err(error) => issues.push(format!(
                "destination draft {draft_id:?} failed replay: {error}"
            )),
        }
    }
    drop(draft_statement);
    let mut origin_statement = connection.prepare(
        "SELECT origin_binding_id, origin_binding_hash,
                profile_id, profile_version, profile_hash
         FROM destination_profile_origin_bindings ORDER BY origin_binding_id",
    )?;
    let origins = origin_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in origins {
        let (id, hash, profile_id, version, profile_hash) = row?;
        checked = checked.saturating_add(1);
        if let Err(error) =
            verify_origin_binding(connection, &id, &hash, &profile_id, &version, &profile_hash)
        {
            issues.push(format!(
                "destination profile origin {id:?} failed replay: {error}"
            ));
        }
    }
    drop(origin_statement);
    let active: Option<(String, String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT profile_stage, destination_origin, destination_authority_id,
                    destination_origin_hash, selected_profile_id,
                    selected_profile_version, selected_profile_hash
             FROM engagement_active_context
             WHERE profile_stage IN ('REVIEWED', 'APPROVED')",
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
                ))
            },
        )
        .optional()?;
    if let Some((
        profile_stage,
        origin,
        authority_id,
        origin_hash,
        profile_id,
        version,
        profile_hash,
    )) = active
    {
        if origin == "WORKSPACE_PROFILE" {
            if let Err(error) = verify_origin_binding(
                connection,
                &authority_id,
                &origin_hash,
                &profile_id,
                &version,
                &profile_hash,
            ) {
                issues.push(format!(
                    "active workspace destination authority failed replay: {error}"
                ));
            }
            if profile_stage == "APPROVED" {
                match crate::profile_registry::inspect_profile_connection(
                    connection,
                    &profile_id,
                    &version,
                    &profile_hash,
                ) {
                    Ok(profile)
                        if profile.authority == ProfileAuthority::LocalMappingValidation
                            && profile.approval.is_some() => {}
                    Ok(_) => issues.push(
                        "active approved workspace destination lacks exact registry approval authority"
                            .to_owned(),
                    ),
                    Err(error) => issues.push(format!(
                        "active approved workspace destination profile failed replay: {error}"
                    )),
                }
            }
        } else if origin != "CONTROLLED_PACK" || origin_hash != profile_hash {
            issues.push(
                "active destination origin does not match controlled-pack/workspace-profile contract"
                    .to_owned(),
            );
        }
    }
    verify_artifact_event_chains(connection, issues, &mut checked)?;
    verify_workshop_attempts(connection, issues, &mut checked)?;
    verify_rebases(connection, issues, &mut checked)?;
    verify_carry_forwards(connection, issues, &mut checked)?;
    Ok(checked)
}

#[derive(Debug)]
struct ArtifactReplayRow {
    attempt_id: String,
    engagement_id: String,
    workspace_id: String,
    workspace_descriptor_hash: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
    state: String,
    partial_relative_path: String,
    artifact_label: String,
    source_name: String,
    system_context: Option<String>,
    client_context: Option<String>,
    release_context: Option<String>,
    source_sha256: Option<String>,
    source_size: Option<i64>,
    snapshot_id: Option<String>,
    catalogue_authority_id: Option<String>,
    error_code: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

fn verify_artifact_event_chains(
    connection: &Connection,
    issues: &mut Vec<String>,
    checked: &mut usize,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT attempt_id,
                length(CAST(canonical_request_json AS BLOB))
         FROM destination_artifact_attempts ORDER BY attempt_id",
    )?;
    let attempts = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (attempt_id, canonical_bytes) in attempts {
        *checked = checked.saturating_add(1);
        let bytes = usize::try_from(canonical_bytes).unwrap_or(usize::MAX);
        if !(2..=MAX_COMMAND_BYTES).contains(&bytes) {
            issues.push(format!(
                "destination artifact attempt {attempt_id:?} canonical request exceeds replay bounds"
            ));
            continue;
        }
        if let Err(error) = verify_artifact_attempt(connection, &attempt_id, checked) {
            issues.push(format!(
                "destination artifact attempt {attempt_id:?} failed replay: {error}"
            ));
        }
    }
    Ok(())
}

fn verify_artifact_attempt(
    connection: &Connection,
    attempt_id: &str,
    checked: &mut usize,
) -> Result<()> {
    let row = connection.query_row(
        "SELECT attempt_id, engagement_id, workspace_id,
                workspace_descriptor_hash, idempotency_key, request_hash,
                canonical_request_json, state, partial_relative_path,
                artifact_label, source_name, system_context, client_context,
                release_context, source_sha256, source_size, snapshot_id,
                catalogue_authority_id, error_code, created_at, updated_at,
                completed_at
         FROM destination_artifact_attempts WHERE attempt_id = ?1",
        [attempt_id],
        |record| {
            Ok(ArtifactReplayRow {
                attempt_id: record.get(0)?,
                engagement_id: record.get(1)?,
                workspace_id: record.get(2)?,
                workspace_descriptor_hash: record.get(3)?,
                idempotency_key: record.get(4)?,
                request_hash: record.get(5)?,
                canonical_request_json: record.get(6)?,
                state: record.get(7)?,
                partial_relative_path: record.get(8)?,
                artifact_label: record.get(9)?,
                source_name: record.get(10)?,
                system_context: record.get(11)?,
                client_context: record.get(12)?,
                release_context: record.get(13)?,
                source_sha256: record.get(14)?,
                source_size: record.get(15)?,
                snapshot_id: record.get(16)?,
                catalogue_authority_id: record.get(17)?,
                error_code: record.get(18)?,
                created_at: record.get(19)?,
                updated_at: record.get(20)?,
                completed_at: record.get(21)?,
            })
        },
    )?;
    validate_identifier(&row.attempt_id, "destination artifact attempt")?;
    validate_identifier(&row.engagement_id, "destination artifact engagement")?;
    validate_identifier(&row.workspace_id, "destination artifact workspace")?;
    validate_sha256(
        &row.workspace_descriptor_hash,
        "destination artifact workspace descriptor hash",
    )?;
    validate_idempotency(&row.idempotency_key)?;
    validate_sha256(&row.request_hash, "destination artifact request hash")?;
    validate_name(&row.created_at, "destination artifact created time", 64)?;
    validate_name(&row.updated_at, "destination artifact updated time", 64)?;
    if let Some(completed_at) = row.completed_at.as_deref() {
        validate_name(completed_at, "destination artifact completed time", 64)?;
    }
    let command: ReserveEdmxCommand = replay_destination_json(
        &row.canonical_request_json,
        &row.request_hash,
        "destination artifact reservation",
    )?;
    require_schema(&command.schema_version, RESERVE_EDMX_COMMAND_SCHEMA)?;
    validate_idempotency(&command.idempotency_key)?;
    validate_name(&command.source_name, "EDMX source name", 255)?;
    validate_name(&command.artifact_label, "artifact label", 256)?;
    validate_optional_text(command.system.as_deref(), "system provenance")?;
    validate_optional_text(
        command.client_or_tenant.as_deref(),
        "client or tenant provenance",
    )?;
    validate_optional_text(command.release.as_deref(), "release provenance")?;
    if !command.source_name.to_ascii_lowercase().ends_with(".edmx")
        || command.expected_workspace_id != row.workspace_id
        || command.expected_workspace_descriptor_hash != row.workspace_descriptor_hash
        || command.idempotency_key != row.idempotency_key
        || command.artifact_label != row.artifact_label
        || command.source_name != row.source_name
        || command.system != row.system_context
        || command.client_or_tenant != row.client_context
        || command.release != row.release_context
        || row.partial_relative_path != format!("destination/uploads/{}.edmx.part", row.attempt_id)
    {
        return Err(destination_replay_error(
            "artifact reservation command differs from its durable attempt",
        ));
    }
    let workspace: Option<(String, String)> = connection
        .query_row(
            "SELECT workspace_id, descriptor_hash
             FROM object_workspace_bindings WHERE singleton = 1",
            [],
            |record| Ok((record.get(0)?, record.get(1)?)),
        )
        .optional()?;
    if workspace.as_ref()
        != Some(&(
            row.workspace_id.clone(),
            row.workspace_descriptor_hash.clone(),
        ))
    {
        return Err(destination_replay_error(
            "artifact reservation differs from immutable object-workspace authority",
        ));
    }
    let bound_engagement: Option<(String, String)> = connection
        .query_row(
            "SELECT workspace_id, engagement_id
             FROM object_workspace_engagement_bindings WHERE singleton = 1",
            [],
            |record| Ok((record.get(0)?, record.get(1)?)),
        )
        .optional()?;
    if bound_engagement.as_ref() != Some(&(row.workspace_id.clone(), row.engagement_id.clone())) {
        return Err(destination_replay_error(
            "artifact reservation crosses object-workspace engagement authority",
        ));
    }

    let mut event_statement = connection.prepare(
        "SELECT sequence, state, event_hash, previous_event_hash, occurred_at
         FROM destination_artifact_events
         WHERE attempt_id = ?1 ORDER BY sequence",
    )?;
    let events = event_statement
        .query_map([&row.attempt_id], |record| {
            Ok((
                record.get::<_, i64>(0)?,
                record.get::<_, String>(1)?,
                record.get::<_, String>(2)?,
                record.get::<_, Option<String>>(3)?,
                record.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(event_statement);
    if events.is_empty() || events.len() > 2 {
        return Err(destination_replay_error(
            "artifact attempt must have one receiving event and at most one terminal event",
        ));
    }
    let mut previous: Option<String> = None;
    for (index, (_, state, event_hash, stored_previous, occurred_at)) in events.iter().enumerate() {
        *checked = checked.saturating_add(1);
        validate_sha256(event_hash, "destination artifact event hash")?;
        if let Some(stored_previous) = stored_previous.as_deref() {
            validate_sha256(stored_previous, "destination artifact previous event hash")?;
        }
        validate_name(occurred_at, "destination artifact event time", 64)?;
        let expected_state = if index == 0 {
            "RECEIVING"
        } else {
            row.state.as_str()
        };
        if state != expected_state
            || stored_previous.as_deref() != previous.as_deref()
            || *event_hash
                != artifact_event_hash(&row.attempt_id, state, previous.as_deref(), occurred_at)?
            || (index == 0 && occurred_at != &row.created_at)
        {
            return Err(destination_replay_error(
                "artifact event canonical chain or state transition is invalid",
            ));
        }
        previous = Some(event_hash.clone());
    }
    let expected_events = usize::from(row.state != "RECEIVING").saturating_add(1);
    let final_event = events.last().expect("nonempty artifact event chain");
    if events.len() != expected_events
        || final_event.1 != row.state
        || final_event.4 != row.updated_at
        || row
            .completed_at
            .as_deref()
            .is_some_and(|completed| completed != final_event.4)
    {
        return Err(destination_replay_error(
            "artifact attempt state/timestamps differ from its event chain",
        ));
    }
    match row.state.as_str() {
        "RECEIVING" => {
            if row.source_sha256.is_some()
                || row.source_size.is_some()
                || row.snapshot_id.is_some()
                || row.catalogue_authority_id.is_some()
                || row.error_code.is_some()
                || row.completed_at.is_some()
                || row.updated_at != row.created_at
            {
                return Err(destination_replay_error(
                    "receiving artifact attempt retains terminal evidence",
                ));
            }
        }
        "READY" => verify_ready_artifact_attempt(connection, &row)?,
        "REJECTED" | "INTERRUPTED" => {
            if row.error_code.as_deref().is_none()
                || row.completed_at.as_deref() != Some(row.updated_at.as_str())
            {
                return Err(destination_replay_error(
                    "failed artifact attempt lacks exact terminal evidence",
                ));
            }
            validate_identifier(
                row.error_code.as_deref().unwrap_or_default(),
                "destination artifact error code",
            )?;
        }
        _ => {
            return Err(destination_replay_error(
                "artifact attempt has an unsupported state",
            ));
        }
    }
    Ok(())
}

fn verify_ready_artifact_attempt(connection: &Connection, row: &ArtifactReplayRow) -> Result<()> {
    let source_sha256 = row
        .source_sha256
        .as_deref()
        .ok_or_else(|| destination_replay_error("ready artifact attempt has no source hash"))?;
    let source_size = row
        .source_size
        .ok_or_else(|| destination_replay_error("ready artifact attempt has no source size"))?;
    let snapshot_id = row.snapshot_id.as_deref().ok_or_else(|| {
        destination_replay_error("ready artifact attempt has no snapshot identity")
    })?;
    let catalogue_id = row.catalogue_authority_id.as_deref().ok_or_else(|| {
        destination_replay_error("ready artifact attempt has no catalogue authority")
    })?;
    validate_sha256(source_sha256, "destination artifact source hash")?;
    if !(1..=i64::try_from(MAX_EDMX_BYTES).unwrap_or(i64::MAX)).contains(&source_size)
        || row.error_code.is_some()
        || row.completed_at.as_deref() != Some(row.updated_at.as_str())
    {
        return Err(destination_replay_error(
            "ready artifact attempt terminal evidence is outside its bounds",
        ));
    }
    let snapshot: (String, i64) = connection.query_row(
        "SELECT sha256, byte_size FROM metadata_snapshots WHERE id = ?1",
        [snapshot_id],
        |record| Ok((record.get(0)?, record.get(1)?)),
    )?;
    if snapshot.0 != source_sha256 || snapshot.1 != source_size {
        return Err(destination_replay_error(
            "ready artifact source identity differs from immutable metadata snapshot",
        ));
    }
    let authority = load_catalogue_authority(connection, catalogue_id)?;
    let authority_engagement: String = connection.query_row(
        "SELECT engagement_id FROM destination_catalogue_authorities
         WHERE catalogue_authority_id = ?1",
        [catalogue_id],
        |record| record.get(0),
    )?;
    if authority.origin != "EDMX"
        || authority_engagement != row.engagement_id
        || authority.snapshot_id.as_deref() != Some(snapshot_id)
        || authority.snapshot_sha256.as_deref() != Some(source_sha256)
        || authority.artifact_label != row.artifact_label
    {
        return Err(destination_replay_error(
            "ready artifact attempt differs from its catalogue authority",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct WorkshopReplayRow {
    attempt_id: String,
    engagement_id: String,
    command_kind: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
    expected_generation: u64,
    result_generation: u64,
    result_reference: String,
    result_hash: String,
    created_at: String,
}

fn verify_workshop_attempts(
    connection: &Connection,
    issues: &mut Vec<String>,
    checked: &mut usize,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT attempt_id,
                length(CAST(canonical_request_json AS BLOB))
         FROM destination_workshop_attempts ORDER BY attempt_id",
    )?;
    let attempts = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (attempt_id, canonical_bytes) in attempts {
        *checked = checked.saturating_add(1);
        if !(2..=MAX_COMMAND_BYTES)
            .contains(&usize::try_from(canonical_bytes).unwrap_or(usize::MAX))
        {
            issues.push(format!(
                "destination workshop attempt {attempt_id:?} canonical request exceeds replay bounds"
            ));
            continue;
        }
        if let Err(error) = verify_workshop_attempt(connection, &attempt_id) {
            issues.push(format!(
                "destination workshop attempt {attempt_id:?} failed replay: {error}"
            ));
        }
    }
    Ok(())
}

fn load_workshop_replay_row(
    connection: &Connection,
    attempt_id: &str,
) -> Result<WorkshopReplayRow> {
    connection
        .query_row(
            "SELECT attempt_id, engagement_id, command_kind, idempotency_key,
                    request_hash, canonical_request_json, expected_generation,
                    result_generation, result_reference, result_hash, created_at
             FROM destination_workshop_attempts WHERE attempt_id = ?1",
            [attempt_id],
            |row| {
                let expected_generation = row.get::<_, i64>(6)?;
                let result_generation = row.get::<_, i64>(7)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    expected_generation,
                    result_generation,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .map_err(Into::into)
        .and_then(|row| {
            Ok(WorkshopReplayRow {
                attempt_id: row.0,
                engagement_id: row.1,
                command_kind: row.2,
                idempotency_key: row.3,
                request_hash: row.4,
                canonical_request_json: row.5,
                expected_generation: u64::try_from(row.6).map_err(|_| {
                    destination_replay_error(
                        "workshop attempt expected engagement generation is negative",
                    )
                })?,
                result_generation: u64::try_from(row.7).map_err(|_| {
                    destination_replay_error(
                        "workshop attempt result engagement generation is negative",
                    )
                })?,
                result_reference: row.8,
                result_hash: row.9,
                created_at: row.10,
            })
        })
}

fn verify_workshop_attempt(connection: &Connection, attempt_id: &str) -> Result<()> {
    let attempt = load_workshop_replay_row(connection, attempt_id)?;
    validate_identifier(&attempt.attempt_id, "destination workshop attempt")?;
    validate_identifier(&attempt.engagement_id, "destination workshop engagement")?;
    validate_idempotency(&attempt.idempotency_key)?;
    validate_sha256(&attempt.request_hash, "destination workshop request hash")?;
    validate_identifier(
        &attempt.result_reference,
        "destination workshop result reference",
    )?;
    validate_sha256(&attempt.result_hash, "destination workshop result hash")?;
    validate_name(&attempt.created_at, "destination workshop attempt time", 64)?;
    if attempt.result_generation != attempt.expected_generation.saturating_add(1) {
        return Err(destination_replay_error(
            "workshop attempt engagement generation did not advance exactly once",
        ));
    }
    match attempt.command_kind.as_str() {
        "CREATE_MANUAL_CATALOGUE" => verify_manual_attempt(connection, &attempt),
        "START_DRAFT" => verify_start_draft_attempt(connection, &attempt),
        "SAVE_DRAFT" => verify_save_draft_attempt(connection, &attempt),
        "REBASE_DRAFT" => verify_rebase_attempt_link(connection, &attempt),
        "FREEZE_DRAFT" => verify_freeze_draft_attempt(connection, &attempt),
        "APPROVE_PROFILE" => verify_approve_profile_attempt(connection, &attempt),
        "CARRY_FORWARD_MAPPING" => verify_carry_forward_attempt_link(connection, &attempt),
        _ => Err(destination_replay_error(format!(
            "workshop attempt has unsupported command kind {:?}",
            attempt.command_kind
        ))),
    }
}

fn verify_manual_attempt(connection: &Connection, attempt: &WorkshopReplayRow) -> Result<()> {
    let command: CreateManualCatalogueCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "manual destination command",
    )?;
    require_schema(
        &command.schema_version,
        CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA,
    )?;
    validate_idempotency(&command.idempotency_key)?;
    validate_name(&command.artifact_label, "manual destination label", 256)?;
    validate_name(&command.actor, "manual destination actor", 256)?;
    validate_name(&command.reason, "manual destination reason", MAX_TEXT_BYTES)?;
    validate_manual_entities(&command.entities)?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
    {
        return Err(destination_replay_error(
            "manual destination command differs from its workshop attempt",
        ));
    }
    let catalogue_value = json!({
        "schemaVersion": "migration-stream/manual-technical-catalogue/v1",
        "origin": "MANUAL",
        "artifactLabel": command.artifact_label,
        "actor": command.actor,
        "reason": command.reason,
        "entities": command.entities,
    });
    let expected_hash = canonical_hash(&catalogue_value)?;
    let expected_id = format!("destination-{}", &expected_hash[..32]);
    if attempt.result_reference != expected_id || attempt.result_hash != expected_hash {
        return Err(destination_replay_error(
            "manual destination attempt has a different canonical result identity",
        ));
    }
    let authority = load_catalogue_authority(connection, &attempt.result_reference)?;
    let (engagement_id, created_at): (String, String) = connection.query_row(
        "SELECT engagement_id, created_at
         FROM destination_catalogue_authorities
         WHERE catalogue_authority_id = ?1",
        [&attempt.result_reference],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if authority.origin != "MANUAL"
        || authority.catalogue_hash != expected_hash
        || authority.canonical_catalogue_json != canonical_json_string(&catalogue_value)?
        || authority.artifact_label != command.artifact_label
        || engagement_id != attempt.engagement_id
        || created_at != attempt.created_at
    {
        return Err(destination_replay_error(
            "manual destination result differs from its catalogue authority",
        ));
    }
    Ok(())
}

fn verify_start_draft_attempt(connection: &Connection, attempt: &WorkshopReplayRow) -> Result<()> {
    let command: StartDestinationDraftCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "start destination draft command",
    )?;
    require_schema(&command.schema_version, START_DRAFT_COMMAND_SCHEMA)?;
    validate_idempotency(&command.idempotency_key)?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || command.child_technical_entities.len() > MAX_ENTITIES.saturating_sub(1)
    {
        return Err(destination_replay_error(
            "start destination draft command differs from its workshop attempt",
        ));
    }
    let authority = load_catalogue_authority(connection, &command.catalogue_authority_id)?;
    let authority_engagement: String = connection.query_row(
        "SELECT engagement_id FROM destination_catalogue_authorities
         WHERE catalogue_authority_id = ?1",
        [&command.catalogue_authority_id],
        |row| row.get(0),
    )?;
    if authority.catalogue_hash != command.expected_catalogue_hash
        || authority_engagement != attempt.engagement_id
    {
        return Err(destination_replay_error(
            "start draft command differs from its exact catalogue authority",
        ));
    }
    let catalogue = catalogue_model(&authority)?;
    let mut selected = Vec::with_capacity(1 + command.child_technical_entities.len());
    selected.push(command.root_technical_entity.clone());
    selected.extend(command.child_technical_entities.iter().cloned());
    let snapshot = DestinationDraftSnapshot {
        profile_id: command.profile_id.clone(),
        title: command.title.clone(),
        owner: command.owner.clone(),
        entities: initial_draft_entities(&catalogue, &selected, &command.root_technical_entity)?,
        relationships: Vec::new(),
        rules: Vec::new(),
        compatibility_acknowledgements: Vec::new(),
    };
    validate_draft_snapshot(&snapshot)?;
    let expected_hash = canonical_hash(&serde_json::to_value(&snapshot)?)?;
    let result: (String, String, String, String, String, Option<String>) = connection.query_row(
        "SELECT head.engagement_id, head.catalogue_authority_id,
                    head.catalogue_hash, revision.revision_hash,
                    revision.actor, revision.parent_revision_hash
             FROM destination_profile_draft_heads AS head
             JOIN destination_profile_draft_revisions AS revision
               ON revision.draft_id = head.draft_id AND revision.generation = 1
             WHERE head.draft_id = ?1",
        [&attempt.result_reference],
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
    )?;
    let revision: (String, String, String) = connection.query_row(
        "SELECT canonical_draft_json, reason, created_at
         FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 AND generation = 1",
        [&attempt.result_reference],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if result.0 != attempt.engagement_id
        || result.1 != authority.id
        || result.2 != authority.catalogue_hash
        || result.3 != expected_hash
        || result.4 != command.actor
        || result.5.is_some()
        || revision.0 != canonical_json_string(&serde_json::to_value(&snapshot)?)?
        || revision.1 != command.reason
        || revision.2 != attempt.created_at
        || attempt.result_hash != expected_hash
    {
        return Err(destination_replay_error(
            "start draft result differs from its first immutable revision",
        ));
    }
    Ok(())
}

fn verify_save_draft_attempt(connection: &Connection, attempt: &WorkshopReplayRow) -> Result<()> {
    let command: SaveDestinationDraftCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "save destination draft command",
    )?;
    require_schema(&command.schema_version, SAVE_DRAFT_COMMAND_SCHEMA)?;
    validate_idempotency(&command.idempotency_key)?;
    validate_draft_snapshot(&command.snapshot)?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || command.draft_id != attempt.result_reference
    {
        return Err(destination_replay_error(
            "save destination draft command differs from its workshop attempt",
        ));
    }
    load_draft_snapshot(
        connection,
        &command.draft_id,
        command.expected_draft_generation,
        &command.expected_draft_hash,
    )?;
    let result_generation = command.expected_draft_generation.saturating_add(1);
    let revision: (String, Option<String>, String, String, String) = connection.query_row(
        "SELECT revision_hash, parent_revision_hash, canonical_draft_json,
                    actor, reason
             FROM destination_profile_draft_revisions
             WHERE draft_id = ?1 AND generation = ?2",
        params![
            command.draft_id,
            i64::try_from(result_generation).unwrap_or(i64::MAX)
        ],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let revision_created_at: String = connection.query_row(
        "SELECT created_at FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 AND generation = ?2",
        params![
            command.draft_id,
            i64::try_from(result_generation).unwrap_or(i64::MAX)
        ],
        |row| row.get(0),
    )?;
    let snapshot_value = serde_json::to_value(&command.snapshot)?;
    let expected_json = canonical_json_string(&snapshot_value)?;
    let expected_hash = canonical_hash(&snapshot_value)?;
    let (head_engagement, catalogue_id): (String, String) = connection.query_row(
        "SELECT engagement_id, catalogue_authority_id
         FROM destination_profile_draft_heads WHERE draft_id = ?1",
        [&command.draft_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let authority = load_catalogue_authority(connection, &catalogue_id)?;
    let prior_properties = rebase_source_technical_properties(connection, &command.draft_id)?;
    validate_draft_against_catalogue(&command.snapshot, &authority, &prior_properties)?;
    if head_engagement != attempt.engagement_id
        || revision.0 != expected_hash
        || revision.1.as_deref() != Some(command.expected_draft_hash.as_str())
        || revision.2 != expected_json
        || revision.3 != command.actor
        || revision.4 != command.reason
        || revision_created_at != attempt.created_at
        || attempt.result_hash != expected_hash
    {
        return Err(destination_replay_error(
            "save destination draft result differs from its immutable revision",
        ));
    }
    Ok(())
}

fn verify_freeze_draft_attempt(connection: &Connection, attempt: &WorkshopReplayRow) -> Result<()> {
    let command: FreezeDestinationDraftCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "freeze destination draft command",
    )?;
    require_schema(&command.schema_version, FREEZE_DRAFT_COMMAND_SCHEMA)?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || command.draft_id != attempt.result_reference
        || command.acknowledgement != DESTINATION_FREEZE_ACKNOWLEDGEMENT
    {
        return Err(destination_replay_error(
            "freeze destination draft command differs from its workshop attempt",
        ));
    }
    let snapshot = load_draft_snapshot(
        connection,
        &command.draft_id,
        command.expected_draft_generation,
        &command.expected_draft_hash,
    )?;
    let binding: (String, String, String, String, String, String) = connection.query_row(
        "SELECT origin_binding_id, origin_binding_hash, engagement_id,
                    profile_id, profile_hash, created_at
             FROM destination_profile_origin_bindings
             WHERE draft_id = ?1 AND draft_generation = ?2
               AND draft_revision_hash = ?3",
        params![
            command.draft_id,
            i64::try_from(command.expected_draft_generation).unwrap_or(i64::MAX),
            command.expected_draft_hash
        ],
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
    )?;
    let binding_version: String = connection.query_row(
        "SELECT profile_version FROM destination_profile_origin_bindings
         WHERE origin_binding_id = ?1",
        [&binding.0],
        |row| row.get(0),
    )?;
    verify_origin_binding(
        connection,
        &binding.0,
        &binding.1,
        &binding.3,
        &binding_version,
        &binding.4,
    )?;
    if binding.2 != attempt.engagement_id
        || binding.3 != snapshot.profile_id
        || binding_version != command.profile_version
        || binding.4 != attempt.result_hash
        || binding.5 != attempt.created_at
    {
        return Err(destination_replay_error(
            "freeze destination result differs from its origin/profile authority",
        ));
    }
    Ok(())
}

fn verify_approve_profile_attempt(
    connection: &Connection,
    attempt: &WorkshopReplayRow,
) -> Result<()> {
    let command: ApproveDestinationProfileCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "approve destination profile command",
    )?;
    require_schema(
        &command.schema_version,
        APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA,
    )?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || command.authority_acknowledgement != LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT
    {
        return Err(destination_replay_error(
            "destination profile approval command differs from its workshop attempt",
        ));
    }
    let binding: (String, String, String, String, String) = connection.query_row(
        "SELECT engagement_id, profile_id, profile_version, profile_hash,
                origin_binding_hash
         FROM destination_profile_origin_bindings
         WHERE origin_binding_id = ?1",
        [&attempt.result_reference],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    if binding.0 != attempt.engagement_id
        || binding.1 != command.profile_id
        || binding.2 != command.profile_version
        || binding.3 != command.profile_hash
        || binding.4 != command.origin_binding_hash
    {
        return Err(destination_replay_error(
            "destination profile approval differs from its origin binding",
        ));
    }
    verify_origin_binding(
        connection,
        &attempt.result_reference,
        &command.origin_binding_hash,
        &command.profile_id,
        &command.profile_version,
        &command.profile_hash,
    )?;
    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        &command.profile_id,
        &command.profile_version,
        &command.profile_hash,
    )?;
    let approval = profile.approval.ok_or_else(|| {
        destination_replay_error("destination profile approval has no registry approval")
    })?;
    if profile.authority != ProfileAuthority::LocalMappingValidation
        || approval.approval_hash != attempt.result_hash
        || approval.approver != command.actor
        || approval.reason != command.reason
        || !approval.acknowledged
    {
        return Err(destination_replay_error(
            "destination profile approval differs from exact registry authority",
        ));
    }
    Ok(())
}

fn verify_rebase_attempt_link(connection: &Connection, attempt: &WorkshopReplayRow) -> Result<()> {
    let command: RebaseDestinationDraftCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "rebase destination draft command",
    )?;
    require_schema(&command.schema_version, REBASE_DRAFT_COMMAND_SCHEMA)?;
    let row: (String, String, String, String, String, String, String) = connection.query_row(
        "SELECT engagement_id, from_draft_id, from_revision_hash,
                    to_catalogue_authority_id, to_catalogue_hash,
                    actor, reason
             FROM destination_rebases
             WHERE result_draft_id = ?1 AND diff_hash = ?2",
        params![attempt.result_reference, attempt.result_hash],
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
    let created_at: String = connection.query_row(
        "SELECT created_at FROM destination_rebases
         WHERE result_draft_id = ?1 AND diff_hash = ?2",
        params![attempt.result_reference, attempt.result_hash],
        |row| row.get(0),
    )?;
    let source_generation: i64 = connection.query_row(
        "SELECT generation FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 AND revision_hash = ?2",
        params![command.draft_id, command.expected_draft_hash],
        |row| row.get(0),
    )?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || u64::try_from(source_generation).ok() != Some(command.expected_draft_generation)
        || row.0 != attempt.engagement_id
        || row.1 != command.draft_id
        || row.2 != command.expected_draft_hash
        || row.3 != command.target_catalogue_authority_id
        || row.4 != command.expected_target_catalogue_hash
        || row.5 != command.actor
        || row.6 != command.reason
        || created_at != attempt.created_at
    {
        return Err(destination_replay_error(
            "destination rebase attempt differs from its immutable result",
        ));
    }
    Ok(())
}

fn verify_carry_forward_attempt_link(
    connection: &Connection,
    attempt: &WorkshopReplayRow,
) -> Result<()> {
    let command: CarryForwardMappingCommand = replay_destination_json(
        &attempt.canonical_request_json,
        &attempt.request_hash,
        "mapping carry-forward command",
    )?;
    require_schema(
        &command.schema_version,
        CARRY_FORWARD_MAPPING_COMMAND_SCHEMA,
    )?;
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
    ) = connection.query_row(
        "SELECT engagement_id, source_mapping_id, source_mapping_version,
                source_mapping_hash, target_profile_id,
                target_profile_version, target_profile_hash, actor, reason
         FROM destination_mapping_carry_forwards
         WHERE carry_forward_id = ?1 AND result_hash = ?2",
        params![attempt.result_reference, attempt.result_hash],
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
    )?;
    let created_at: String = connection.query_row(
        "SELECT created_at FROM destination_mapping_carry_forwards
         WHERE carry_forward_id = ?1 AND result_hash = ?2",
        params![attempt.result_reference, attempt.result_hash],
        |row| row.get(0),
    )?;
    if command.expected_engagement_generation != attempt.expected_generation
        || command.idempotency_key != attempt.idempotency_key
        || row.0 != attempt.engagement_id
        || row.1 != command.source_mapping_id
        || row.2 != command.source_mapping_version
        || row.3 != command.source_mapping_hash
        || row.4 != command.target_profile_id
        || row.5 != command.target_profile_version
        || row.6 != command.target_profile_hash
        || row.7 != command.actor
        || row.8 != command.reason
        || created_at != attempt.created_at
    {
        return Err(destination_replay_error(
            "mapping carry-forward attempt differs from its immutable result",
        ));
    }
    Ok(())
}

fn replay_destination_json<T>(canonical_json: &str, expected_hash: &str, label: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    if !(2..=MAX_COMMAND_BYTES).contains(&canonical_json.len()) {
        return Err(destination_replay_error(format!(
            "{label} canonical bytes exceed replay bounds"
        )));
    }
    validate_sha256(expected_hash, &format!("{label} hash"))?;
    let value = parse_unique_json(canonical_json.as_bytes())?;
    let replayed = canonical_json_string(&value)?;
    if replayed != canonical_json || sha256_hex(replayed.as_bytes()) != expected_hash {
        return Err(destination_replay_error(format!(
            "{label} canonical bytes/hash mismatch"
        )));
    }
    let typed: T = serde_json::from_value(value)?;
    if canonical_command(&typed)? != canonical_json {
        return Err(destination_replay_error(format!(
            "{label} typed canonical replay mismatch"
        )));
    }
    Ok(typed)
}

fn destination_replay_error(message: impl Into<String>) -> KernelError {
    KernelError::IntegrityFailed(format!("destination integrity replay: {}", message.into()))
}

#[derive(Debug)]
struct RebaseReplayRow {
    rebase_id: String,
    engagement_id: String,
    from_draft_id: String,
    from_revision_hash: String,
    to_catalogue_authority_id: String,
    to_catalogue_hash: String,
    result_draft_id: String,
    result_revision_hash: String,
    algorithm: String,
    canonical_diff_json: String,
    diff_hash: String,
    actor: String,
    reason: String,
    created_at: String,
}

fn verify_rebases(
    connection: &Connection,
    issues: &mut Vec<String>,
    checked: &mut usize,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT rebase_id, length(CAST(canonical_diff_json AS BLOB))
         FROM destination_rebases ORDER BY rebase_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (rebase_id, canonical_bytes) in rows {
        *checked = checked.saturating_add(1);
        if !(2..=MAX_COMMAND_BYTES)
            .contains(&usize::try_from(canonical_bytes).unwrap_or(usize::MAX))
        {
            issues.push(format!(
                "destination rebase {rebase_id:?} canonical diff exceeds replay bounds"
            ));
            continue;
        }
        if let Err(error) = verify_rebase(connection, &rebase_id) {
            issues.push(format!(
                "destination rebase {rebase_id:?} failed replay: {error}"
            ));
        }
    }
    Ok(())
}

fn verify_rebase(connection: &Connection, rebase_id: &str) -> Result<()> {
    let row = connection.query_row(
        "SELECT rebase_id, engagement_id, from_draft_id,
                from_revision_hash, to_catalogue_authority_id,
                to_catalogue_hash, result_draft_id, result_revision_hash,
                algorithm, canonical_diff_json, diff_hash, actor, reason,
                created_at
         FROM destination_rebases WHERE rebase_id = ?1",
        [rebase_id],
        |record| {
            Ok(RebaseReplayRow {
                rebase_id: record.get(0)?,
                engagement_id: record.get(1)?,
                from_draft_id: record.get(2)?,
                from_revision_hash: record.get(3)?,
                to_catalogue_authority_id: record.get(4)?,
                to_catalogue_hash: record.get(5)?,
                result_draft_id: record.get(6)?,
                result_revision_hash: record.get(7)?,
                algorithm: record.get(8)?,
                canonical_diff_json: record.get(9)?,
                diff_hash: record.get(10)?,
                actor: record.get(11)?,
                reason: record.get(12)?,
                created_at: record.get(13)?,
            })
        },
    )?;
    validate_identifier(&row.rebase_id, "destination rebase")?;
    validate_identifier(&row.engagement_id, "destination rebase engagement")?;
    validate_identifier(&row.from_draft_id, "destination rebase source draft")?;
    validate_sha256(
        &row.from_revision_hash,
        "destination rebase source revision hash",
    )?;
    validate_identifier(
        &row.to_catalogue_authority_id,
        "destination rebase target catalogue",
    )?;
    validate_sha256(
        &row.to_catalogue_hash,
        "destination rebase target catalogue hash",
    )?;
    validate_identifier(&row.result_draft_id, "destination rebase result draft")?;
    validate_sha256(
        &row.result_revision_hash,
        "destination rebase result revision hash",
    )?;
    validate_sha256(&row.diff_hash, "destination rebase diff hash")?;
    validate_name(&row.actor, "destination rebase actor", 256)?;
    validate_name(&row.reason, "destination rebase reason", MAX_TEXT_BYTES)?;
    validate_name(&row.created_at, "destination rebase time", 64)?;
    if row.algorithm != REBASE_ALGORITHM {
        return Err(destination_replay_error(
            "destination rebase uses an unsupported algorithm",
        ));
    }
    let diff_value = parse_unique_json(row.canonical_diff_json.as_bytes())?;
    if canonical_json_string(&diff_value)? != row.canonical_diff_json
        || canonical_hash(&diff_value)? != row.diff_hash
    {
        return Err(destination_replay_error(
            "destination rebase diff canonical bytes/hash mismatch",
        ));
    }
    diff_counts(&diff_value)?;

    let source: (i64, String, String, String) = connection.query_row(
        "SELECT revision.generation, head.engagement_id,
                head.catalogue_authority_id, head.catalogue_hash
         FROM destination_profile_draft_revisions AS revision
         JOIN destination_profile_draft_heads AS head
           ON head.draft_id = revision.draft_id
         WHERE revision.draft_id = ?1 AND revision.revision_hash = ?2",
        params![row.from_draft_id, row.from_revision_hash],
        |record| {
            Ok((
                record.get(0)?,
                record.get(1)?,
                record.get(2)?,
                record.get(3)?,
            ))
        },
    )?;
    let source_generation = u64::try_from(source.0).map_err(|_| {
        destination_replay_error("destination rebase source generation is negative")
    })?;
    if source.1 != row.engagement_id {
        return Err(destination_replay_error(
            "destination rebase source draft crosses engagement authority",
        ));
    }
    let source_snapshot = load_draft_snapshot(
        connection,
        &row.from_draft_id,
        source_generation,
        &row.from_revision_hash,
    )?;
    let source_authority = load_catalogue_authority(connection, &source.2)?;
    if source_authority.catalogue_hash != source.3 {
        return Err(destination_replay_error(
            "destination rebase source draft differs from exact catalogue authority",
        ));
    }
    let target_authority = load_catalogue_authority(connection, &row.to_catalogue_authority_id)?;
    let target_engagement: String = connection.query_row(
        "SELECT engagement_id FROM destination_catalogue_authorities
         WHERE catalogue_authority_id = ?1",
        [&row.to_catalogue_authority_id],
        |record| record.get(0),
    )?;
    if target_engagement != row.engagement_id
        || target_authority.catalogue_hash != row.to_catalogue_hash
    {
        return Err(destination_replay_error(
            "destination rebase target catalogue crosses or differs from exact authority",
        ));
    }
    let source_model = catalogue_model(&source_authority)?;
    let target_model = catalogue_model(&target_authority)?;
    let (expected_snapshot, expected_diff) =
        rebase_snapshot(&source_snapshot, &source_model, &target_model)?;
    validate_draft_snapshot(&expected_snapshot)?;
    let prior_properties = rebase_source_technical_properties(connection, &row.result_draft_id)?;
    validate_draft_against_catalogue(&expected_snapshot, &target_authority, &prior_properties)?;
    let expected_snapshot_value = serde_json::to_value(&expected_snapshot)?;
    let expected_snapshot_json = canonical_json_string(&expected_snapshot_value)?;
    let expected_snapshot_hash = canonical_hash(&expected_snapshot_value)?;
    let expected_diff_json = canonical_json_string(&expected_diff)?;
    let expected_diff_hash = canonical_hash(&expected_diff)?;
    if expected_diff_json != row.canonical_diff_json
        || expected_diff_hash != row.diff_hash
        || expected_snapshot_hash != row.result_revision_hash
    {
        return Err(destination_replay_error(
            "destination rebase output does not recompute from exact source and target",
        ));
    }
    let result: (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT head.engagement_id, head.catalogue_authority_id,
                head.catalogue_hash, revision.revision_hash,
                revision.parent_revision_hash, revision.canonical_draft_json,
                revision.actor, revision.reason
         FROM destination_profile_draft_heads AS head
         JOIN destination_profile_draft_revisions AS revision
           ON revision.draft_id = head.draft_id AND revision.generation = 1
         WHERE head.draft_id = ?1",
        [&row.result_draft_id],
        |record| {
            Ok((
                record.get(0)?,
                record.get(1)?,
                record.get(2)?,
                record.get(3)?,
                record.get(4)?,
                record.get(5)?,
                record.get(6)?,
                record.get(7)?,
            ))
        },
    )?;
    let result_created_at: String = connection.query_row(
        "SELECT created_at FROM destination_profile_draft_revisions
         WHERE draft_id = ?1 AND generation = 1",
        [&row.result_draft_id],
        |record| record.get(0),
    )?;
    if result.0 != row.engagement_id
        || result.1 != row.to_catalogue_authority_id
        || result.2 != row.to_catalogue_hash
        || result.3 != row.result_revision_hash
        || result.4.is_some()
        || result.5 != expected_snapshot_json
        || result.6 != row.actor
        || result.7 != row.reason
        || result_created_at != row.created_at
    {
        return Err(destination_replay_error(
            "destination rebase result differs from its first immutable draft revision",
        ));
    }
    let linked_attempts: i64 = connection.query_row(
        "SELECT COUNT(*) FROM destination_workshop_attempts
         WHERE engagement_id = ?1 AND command_kind = 'REBASE_DRAFT'
           AND result_reference = ?2 AND result_hash = ?3",
        params![row.engagement_id, row.result_draft_id, row.diff_hash],
        |record| record.get(0),
    )?;
    if linked_attempts != 1 {
        return Err(destination_replay_error(
            "destination rebase lacks one exact typed workshop attempt",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct CarryForwardReplayRow {
    carry_forward_id: String,
    engagement_id: String,
    source_mapping_id: String,
    source_mapping_version: String,
    source_mapping_hash: String,
    source_profile_id: String,
    source_profile_version: String,
    source_profile_hash: String,
    target_profile_id: String,
    target_profile_version: String,
    target_profile_hash: String,
    result_draft_id: String,
    result_draft_hash: String,
    algorithm: String,
    canonical_result_json: String,
    result_hash: String,
    actor: String,
    reason: String,
    created_at: String,
}

fn verify_carry_forwards(
    connection: &Connection,
    issues: &mut Vec<String>,
    checked: &mut usize,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT carry_forward_id,
                length(CAST(canonical_result_json AS BLOB))
         FROM destination_mapping_carry_forwards ORDER BY carry_forward_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (carry_forward_id, canonical_bytes) in rows {
        *checked = checked.saturating_add(1);
        if !(2..=MAX_COMMAND_BYTES)
            .contains(&usize::try_from(canonical_bytes).unwrap_or(usize::MAX))
        {
            issues.push(format!(
                "destination mapping carry-forward {carry_forward_id:?} canonical result exceeds replay bounds"
            ));
            continue;
        }
        if let Err(error) = verify_carry_forward(connection, &carry_forward_id) {
            issues.push(format!(
                "destination mapping carry-forward {carry_forward_id:?} failed replay: {error}"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_carry_forward(connection: &Connection, carry_forward_id: &str) -> Result<()> {
    use crate::engagement::MappingDraftSnapshot;
    use crate::mapping::MappingDocument;

    let row = connection.query_row(
        "SELECT carry_forward_id, engagement_id,
                source_mapping_id, source_mapping_version, source_mapping_hash,
                source_profile_id, source_profile_version, source_profile_hash,
                target_profile_id, target_profile_version, target_profile_hash,
                result_draft_id, result_draft_hash, algorithm,
                canonical_result_json, result_hash, actor, reason, created_at
         FROM destination_mapping_carry_forwards
         WHERE carry_forward_id = ?1",
        [carry_forward_id],
        |record| {
            Ok(CarryForwardReplayRow {
                carry_forward_id: record.get(0)?,
                engagement_id: record.get(1)?,
                source_mapping_id: record.get(2)?,
                source_mapping_version: record.get(3)?,
                source_mapping_hash: record.get(4)?,
                source_profile_id: record.get(5)?,
                source_profile_version: record.get(6)?,
                source_profile_hash: record.get(7)?,
                target_profile_id: record.get(8)?,
                target_profile_version: record.get(9)?,
                target_profile_hash: record.get(10)?,
                result_draft_id: record.get(11)?,
                result_draft_hash: record.get(12)?,
                algorithm: record.get(13)?,
                canonical_result_json: record.get(14)?,
                result_hash: record.get(15)?,
                actor: record.get(16)?,
                reason: record.get(17)?,
                created_at: record.get(18)?,
            })
        },
    )?;
    validate_identifier(&row.carry_forward_id, "destination mapping carry-forward")?;
    validate_identifier(
        &row.engagement_id,
        "destination mapping carry-forward engagement",
    )?;
    validate_identifier(&row.source_mapping_id, "source mapping ID")?;
    validate_version(&row.source_mapping_version)?;
    validate_sha256(&row.source_mapping_hash, "source mapping hash")?;
    validate_profile_id(&row.source_profile_id)?;
    validate_version(&row.source_profile_version)?;
    validate_sha256(&row.source_profile_hash, "source profile hash")?;
    validate_profile_id(&row.target_profile_id)?;
    validate_version(&row.target_profile_version)?;
    validate_sha256(&row.target_profile_hash, "target profile hash")?;
    validate_identifier(&row.result_draft_id, "carry-forward result draft")?;
    validate_sha256(&row.result_draft_hash, "carry-forward draft hash")?;
    validate_sha256(&row.result_hash, "carry-forward result hash")?;
    validate_name(&row.actor, "mapping carry-forward actor", 256)?;
    validate_name(&row.reason, "mapping carry-forward reason", MAX_TEXT_BYTES)?;
    validate_name(&row.created_at, "mapping carry-forward time", 64)?;
    if row.algorithm != CARRY_FORWARD_ALGORITHM {
        return Err(destination_replay_error(
            "mapping carry-forward uses an unsupported algorithm",
        ));
    }
    let stored_result = parse_unique_json(row.canonical_result_json.as_bytes())?;
    if canonical_json_string(&stored_result)? != row.canonical_result_json
        || canonical_hash(&stored_result)? != row.result_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward result canonical bytes/hash mismatch",
        ));
    }
    let source_authority = carry_forward_source_from_result(&stored_result)?;
    if source_authority.mapping_id != row.source_mapping_id
        || source_authority.mapping_version != row.source_mapping_version
        || source_authority.mapping_hash != row.source_mapping_hash
        || source_authority.profile_id != row.source_profile_id
        || source_authority.profile_version != row.source_profile_version
        || source_authority.profile_hash != row.source_profile_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward source authority differs from its immutable row",
        ));
    }
    if row.source_profile_id == row.target_profile_id
        && row.source_profile_version == row.target_profile_version
        && row.source_profile_hash == row.target_profile_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward source and target profile authorities are identical",
        ));
    }
    verify_carry_forward_source_context(connection, &row.engagement_id, &source_authority)?;

    let (
        source_document_json,
        registered_source_mapping_hash,
        registered_source_profile_id,
        registered_source_profile_version,
        registered_source_profile_hash,
    ): (String, String, String, String, String) = connection.query_row(
        "SELECT document_json, mapping_hash, profile_id, profile_version,
                profile_hash
         FROM mapping_versions
         WHERE mapping_id = ?1 AND mapping_version = ?2",
        params![row.source_mapping_id, row.source_mapping_version],
        |record| {
            Ok((
                record.get(0)?,
                record.get(1)?,
                record.get(2)?,
                record.get(3)?,
                record.get(4)?,
            ))
        },
    )?;
    let source_value = parse_unique_json(source_document_json.as_bytes())?;
    if canonical_hash(&source_value)? != row.source_mapping_hash
        || registered_source_mapping_hash != row.source_mapping_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward source mapping canonical hash mismatch",
        ));
    }
    let source_mapping: MappingDocument = serde_json::from_value(source_value)?;
    if source_mapping.mapping_id != row.source_mapping_id
        || source_mapping.version != row.source_mapping_version
        || registered_source_profile_id != row.source_profile_id
        || registered_source_profile_version != row.source_profile_version
        || registered_source_profile_hash != row.source_profile_hash
        || source_mapping.profile.profile_id != row.source_profile_id
        || source_mapping.profile.version != row.source_profile_version
        || source_mapping.profile.sha256 != row.source_profile_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward source mapping/profile links are inconsistent",
        ));
    }
    let source_profile = crate::profile_registry::load_verified_profile_document(
        connection,
        &row.source_profile_id,
        &row.source_profile_version,
        &row.source_profile_hash,
    )?;
    let target_profile = crate::profile_registry::load_verified_profile_document(
        connection,
        &row.target_profile_id,
        &row.target_profile_version,
        &row.target_profile_hash,
    )?;
    let target_view = crate::profile_registry::inspect_profile_connection(
        connection,
        &row.target_profile_id,
        &row.target_profile_version,
        &row.target_profile_hash,
    )?;
    if target_view.authority != ProfileAuthority::LocalMappingValidation
        || target_view.approval.is_none()
    {
        return Err(destination_replay_error(
            "mapping carry-forward target profile lacks exact approval authority",
        ));
    }
    let origin: (String, String, String, i64, String) = connection.query_row(
        "SELECT origin_binding_id, origin_binding_hash,
                draft_id, draft_generation, draft_revision_hash
         FROM destination_profile_origin_bindings
         WHERE engagement_id = ?1 AND profile_id = ?2
           AND profile_version = ?3 AND profile_hash = ?4",
        params![
            row.engagement_id,
            row.target_profile_id,
            row.target_profile_version,
            row.target_profile_hash
        ],
        |record| {
            Ok((
                record.get(0)?,
                record.get(1)?,
                record.get(2)?,
                record.get(3)?,
                record.get(4)?,
            ))
        },
    )?;
    verify_origin_binding(
        connection,
        &origin.0,
        &origin.1,
        &row.target_profile_id,
        &row.target_profile_version,
        &row.target_profile_hash,
    )?;

    let carried = compute_carry_forward(&source_mapping, &source_profile, &target_profile)?;
    let snapshot = MappingDraftSnapshot {
        source_entity_assignments: carried.source_entity_assignments.clone(),
        bindings: carried.cloned_bindings.clone(),
        relationship_bindings: carried.cloned_relationships.clone(),
        reference_tables: Vec::new(),
        author: Some(row.actor.clone()),
    };
    let snapshot_value = serde_json::to_value(&snapshot)?;
    let snapshot_json = canonical_json_string(&snapshot_value)?;
    let snapshot_hash = canonical_hash(&snapshot_value)?;
    let expected_result = json!({
        "schemaVersion": DESTINATION_CARRY_FORWARD_SCHEMA,
        "algorithm": CARRY_FORWARD_ALGORITHM,
        "sourceMapping": {
            "mappingId": row.source_mapping_id,
            "mappingVersion": row.source_mapping_version,
            "mappingHash": row.source_mapping_hash,
            "mappingApprovalId": source_authority.mapping_approval_id.clone(),
            "mappingApprovalHash": source_authority.mapping_approval_hash.clone(),
            "activeJobId": source_authority.active_job_id.clone(),
            "activeSourceHash": source_authority.active_source_hash.clone(),
            "validatedJobId": source_authority.validated_job_id.clone(),
            "validatedSourceHash": source_authority.validated_source_hash.clone(),
            "profileId": row.source_profile_id,
            "profileVersion": row.source_profile_version,
            "profileHash": row.source_profile_hash,
        },
        "targetProfile": {
            "profileId": row.target_profile_id,
            "profileVersion": row.target_profile_version,
            "profileHash": row.target_profile_hash,
            "originBindingId": origin.0,
            "originBindingHash": origin.1,
            "draftId": origin.2,
            "draftGeneration": origin.3,
            "draftHash": origin.4,
        },
        "clonedBindings": carried.cloned_bindings,
        "unresolvedTargets": carried.unresolved_targets,
        "unresolvedRelationships": carried.unresolved_relationships,
        "draftHash": snapshot_hash,
    });
    if canonical_json_string(&expected_result)? != row.canonical_result_json
        || canonical_hash(&expected_result)? != row.result_hash
        || snapshot_hash != row.result_draft_hash
    {
        return Err(destination_replay_error(
            "mapping carry-forward output does not recompute from exact mapping/profile authority",
        ));
    }

    let result: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        Option<String>,
        String,
        String,
    ) = connection.query_row(
        "SELECT head.engagement_id, head.source_job_id, head.source_hash,
                head.object_pack_id, head.object_pack_version,
                head.profile_id, head.profile_version, head.profile_hash,
                revision.generation, revision.parent_revision_hash,
                revision.revision_hash, revision.canonical_draft_json
         FROM engagement_mapping_draft_heads AS head
         JOIN engagement_mapping_draft_revisions AS revision
           ON revision.draft_id = head.draft_id AND revision.generation = 1
         WHERE head.draft_id = ?1",
        [&row.result_draft_id],
        |record| {
            Ok((
                record.get(0)?,
                record.get(1)?,
                record.get(2)?,
                record.get(3)?,
                record.get(4)?,
                record.get(5)?,
                record.get(6)?,
                record.get(7)?,
                record.get(8)?,
                record.get(9)?,
                record.get(10)?,
                record.get(11)?,
            ))
        },
    )?;
    let revision_actor_time: (String, String) = connection.query_row(
        "SELECT actor, created_at FROM engagement_mapping_draft_revisions
         WHERE draft_id = ?1 AND generation = 1",
        [&row.result_draft_id],
        |record| Ok((record.get(0)?, record.get(1)?)),
    )?;
    let source_job: (String, String) = connection.query_row(
        "SELECT state, source_hash FROM jobs WHERE id = ?1",
        [&result.1],
        |record| Ok((record.get(0)?, record.get(1)?)),
    )?;
    if result.0 != row.engagement_id
        || result.1 != source_authority.active_job_id
        || result.2 != source_authority.active_source_hash
        || source_job.0 != "COMPLETE"
        || source_job.1 != result.2
        || result.3 != row.target_profile_id
        || result.4 != row.target_profile_version
        || result.5 != row.target_profile_id
        || result.6 != row.target_profile_version
        || result.7 != row.target_profile_hash
        || result.8 != 1
        || result.9.is_some()
        || result.10 != row.result_draft_hash
        || result.11 != snapshot_json
        || revision_actor_time.0 != row.actor
        || revision_actor_time.1 != row.created_at
    {
        return Err(destination_replay_error(
            "mapping carry-forward result differs from its first immutable mapping draft revision",
        ));
    }
    let linked_attempts: i64 = connection.query_row(
        "SELECT COUNT(*) FROM destination_workshop_attempts
         WHERE engagement_id = ?1
           AND command_kind = 'CARRY_FORWARD_MAPPING'
           AND result_reference = ?2 AND result_hash = ?3",
        params![row.engagement_id, row.carry_forward_id, row.result_hash],
        |record| record.get(0),
    )?;
    if linked_attempts != 1 {
        return Err(destination_replay_error(
            "mapping carry-forward lacks one exact typed workshop attempt",
        ));
    }
    Ok(())
}

pub(crate) fn verify_mapping_carry_forward_draft_origin(
    connection: &Connection,
    draft_id: &str,
    draft_hash: &str,
) -> Result<bool> {
    let mut statement = connection.prepare(
        "SELECT carry_forward_id, result_draft_hash
         FROM destination_mapping_carry_forwards
         WHERE result_draft_id = ?1
         ORDER BY carry_forward_id",
    )?;
    let rows = statement
        .query_map([draft_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let [] = rows.as_slice() else {
        if rows.len() != 1 {
            return Err(destination_replay_error(
                "mapping draft has ambiguous carry-forward origin authority",
            ));
        }
        let (carry_forward_id, result_draft_hash) = &rows[0];
        if result_draft_hash != draft_hash {
            return Err(destination_replay_error(
                "mapping draft hash differs from its carry-forward origin authority",
            ));
        }
        verify_carry_forward(connection, carry_forward_id)?;
        return Ok(true);
    };
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{
        MappingBinding, MappingDocument, MappingKind, MappingProfileIdentity,
        MappingRelationshipBinding, MappingSourceIdentity, ReferenceTable, RelationshipFieldPair,
        SourceEntityAssignment,
    };

    #[test]
    fn workspace_relative_paths_use_canonical_slashes() {
        let workspace = Path::new("workspace-root");
        let nested = workspace.join("catalogues").join("catalogue.json");

        assert_eq!(
            relative_workspace_path(workspace, &nested).expect("nested workspace-relative path"),
            "catalogues/catalogue.json"
        );
    }

    fn profile(root_technical: &str, relationship_cardinality: &str) -> Value {
        json!({
            "profileId": "synthetic.carry",
            "version": "1.0.0",
            "status": "REVIEWED_DRAFT",
            "title": "Carry-forward profile",
            "owner": "test",
            "technicalSources": [],
            "entities": [
                {
                    "entityId": "root",
                    "root": true,
                    "technicalEntity": root_technical,
                    "key": ["id"],
                    "fields": [{
                        "fieldId": "id",
                        "label": "ID",
                        "logicalType": "identifier",
                        "required": true,
                        "sensitive": false,
                        "technicalProperty": format!("{root_technical}.Id")
                    }]
                },
                {
                    "entityId": "item",
                    "root": false,
                    "technicalEntity": "Synthetic.Item",
                    "key": ["parentId"],
                    "fields": [{
                        "fieldId": "parentId",
                        "label": "Parent ID",
                        "logicalType": "identifier",
                        "required": true,
                        "sensitive": false,
                        "technicalProperty": "Synthetic.Item.ParentId"
                    }]
                }
            ],
            "relationships": [
                {
                    "relationshipId": "root-items",
                    "from": "root",
                    "to": "item",
                    "cardinality": relationship_cardinality,
                    "keyPairs": [{"fromFieldId": "id", "toFieldId": "parentId"}]
                },
                {
                    "relationshipId": "new-review-link",
                    "from": "root",
                    "to": "item",
                    "cardinality": "0..*",
                    "keyPairs": [{"fromFieldId": "id", "toFieldId": "parentId"}]
                }
            ],
            "rules": [],
            "publicationAdapters": []
        })
    }

    #[test]
    fn carry_forward_drops_changed_entity_routes_lookup_dependencies_and_relationships() {
        let source_profile = profile("Synthetic.Root", "1..*");
        let mut source_profile = source_profile;
        source_profile
            .get_mut("relationships")
            .and_then(Value::as_array_mut)
            .expect("source relationships")
            .pop();
        let target_profile = profile("Synthetic.RenamedRoot", "0..*");
        let source_mapping = MappingDocument {
            schema_version: 2,
            mapping_id: "synthetic-carry-mapping".to_owned(),
            version: "1.0.0".to_owned(),
            profile: MappingProfileIdentity {
                profile_id: "synthetic.carry".to_owned(),
                version: "1.0.0".to_owned(),
                sha256: "0".repeat(64),
            },
            source: MappingSourceIdentity {
                package_sha256: "1".repeat(64),
                source_files: vec!["root.csv".to_owned(), "item.csv".to_owned()],
            },
            root_entity_id: "root".to_owned(),
            source_entity_assignments: vec![
                SourceEntityAssignment {
                    source_file: "root.csv".to_owned(),
                    entity_id: "root".to_owned(),
                },
                SourceEntityAssignment {
                    source_file: "item.csv".to_owned(),
                    entity_id: "item".to_owned(),
                },
            ],
            bindings: vec![
                MappingBinding {
                    binding_id: "root-id".to_owned(),
                    target_entity_id: Some("root".to_owned()),
                    source_file: Some("root.csv".to_owned()),
                    source_column: Some("case_ref".to_owned()),
                    source_column_ordinal: Some(0),
                    target_field_id: "id".to_owned(),
                    kind: MappingKind::Direct,
                    constant: None,
                    lookup_table_id: None,
                },
                MappingBinding {
                    binding_id: "item-parent".to_owned(),
                    target_entity_id: Some("item".to_owned()),
                    source_file: Some("item.csv".to_owned()),
                    source_column: Some("case_link".to_owned()),
                    source_column_ordinal: Some(0),
                    target_field_id: "parentId".to_owned(),
                    kind: MappingKind::Lookup,
                    constant: None,
                    lookup_table_id: Some("parent-map".to_owned()),
                },
            ],
            reference_tables: vec![ReferenceTable {
                table_id: "parent-map".to_owned(),
                version: "1.0.0".to_owned(),
                entity_id: Some("item".to_owned()),
                values: BTreeMap::from([("A".to_owned(), "B".to_owned())]),
            }],
            relationship_bindings: vec![MappingRelationshipBinding {
                relationship_id: "root-items".to_owned(),
                from_entity_id: "root".to_owned(),
                to_entity_id: "item".to_owned(),
                cardinality: "1..*".to_owned(),
                key_pairs: vec![RelationshipFieldPair {
                    from_field_id: "id".to_owned(),
                    to_field_id: "parentId".to_owned(),
                }],
            }],
            author: Some("test".to_owned()),
            reviewer: Some("test".to_owned()),
        };

        let carried = compute_carry_forward(&source_mapping, &source_profile, &target_profile)
            .expect("bounded carry-forward");
        assert_eq!(
            carried.source_entity_assignments,
            vec![SourceEntityAssignment {
                source_file: "item.csv".to_owned(),
                entity_id: "item".to_owned(),
            }],
            "same canonical entity ID must not preserve routing after technical/root identity drift"
        );
        assert_eq!(
            carried.cloned_bindings,
            [] as [crate::mapping::MappingBinding; 0]
        );
        assert_eq!(
            carried.unresolved_targets,
            vec![
                ("item".to_owned(), "parentId".to_owned()),
                ("root".to_owned(), "id".to_owned()),
            ]
        );
        assert_eq!(
            carried.unresolved_relationships,
            vec![
                UnresolvedCarryForwardRelationshipView {
                    relationship_id: "new-review-link".to_owned(),
                    reason: "RELATIONSHIP_NOT_PRESENT".to_owned(),
                },
                UnresolvedCarryForwardRelationshipView {
                    relationship_id: "root-items".to_owned(),
                    reason: "RELATIONSHIP_CHANGED".to_owned(),
                },
            ]
        );
    }
}

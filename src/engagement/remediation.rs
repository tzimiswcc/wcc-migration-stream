//! Governed, local-only remediation-pack authority and publication.
//!
//! Broad reads in this module intentionally contain no received values. Values
//! selected by an operator are streamed only into one verified local ZIP and
//! are never persisted in `SQLite`.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::EngagementService;
use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json, sha256_hex},
    mapping::{MappingDocument, MappingKind, load_stored_mapping},
    state::{now_rfc3339, sqlite_integer},
};

pub const REMEDIATION_PACK_OPTIONS_SCHEMA: &str =
    "migration-stream/engagement/remediation-pack-options/v1";
pub const CREATE_REMEDIATION_PACK_COMMAND_SCHEMA: &str =
    "migration-stream/engagement/write/create-remediation-pack/v1";
pub const REMEDIATION_PACK_SCHEMA: &str = "migration-stream/engagement/remediation-pack/v1";
pub const REMEDIATION_PACK_HISTORY_SCHEMA: &str =
    "migration-stream/engagement/remediation-pack-history/v1";
pub const LOCAL_REMEDIATION_DATA_EXPORT_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-remediation-data-export/v1";
pub const LOCAL_REMEDIATION_OFFENDING_VALUES_EXPORT_ACKNOWLEDGEMENT: &str =
    "migration-stream/local-remediation-offending-values-export/v1";

const MAPPING_AUTHORITY_SCHEMA: &str = "migration-stream/engagement/mapping-authority/v1";
const REMEDIATION_PACK_AUTHORITY_SCHEMA: &str =
    "migration-stream/engagement/remediation-pack-authority/v1";
const REMEDIATION_PACK_RECORD_SCHEMA: &str =
    "migration-stream/engagement/remediation-pack-record/v1";
const REMEDIATION_ISSUE_MEMBER_SCHEMA: &str =
    "migration-stream/engagement/remediation-issue-member/v1";
const MAXIMUM_SELECTIONS: usize = 128;
const MAXIMUM_ISSUES: usize = 100_000;
const MAXIMUM_VALUE_BYTES: usize = 64 * 1024;
const MAXIMUM_TOTAL_EXPORTED_VALUE_BYTES: usize = 128 * 1024 * 1024;
const MAXIMUM_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_HISTORY_ITEMS: usize = 200;
const REMEDIATION_DIRECTORY: &str = "engagement/remediation-packs";

#[cfg(test)]
thread_local! {
    static ARCHIVE_WRITE_FAILURE_AFTER_MEMBER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn inject_archive_write_failure(member_index: usize) -> Result<()> {
    let should_fail = ARCHIVE_WRITE_FAILURE_AFTER_MEMBER
        .with(|checkpoint| checkpoint.replace(None) == Some(member_index));
    if should_fail {
        return Err(KernelError::IntegrityFailed(
            "injected remediation archive partial-write failure".to_owned(),
        ));
    }
    Ok(())
}

const REMEDIATION_WORKSPACE_INSERT_GUARD: &str =
    "CREATE TRIGGER engagement_remediation_pack_attempts_workspace_insert_guard
     BEFORE INSERT ON engagement_remediation_pack_attempts
     WHEN NOT EXISTS (
         SELECT 1
         FROM object_workspace_bindings AS workspace
         JOIN object_workspace_engagement_bindings AS workspace_engagement
           ON workspace_engagement.workspace_id = workspace.workspace_id
         WHERE workspace.workspace_id = NEW.workspace_id
           AND workspace.descriptor_hash = NEW.workspace_descriptor_hash
           AND workspace_engagement.engagement_id = NEW.engagement_id
     )
     BEGIN
         SELECT RAISE(ABORT, 'remediation attempt differs from exact object workspace authority');
     END";
const V14_SCHEMA_AUTHORITIES: &[(&str, &str, &str)] = &[
    (
        "index",
        "engagement_mapping_authorities_history",
        "a370b0d32963d8ee7162d1aed98b9cd121b6b1a70d03454370d3ce96bbeed40a",
    ),
    (
        "index",
        "engagement_remediation_issue_members_history",
        "ac83db30a8eed9b166b6ed8d0ceaa29d894b4e884f47661cefc99fe04d7446b5",
    ),
    (
        "index",
        "engagement_remediation_packs_history",
        "6b9203318254149a90b6f2feea76c41a40d453eb7c01746cca4c16b12a766d36",
    ),
    (
        "trigger",
        "engagement_mapping_authorities_exact_insert_guard",
        "ad4a3fd82e655ce53251530a494ab9e88b09c2866ce87126598f9925c3a1e72f",
    ),
    (
        "trigger",
        "engagement_mapping_authorities_no_delete",
        "2907382ce8f4de99881e66633e2f40e83c3b58bf756caa41722a57a470b3e2cd",
    ),
    (
        "trigger",
        "engagement_mapping_authorities_no_update",
        "5a23f84d775818fa91bb6457d023a6acd5a7634ab811158d5496af1ac8b3b20a",
    ),
    (
        "trigger",
        "engagement_mapping_reuse_authority_insert_guard",
        "1018af661e1f0c1f4d0c93b5151b7d913a68202516ca558a2524fa477b50269c",
    ),
    (
        "trigger",
        "engagement_remediation_issue_members_exact_insert_guard",
        "c07bfa241a6ea0e8dc2610d488957361cf7455a17867fb8b4e737c5bfa67eb7a",
    ),
    (
        "trigger",
        "engagement_remediation_issue_members_no_delete",
        "a3d224ad084b88d4f398ebe9b9b69b7dbdaa81c726c5af8c2d19a546014b74c2",
    ),
    (
        "trigger",
        "engagement_remediation_issue_members_no_update",
        "e37b9f2bd3373a4f6e0c0822eba8ff16b60c77bb8152ccc7bfa56fa883b2cbf7",
    ),
    (
        "trigger",
        "engagement_remediation_pack_attempts_exact_insert_guard",
        "8de6e7c0c9ab4f251d8667e7a0ae3f9d17486c63188638cd2a3232ced2b5790f",
    ),
    (
        "trigger",
        "engagement_remediation_pack_attempts_no_delete",
        "f36611d170e57ab9413ed1760f026d606bef79ab852e952de57bf82989020290",
    ),
    (
        "trigger",
        "engagement_remediation_pack_attempts_no_update",
        "524cf164250140af105b0fe241b80f8f4d342474a49deaa91c89126a96268905",
    ),
    (
        "trigger",
        "engagement_remediation_pack_members_no_delete",
        "3ad70544c8730a76a5691dc7b0f408f65cc150a8e0a659de263057c2e1e88b0e",
    ),
    (
        "trigger",
        "engagement_remediation_pack_members_no_update",
        "e57bcd3d4b604cd4f49a86b7336259fb5e96164f0971804f888e7b6ad17c7470",
    ),
    (
        "trigger",
        "engagement_remediation_pack_results_no_delete",
        "2658c23b172c1913e07cca4f8dd3e59191563a656547302e3590292929de43c7",
    ),
    (
        "trigger",
        "engagement_remediation_pack_results_no_update",
        "a09305c4e1460ac96b0c0443aebd41ebf8ed20822ce846121093f13b7c874c02",
    ),
    (
        "trigger",
        "engagement_remediation_pack_selections_no_delete",
        "236b3e469613fe21b8218f5a74fc2a4d10836c6bce6366e86994c75c16597be2",
    ),
    (
        "trigger",
        "engagement_remediation_pack_selections_no_update",
        "9c31e669ccdd65e6a298ce2ad80ae3d73be89b57aa57f95056290d552c775ff2",
    ),
    (
        "trigger",
        "engagement_remediation_packs_exact_insert_guard",
        "76bf1d1a00e22134c87564ce8da77a2a74fef0b40f1b4c252726dc3315c82ea7",
    ),
    (
        "trigger",
        "engagement_remediation_packs_no_delete",
        "6539e800e8719169753a1ca6fd7f0ca7b41853e124926967ad3fa60d6ebe3f90",
    ),
    (
        "trigger",
        "engagement_remediation_packs_no_update",
        "a1bdd72135e3bed68a059bd4c55c1eac833a45ce222096850e4561ac18c464c9",
    ),
    (
        "trigger",
        "engagement_tours_mapping_authority_insert_guard",
        "6fddd780b639dc5b7d74bf3cbaeba50a6ce797aa72518999bc0861b148bee95c",
    ),
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationKeySelection {
    pub entity_id: String,
    pub source_file: String,
    pub source_column_ordinal: usize,
    pub source_column: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationColumnOptionView {
    pub source_column_ordinal: usize,
    pub source_column: String,
    pub suggested: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationFileOptionView {
    pub entity_id: String,
    pub source_file: String,
    pub columns: Vec<RemediationColumnOptionView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationPackOptionsView {
    pub schema_version: String,
    pub engagement_id: String,
    pub engagement_generation: u64,
    pub workspace_id: String,
    pub workspace_descriptor_hash: String,
    pub job_id: String,
    pub source_hash: String,
    pub manifest_hash: String,
    pub profiling_run_id: String,
    pub profiling_result_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub profile_approval_id: String,
    pub profile_approval_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub mapping_authority_id: String,
    pub mapping_authority_hash: String,
    pub validation_run_id: String,
    pub validation_result_hash: String,
    pub issue_count: u64,
    pub options_hash: String,
    pub files: Vec<RemediationFileOptionView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRemediationPackCommand {
    pub schema_version: String,
    pub expected_engagement_generation: u64,
    pub expected_validation_run_id: String,
    pub expected_options_hash: String,
    pub selected_key_columns: Vec<RemediationKeySelection>,
    pub actor: String,
    pub reason: String,
    pub local_data_export_acknowledgement: String,
    #[serde(default)]
    pub include_offending_values: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offending_values_acknowledgement: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationPackView {
    pub pack_id: String,
    pub state: String,
    pub engagement_id: String,
    pub validation_run_id: String,
    pub source_hash: String,
    pub selected_key_columns: Vec<RemediationKeySelection>,
    pub include_offending_values: bool,
    pub actor: String,
    pub reason: String,
    pub issue_count: u64,
    pub member_count: u64,
    pub archive_size_bytes: u64,
    pub archive_sha256: String,
    pub membership_hash: String,
    pub authority_hash: String,
    pub download_token: String,
    pub created_at: String,
    pub completed_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemediationPackHistoryView {
    pub schema_version: String,
    pub total_count: u64,
    pub returned_count: u64,
    pub truncated: bool,
    pub items: Vec<RemediationPackView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRemediationPackResult {
    pub schema_version: String,
    pub pack: RemediationPackView,
    pub reused_attempt: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MappingAuthorityIdentity {
    pub authority_id: String,
    pub authority_hash: String,
    pub canonical_json: String,
    pub authority_kind: String,
    pub mapping_approval_id: Option<String>,
    pub mapping_approval_hash: Option<String>,
    pub preparation_id: Option<String>,
    pub mapping_validation_attempt_id: Option<String>,
    pub mapping_validation_attempt_hash: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug)]
struct ActiveExportAuthority {
    options: RemediationPackOptionsView,
    mapping_authority: MappingAuthorityIdentity,
}

#[derive(Clone, Debug)]
struct ValidationEvidenceIdentity {
    engagement_id: String,
    job_id: String,
    source_hash: String,
    manifest_hash: String,
    profiling_run_id: String,
    profiling_result_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    validation_run_id: String,
    validation_result_hash: String,
}

#[derive(Clone, Debug)]
struct ActiveValidationEvidence {
    identity: ValidationEvidenceIdentity,
    engagement_generation: i64,
    workspace_id: String,
    workspace_descriptor_hash: String,
    issue_count: i64,
}

#[derive(Clone, Debug)]
struct ValidationAuthorityBinding {
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_authority: MappingAuthorityIdentity,
}

#[derive(Clone, Debug)]
struct RegisteredValidationAuthority {
    tour_id: String,
    tour_number: i64,
    source_reuse_id: Option<String>,
    mapping_origin_source_hash: Option<String>,
    identity: ValidationEvidenceIdentity,
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_authority_id: Option<String>,
    mapping_authority_hash: Option<String>,
}

#[derive(Clone, Debug)]
struct IssueRow {
    issue_id: String,
    issue_hash: String,
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

#[derive(Clone, Debug)]
struct IssueMember {
    issue: IssueRow,
    lineage_kind: &'static str,
    offending_ordinal: Option<usize>,
    offending_column: Option<String>,
    canonical_json: String,
    member_hash: String,
}

#[derive(Clone, Debug)]
struct AttemptIntegrityRow {
    planned_pack_id: String,
    engagement_id: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
    expected_generation: i64,
    workspace_id: String,
    workspace_descriptor_hash: String,
    job_id: String,
    source_hash: String,
    manifest_hash: String,
    profiling_run_id: String,
    profiling_result_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    mapping_authority_id: String,
    mapping_authority_hash: String,
    validation_run_id: String,
    validation_result_hash: String,
    options_hash: String,
    actor: String,
    reason: String,
    local_export_acknowledgement: String,
    include_offending_values: i64,
    offending_values_acknowledgement: Option<String>,
    canonical_authority_json: String,
    authority_hash: String,
    created_at: String,
}

#[derive(Clone, Debug)]
struct PackIntegrityRow {
    attempt_id: String,
    engagement_id: String,
    validation_run_id: String,
    source_hash: String,
    selected_columns_json: String,
    selected_columns_hash: String,
    include_offending_values: i64,
    actor: String,
    reason: String,
    issue_count: i64,
    membership_hash: String,
    member_count: i64,
    archive_relative_path: String,
    archive_size: i64,
    archive_sha256: String,
    manifest_hash: String,
    canonical_record_json: String,
    record_hash: String,
    completed_at: String,
}

#[derive(Clone, Debug)]
struct RemediationTerminalResultRow {
    attempt_id: String,
    outcome: String,
    pack_id: Option<String>,
    result_hash: String,
    error_code: Option<String>,
    completed_at: String,
}

#[derive(Clone, Debug)]
struct IssueMemberIntegrityRow {
    issue_id: String,
    issue_hash: String,
    severity: String,
    rule_id: String,
    issue_code: String,
    display_message: String,
    entity_id: String,
    field_id: String,
    source_file: String,
    source_ordinal: i64,
    line_start: i64,
    line_end: i64,
    offending_lineage_kind: String,
    offending_source_column_ordinal: Option<i64>,
    offending_source_column: Option<String>,
    canonical_member_json: String,
    member_hash: String,
}

#[derive(Clone, Debug)]
struct DatabaseArchiveMember {
    media_type: String,
    row_count: Option<i64>,
    byte_size: usize,
    sha256: String,
}

#[derive(Clone, Debug)]
struct ArchiveMember {
    filename: &'static str,
    media_type: &'static str,
    bytes: Vec<u8>,
    row_count: Option<u64>,
}

#[derive(Debug)]
struct PublishedArchive {
    relative_path: String,
    archive_sha256: String,
    archive_size: u64,
    manifest_hash: String,
    members: Vec<ArchiveMember>,
    cleanup: Option<OwnedArchiveCleanup>,
}

#[derive(Debug)]
struct OwnedArchiveCleanup {
    paths: Vec<PathBuf>,
    file: Option<File>,
    armed: bool,
}

impl OwnedArchiveCleanup {
    fn new(path: PathBuf, file: File) -> Self {
        Self {
            paths: vec![path],
            file: Some(file),
            armed: true,
        }
    }

    fn add_candidate(&mut self, path: PathBuf) {
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedArchiveCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let owned_paths = self.file.as_ref().map_or_else(Vec::new, |file| {
            self.paths
                .iter()
                .filter(|path| named_file_matches(path, file).unwrap_or(false))
                .cloned()
                .collect::<Vec<_>>()
        });
        // Windows may reject deletion while the retained identity handle is
        // open. Resolve exact owned names first, then close the handle before
        // removing only those names.
        drop(self.file.take());
        for path in owned_paths {
            let _ = fs::remove_file(path);
        }
    }
}

impl PublishedArchive {
    /// Keep a newly published data-bearing archive only after its exact
    /// immutable database graph commits. Any error returned by `registration`
    /// drops the still-armed owned-file guard before it can escape untracked.
    fn complete_registration(
        mut self,
        registration: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        registration(&self)?;
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.disarm();
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RemediationIntegritySummary {
    pub mapping_authorities: usize,
    pub attempts: usize,
    pub packs: usize,
    pub members: usize,
    pub issue_members: usize,
}

impl EngagementService {
    /// Return value-free key-selection options for the exact active completed
    /// validation. Suggestions remain unchecked and have no authority.
    pub fn remediation_pack_options(&self) -> Result<RemediationPackOptionsView> {
        Ok(load_active_export_authority(&self.open_read()?)?.options)
    }

    /// Build and publish one exact local-only remediation ZIP.
    #[allow(clippy::too_many_lines)]
    pub fn create_remediation_pack(
        &self,
        command: &CreateRemediationPackCommand,
    ) -> Result<CreateRemediationPackResult> {
        validate_command(command)?;
        let canonical_request = canonical_json_string(&serde_json::to_value(command)?)?;
        if canonical_request.len() > 2 * 1024 * 1024 {
            return Err(KernelError::PolicyInvalid(
                "remediation command exceeds its canonical byte bound".to_owned(),
            ));
        }
        let request_hash = sha256_hex(canonical_request.as_bytes());

        // A completed idempotent command remains replayable after a later
        // source revision becomes active. Verify that exact immutable attempt
        // and archive before consulting today's active options. A different
        // command under the same key still conflicts independently of the
        // active revision.
        let writer = self.open_writer()?;
        ensure_workspace_insert_guard(writer.connection())?;
        let engagement_id = super::require_engagement_id(writer.connection())?;
        let existing = load_attempt_by_idempotency(
            writer.connection(),
            &engagement_id,
            &command.idempotency_key,
        )?;
        if let Some(existing) = existing.as_ref() {
            if existing.1 != request_hash {
                return Err(KernelError::Conflict(
                    "remediation idempotency key was used for a different command".to_owned(),
                ));
            }
            if let Some(view) = load_pack_for_attempt(writer.connection(), &existing.0)? {
                verify_attempt_row(writer.connection(), &existing.0)?;
                verify_pack_row(&self.workspace, writer.connection(), &view.pack_id)?;
                return Ok(CreateRemediationPackResult {
                    schema_version: REMEDIATION_PACK_SCHEMA.to_owned(),
                    pack: view,
                    reused_attempt: true,
                });
            }
        }

        let authority = load_active_export_authority(writer.connection())?;
        if command.expected_engagement_generation != authority.options.engagement_generation
            || command.expected_validation_run_id != authority.options.validation_run_id
            || command.expected_options_hash != authority.options.options_hash
        {
            return Err(KernelError::Conflict(
                "remediation export differs from the exact active validation/options authority"
                    .to_owned(),
            ));
        }
        let selections = validate_selections(&authority.options, &command.selected_key_columns)?;
        let (attempt_id, pack_id, created_at, authority_hash, reused_attempt) =
            if let Some(existing) = existing {
                let planned = load_planned_attempt(writer.connection(), &existing.0)?;
                verify_attempt_row(writer.connection(), &existing.0)?;
                if planned.4 != selections
                    || planned.5 != authority.options.options_hash
                    || planned.6 != authority.options.mapping_authority_id
                    || planned.7 != authority.options.mapping_authority_hash
                {
                    return Err(KernelError::IntegrityFailed(
                        "planned remediation retry differs from exact options/selections authority"
                            .to_owned(),
                    ));
                }
                (existing.0, planned.0, planned.1, planned.2, true)
            } else {
                let attempt_id = Uuid::new_v4().to_string();
                let pack_id = Uuid::new_v4().to_string();
                let created_at = now_rfc3339();
                let authority_value = remediation_attempt_authority_value(
                    &attempt_id,
                    &pack_id,
                    &authority.options,
                    command,
                    &selections,
                    &request_hash,
                    &created_at,
                );
                let canonical_authority = canonical_json_string(&authority_value)?;
                let authority_hash = canonical_hash(&authority_value)?;
                let transaction = writer.connection().unchecked_transaction()?;
                insert_mapping_authority(
                    &transaction,
                    &authority.options,
                    &authority.mapping_authority,
                )?;
                insert_remediation_attempt(
                    &transaction,
                    &attempt_id,
                    &pack_id,
                    &authority.options,
                    command,
                    &request_hash,
                    &canonical_request,
                    &canonical_authority,
                    &authority_hash,
                    &created_at,
                )?;
                for selection in &selections {
                    let value = serde_json::to_value(selection)?;
                    let canonical = canonical_json_string(&value)?;
                    let hash = canonical_hash(&value)?;
                    transaction.execute(
                        "INSERT INTO engagement_remediation_pack_selections (
                             attempt_id, entity_id, source_file, source_column_ordinal,
                             source_column, canonical_selection_json, selection_hash
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            attempt_id,
                            selection.entity_id,
                            selection.source_file,
                            sqlite_integer(
                                u64::try_from(selection.source_column_ordinal).unwrap_or(u64::MAX),
                                "remediation source-column ordinal"
                            )?,
                            selection.source_column,
                            canonical,
                            hash,
                        ],
                    )?;
                }
                transaction.commit()?;
                (attempt_id, pack_id, created_at, authority_hash, false)
            };
        // Keep the exact Object Workspace writer lock across source reads,
        // archive publication, and immutable DB registration. A concurrent
        // retry cannot remove or replace this attempt's owned partial/archive.
        let issues = load_complete_issue_members(writer.connection(), &authority.options)?;
        let archive = build_and_publish_archive(
            &self.workspace,
            &pack_id,
            &created_at,
            &authority.options,
            command,
            &selections,
            &issues,
            writer.connection(),
        )?;
        archive.complete_registration(|archive| {
            let transaction = writer.connection().unchecked_transaction()?;
            let current = load_active_export_authority(&transaction)?;
            if current.options != authority.options {
                return Err(KernelError::Conflict(
                    "active validation authority changed before remediation publication completed"
                        .to_owned(),
                ));
            }
            persist_completed_pack(
                &transaction,
                &pack_id,
                &attempt_id,
                &authority.options,
                command,
                &selections,
                &issues,
                archive,
                &authority_hash,
                &created_at,
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        let pack = load_pack_for_attempt(writer.connection(), &attempt_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "completed remediation pack disappeared after publication".to_owned(),
            )
        })?;
        Ok(CreateRemediationPackResult {
            schema_version: REMEDIATION_PACK_SCHEMA.to_owned(),
            pack,
            reused_attempt,
        })
    }

    /// List bounded, value-free immutable remediation history.
    pub fn remediation_pack_history(&self) -> Result<RemediationPackHistoryView> {
        let connection = self.open_read()?;
        let engagement_id: String = connection.query_row(
            "SELECT engagement_id FROM engagement_records WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let total_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM engagement_remediation_pack_attempts AS attempt
             JOIN engagement_remediation_packs AS pack
               ON pack.attempt_id = attempt.attempt_id
             JOIN engagement_remediation_pack_results AS result
               ON result.attempt_id = attempt.attempt_id
              AND result.outcome = 'COMPLETE' AND result.pack_id = pack.pack_id
             WHERE attempt.engagement_id = ?1",
            [&engagement_id],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT attempt.attempt_id
             FROM engagement_remediation_pack_attempts AS attempt
             JOIN engagement_remediation_packs AS pack
               ON pack.attempt_id = attempt.attempt_id
             JOIN engagement_remediation_pack_results AS result
               ON result.attempt_id = attempt.attempt_id
              AND result.outcome = 'COMPLETE' AND result.pack_id = pack.pack_id
             WHERE attempt.engagement_id = ?1
             ORDER BY attempt.created_at DESC, attempt.attempt_id DESC LIMIT ?2",
        )?;
        let attempt_ids = statement
            .query_map(
                params![
                    engagement_id,
                    i64::try_from(MAXIMUM_HISTORY_ITEMS).unwrap_or(i64::MAX)
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut items = Vec::new();
        for attempt_id in attempt_ids {
            verify_attempt_row(&connection, &attempt_id)?;
            if let Some(pack) = load_verified_pack_summary(&connection, &attempt_id)? {
                items.push(pack);
            }
        }
        let total_count = u64::try_from(total_count).map_err(|_| {
            KernelError::IntegrityFailed("remediation history count is negative".to_owned())
        })?;
        let returned_count = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(RemediationPackHistoryView {
            schema_version: REMEDIATION_PACK_HISTORY_SCHEMA.to_owned(),
            total_count,
            returned_count,
            truncated: returned_count < total_count,
            items,
        })
    }

    /// Read one exact verified server-owned archive for a download response.
    ///
    /// Verification and reading use the same retained file handle, closing the
    /// pathname race that would exist if an HTTP handler reopened a `PathBuf`.
    pub fn remediation_pack_download(
        &self,
        pack_id: &str,
    ) -> Result<(RemediationPackView, Vec<u8>)> {
        validate_text(pack_id, "remediation pack ID", 256)?;
        let connection = self.open_read()?;
        let row: Option<(String, String, i64, String)> = connection
            .query_row(
                "SELECT pack.archive_relative_path, pack.archive_sha256,
                        pack.archive_size, pack.attempt_id
                 FROM engagement_remediation_packs AS pack WHERE pack.pack_id = ?1",
                [pack_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((relative, expected_hash, expected_size, attempt_id)) = row else {
            return Err(KernelError::NotFound(
                "remediation pack is not available".to_owned(),
            ));
        };
        verify_attempt_row(&connection, &attempt_id)?;
        verify_pack_row(&self.workspace, &connection, pack_id)?;
        let path = checked_archive_path(&self.workspace, &relative)?;
        let bytes = read_verified_archive(
            &path,
            &expected_hash,
            u64::try_from(expected_size).unwrap_or(u64::MAX),
        )?;
        let view = load_pack_for_attempt(&connection, &attempt_id)?.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "downloadable remediation pack has no completed read model".to_owned(),
            )
        })?;
        Ok((view, bytes))
    }
}

fn normalized_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The core schema is also created while an Object Workspace establishment is
/// deliberately pending, before the final binding tables exist. Install this
/// exact cross-workspace guard lazily under the already-active workspace
/// writer so fresh initialization remains possible without weakening writes.
fn ensure_workspace_insert_guard(connection: &Connection) -> Result<()> {
    let actual: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger'
               AND name = 'engagement_remediation_pack_attempts_workspace_insert_guard'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match actual {
        Some(actual)
            if normalized_sql(&actual) == normalized_sql(REMEDIATION_WORKSPACE_INSERT_GUARD) =>
        {
            Ok(())
        }
        Some(_) => Err(KernelError::IntegrityFailed(
            "remediation object-workspace insert guard is altered".to_owned(),
        )),
        None => {
            let tables_exist: bool = connection.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'object_workspace_bindings'
                 ) AND EXISTS(
                     SELECT 1 FROM sqlite_schema
                     WHERE type = 'table' AND name = 'object_workspace_engagement_bindings'
                 )",
                [],
                |row| row.get(0),
            )?;
            if !tables_exist {
                return Err(KernelError::IntegrityFailed(
                    "remediation export requires the exact active Object Workspace binding"
                        .to_owned(),
                ));
            }
            connection.execute_batch(REMEDIATION_WORKSPACE_INSERT_GUARD)?;
            ensure_workspace_insert_guard(connection)
        }
    }
}

fn verify_v14_schema_authorities(
    connection: &Connection,
    require_workspace_guard: bool,
) -> Result<()> {
    for (kind, name, expected_hash) in V14_SCHEMA_AUTHORITIES {
        let sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = ?1 AND name = ?2",
                params![kind, name],
                |row| row.get(0),
            )
            .optional()?;
        let actual_hash = sql
            .as_deref()
            .map(normalized_sql)
            .map(|normalized| sha256_hex(normalized.as_bytes()));
        if actual_hash.as_deref() != Some(*expected_hash) {
            return Err(KernelError::IntegrityFailed(format!(
                "v14 schema authority {kind} {name} is missing or altered"
            )));
        }
    }
    let workspace_guard: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger'
               AND name = 'engagement_remediation_pack_attempts_workspace_insert_guard'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(sql) = workspace_guard.as_deref()
        && normalized_sql(sql) != normalized_sql(REMEDIATION_WORKSPACE_INSERT_GUARD)
    {
        return Err(KernelError::IntegrityFailed(
            "remediation workspace insert guard is altered".to_owned(),
        ));
    }
    if require_workspace_guard && workspace_guard.is_none() {
        return Err(KernelError::IntegrityFailed(
            "remediation workspace insert guard is missing".to_owned(),
        ));
    }
    Ok(())
}

fn validate_command(command: &CreateRemediationPackCommand) -> Result<()> {
    if command.schema_version != CREATE_REMEDIATION_PACK_COMMAND_SCHEMA {
        return Err(KernelError::PolicyInvalid(format!(
            "remediation command schema must be {CREATE_REMEDIATION_PACK_COMMAND_SCHEMA:?}"
        )));
    }
    validate_text(
        &command.expected_validation_run_id,
        "validation run ID",
        256,
    )?;
    validate_sha(&command.expected_options_hash, "remediation options hash")?;
    validate_text(&command.actor, "remediation actor", 256)?;
    validate_text(&command.reason, "remediation reason", 4_096)?;
    validate_text(&command.idempotency_key, "remediation idempotency key", 128)?;
    if command.idempotency_key.len() < 16 {
        return Err(KernelError::PolicyInvalid(
            "remediation idempotency key must contain at least 16 bytes".to_owned(),
        ));
    }
    if command.local_data_export_acknowledgement != LOCAL_REMEDIATION_DATA_EXPORT_ACKNOWLEDGEMENT {
        return Err(KernelError::PolicyInvalid(format!(
            "remediation export requires acknowledgement {LOCAL_REMEDIATION_DATA_EXPORT_ACKNOWLEDGEMENT:?}"
        )));
    }
    match (
        command.include_offending_values,
        command.offending_values_acknowledgement.as_deref(),
    ) {
        (false, None) => {}
        (true, Some(value))
            if value == LOCAL_REMEDIATION_OFFENDING_VALUES_EXPORT_ACKNOWLEDGEMENT => {}
        _ => {
            return Err(KernelError::PolicyInvalid(format!(
                "offending-value export requires the separate acknowledgement {LOCAL_REMEDIATION_OFFENDING_VALUES_EXPORT_ACKNOWLEDGEMENT:?} only when enabled"
            )));
        }
    }
    if command.selected_key_columns.is_empty()
        || command.selected_key_columns.len() > MAXIMUM_SELECTIONS
    {
        return Err(KernelError::PolicyInvalid(format!(
            "remediation export requires 1..={MAXIMUM_SELECTIONS} selected key columns"
        )));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.trim() != value || value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be nonempty, trimmed, NUL-free, and at most {maximum} bytes"
        )));
    }
    Ok(())
}

fn validate_sha(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be a lowercase SHA-256 digest"
        )));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must use lowercase hexadecimal"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn load_active_export_authority(connection: &Connection) -> Result<ActiveExportAuthority> {
    let active = load_active_validation_evidence(connection)?;
    let identity = &active.identity;
    crate::validation::verify_validation_run_for_read(connection, &identity.validation_run_id)?;
    let stored_mapping =
        load_stored_mapping(connection, &identity.mapping_id, &identity.mapping_version)?;
    if stored_mapping.mapping_hash != identity.mapping_hash
        || stored_mapping.document.profile.profile_id != identity.profile_id
        || stored_mapping.document.profile.version != identity.profile_version
        || stored_mapping.document.profile.sha256 != identity.profile_hash
    {
        return Err(KernelError::IntegrityFailed(
            "remediation mapping differs from active validation authority".to_owned(),
        ));
    }
    let authority = replay_validation_authority(connection, identity, true)?;
    let profile = crate::profile_registry::load_verified_profile_document(
        connection,
        &identity.profile_id,
        &identity.profile_version,
        &identity.profile_hash,
    )?;
    let files = remediation_file_options(
        connection,
        &identity.job_id,
        &stored_mapping.document,
        &profile,
    )?;
    let mut options = RemediationPackOptionsView {
        schema_version: REMEDIATION_PACK_OPTIONS_SCHEMA.to_owned(),
        engagement_id: identity.engagement_id.clone(),
        engagement_generation: u64::try_from(active.engagement_generation).map_err(|_| {
            KernelError::IntegrityFailed("engagement generation is negative".to_owned())
        })?,
        workspace_id: active.workspace_id,
        workspace_descriptor_hash: active.workspace_descriptor_hash,
        job_id: identity.job_id.clone(),
        source_hash: identity.source_hash.clone(),
        manifest_hash: identity.manifest_hash.clone(),
        profiling_run_id: identity.profiling_run_id.clone(),
        profiling_result_hash: identity.profiling_result_hash.clone(),
        profile_id: identity.profile_id.clone(),
        profile_version: identity.profile_version.clone(),
        profile_hash: identity.profile_hash.clone(),
        profile_approval_id: authority.profile_approval_id,
        profile_approval_hash: authority.profile_approval_hash,
        mapping_id: identity.mapping_id.clone(),
        mapping_version: identity.mapping_version.clone(),
        mapping_hash: identity.mapping_hash.clone(),
        mapping_authority_id: authority.mapping_authority.authority_id.clone(),
        mapping_authority_hash: authority.mapping_authority.authority_hash.clone(),
        validation_run_id: identity.validation_run_id.clone(),
        validation_result_hash: identity.validation_result_hash.clone(),
        issue_count: u64::try_from(active.issue_count).map_err(|_| {
            KernelError::IntegrityFailed("validation issue count is negative".to_owned())
        })?,
        options_hash: String::new(),
        files,
    };
    options.options_hash = options_hash(&options)?;
    Ok(ActiveExportAuthority {
        options,
        mapping_authority: authority.mapping_authority,
    })
}

fn load_active_validation_evidence(connection: &Connection) -> Result<ActiveValidationEvidence> {
    connection
        .query_row(
            "SELECT engagement.engagement_id, active.generation,
                    workspace.workspace_id, workspace.descriptor_hash,
                    run.job_id, run.source_hash, preflight.manifest_hash,
                    profiling.id, profiling.result_hash,
                    run.profile_id, run.profile_version, run.profile_hash,
                    run.mapping_id, run.mapping_version, run.mapping_hash,
                    run.id, run.result_hash, run.issue_count
             FROM engagement_records AS engagement
             JOIN engagement_active_context AS active
               ON active.engagement_id = engagement.engagement_id
             JOIN object_workspace_engagement_bindings AS workspace_engagement
               ON workspace_engagement.engagement_id = engagement.engagement_id
             JOIN object_workspace_bindings AS workspace
               ON workspace.workspace_id = workspace_engagement.workspace_id
             JOIN validation_runs AS run
               ON run.id = active.active_validation_run_id
             JOIN engagement_acceptance_attempts AS acceptance
               ON acceptance.job_id = run.job_id
              AND acceptance.engagement_id = engagement.engagement_id
             JOIN engagement_source_preflights AS preflight
               ON preflight.preflight_id = acceptance.preflight_id
             JOIN profiling_runs AS profiling
               ON profiling.id = acceptance.profiling_run_id
             WHERE engagement.singleton = 1
               AND active.validation_stage = 'COMPLETE'
               AND active.current_preflight_id = preflight.preflight_id
               AND active.active_job_id = run.job_id
               AND active.active_profiling_run_id = profiling.id
               AND active.selected_profile_id = run.profile_id
               AND active.selected_profile_version = run.profile_version
               AND active.selected_profile_hash = run.profile_hash
               AND active.active_mapping_id = run.mapping_id
               AND active.active_mapping_version = run.mapping_version
               AND active.active_mapping_hash = run.mapping_hash
               AND run.state = 'COMPLETE'
               AND acceptance.profiling_run_id = profiling.id
               AND acceptance.accepted_source_hash = run.source_hash
               AND acceptance.accepted_manifest_hash = preflight.manifest_hash
               AND profiling.job_id = run.job_id
               AND profiling.state = 'COMPLETE'",
            [],
            |row| {
                Ok(ActiveValidationEvidence {
                    identity: ValidationEvidenceIdentity {
                        engagement_id: row.get(0)?,
                        job_id: row.get(4)?,
                        source_hash: row.get(5)?,
                        manifest_hash: row.get(6)?,
                        profiling_run_id: row.get(7)?,
                        profiling_result_hash: row.get(8)?,
                        profile_id: row.get(9)?,
                        profile_version: row.get(10)?,
                        profile_hash: row.get(11)?,
                        mapping_id: row.get(12)?,
                        mapping_version: row.get(13)?,
                        mapping_hash: row.get(14)?,
                        validation_run_id: row.get(15)?,
                        validation_result_hash: row.get(16)?,
                    },
                    engagement_generation: row.get(1)?,
                    workspace_id: row.get(2)?,
                    workspace_descriptor_hash: row.get(3)?,
                    issue_count: row.get(17)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::Conflict(
                "remediation export requires the exact active completed validation".to_owned(),
            )
        })
}

#[allow(clippy::too_many_lines)]
fn replay_validation_authority(
    connection: &Connection,
    identity: &ValidationEvidenceIdentity,
    require_active_tour: bool,
) -> Result<ValidationAuthorityBinding> {
    let active_tour_id: Option<Option<String>> = connection
        .query_row(
            "SELECT active_tour_id FROM engagement_tour_heads WHERE engagement_id = ?1",
            [&identity.engagement_id],
            |row| row.get(0),
        )
        .optional()?;
    let registered = load_registered_validation_authority(connection, identity)?;
    let use_registered = if require_active_tour {
        match (active_tour_id.as_ref(), registered.as_ref()) {
            (None, None) => false,
            (Some(Some(active_id)), Some(tour)) if active_id == &tour.tour_id => true,
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "active remediation validation differs from the registered tour head"
                        .to_owned(),
                ));
            }
        }
    } else {
        registered.is_some()
    };

    let binding = if use_registered {
        let tour = registered.expect("registered tour selected above");
        let mut issues = Vec::new();
        super::verify_tour_integrity(connection, &mut issues)?;
        if !issues.is_empty() {
            return Err(KernelError::IntegrityFailed(format!(
                "registered tour authority failed independent replay: {}",
                issues.join("; ")
            )));
        }
        let authority_id = tour.mapping_authority_id.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "registered tour has no normalized mapping authority ID".to_owned(),
            )
        })?;
        let authority_hash = tour.mapping_authority_hash.as_deref().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "registered tour has no normalized mapping authority hash".to_owned(),
            )
        })?;
        let mapping_authority = verify_exact_mapping_authority(
            connection,
            authority_id,
            authority_hash,
            &identity.engagement_id,
            &identity.job_id,
            &identity.source_hash,
            &identity.mapping_id,
            &identity.mapping_version,
            &identity.mapping_hash,
        )?;
        let expected_mapping_source = match (
            tour.tour_number,
            tour.source_reuse_id.as_deref(),
            tour.mapping_origin_source_hash.as_deref(),
        ) {
            (1, None, None) => identity.source_hash.as_str(),
            (number, Some(_), Some(mapping_source)) if number > 1 => mapping_source,
            _ => {
                return Err(KernelError::IntegrityFailed(
                    "registered tour mapping-origin/source-reuse shape is invalid".to_owned(),
                ));
            }
        };
        let stored_mapping =
            load_stored_mapping(connection, &identity.mapping_id, &identity.mapping_version)?;
        if stored_mapping.mapping_hash != identity.mapping_hash
            || stored_mapping.document.source.package_sha256 != expected_mapping_source
        {
            return Err(KernelError::IntegrityFailed(
                "registered tour mapping origin differs from exact source-reuse authority"
                    .to_owned(),
            ));
        }
        ValidationAuthorityBinding {
            profile_approval_id: tour.profile_approval_id,
            profile_approval_hash: tour.profile_approval_hash,
            mapping_authority,
        }
    } else {
        load_ordinary_validation_authority(connection, identity)?
    };

    let profile = crate::profile_registry::inspect_profile_connection(
        connection,
        &identity.profile_id,
        &identity.profile_version,
        &identity.profile_hash,
    )?;
    let approval = profile.approval.as_ref().ok_or_else(|| {
        KernelError::IntegrityFailed(
            "remediation validation profile has no exact approval".to_owned(),
        )
    })?;
    if approval.approval_id != binding.profile_approval_id
        || approval.approval_hash != binding.profile_approval_hash
    {
        return Err(KernelError::IntegrityFailed(
            "remediation validation profile approval failed exact replay".to_owned(),
        ));
    }
    Ok(binding)
}

fn load_registered_validation_authority(
    connection: &Connection,
    identity: &ValidationEvidenceIdentity,
) -> Result<Option<RegisteredValidationAuthority>> {
    let row = connection
        .query_row(
            "SELECT tour.tour_id, tour.engagement_id, tour.job_id,
                    tour.profiling_run_id,
                    tour.source_hash, tour.manifest_hash,
                    tour.profiling_result_hash,
                    tour.profile_id, tour.profile_version, tour.profile_hash,
                    tour.profile_approval_id, tour.profile_approval_hash,
                    tour.mapping_id, tour.mapping_version, tour.mapping_hash,
                    tour.mapping_authority_id, tour.mapping_authority_hash,
                    tour.validation_run_id, tour.validation_result_hash,
                    tour.tour_number, tour.source_reuse_id,
                    reuse.mapping_source_hash
             FROM engagement_tours AS tour
             LEFT JOIN engagement_mapping_reuse_authorities AS reuse
               ON reuse.source_reuse_id = tour.source_reuse_id
              AND reuse.source_reuse_hash = tour.source_reuse_hash
             WHERE tour.engagement_id = ?1 AND tour.validation_run_id = ?2",
            params![identity.engagement_id, identity.validation_run_id],
            |row| {
                Ok(RegisteredValidationAuthority {
                    tour_id: row.get(0)?,
                    tour_number: row.get(19)?,
                    source_reuse_id: row.get(20)?,
                    mapping_origin_source_hash: row.get(21)?,
                    identity: ValidationEvidenceIdentity {
                        engagement_id: row.get(1)?,
                        job_id: row.get(2)?,
                        source_hash: row.get(4)?,
                        manifest_hash: row.get(5)?,
                        profiling_run_id: row.get(3)?,
                        profiling_result_hash: row.get(6)?,
                        profile_id: row.get(7)?,
                        profile_version: row.get(8)?,
                        profile_hash: row.get(9)?,
                        mapping_id: row.get(12)?,
                        mapping_version: row.get(13)?,
                        mapping_hash: row.get(14)?,
                        validation_run_id: row.get(17)?,
                        validation_result_hash: row.get(18)?,
                    },
                    profile_approval_id: row.get(10)?,
                    profile_approval_hash: row.get(11)?,
                    mapping_authority_id: row.get(15)?,
                    mapping_authority_hash: row.get(16)?,
                })
            },
        )
        .optional()?;
    if row.as_ref().is_some_and(|tour| {
        tour.identity.engagement_id != identity.engagement_id
            || tour.identity.job_id != identity.job_id
            || tour.identity.source_hash != identity.source_hash
            || tour.identity.manifest_hash != identity.manifest_hash
            || tour.identity.profiling_run_id != identity.profiling_run_id
            || tour.identity.profiling_result_hash != identity.profiling_result_hash
            || tour.identity.profile_id != identity.profile_id
            || tour.identity.profile_version != identity.profile_version
            || tour.identity.profile_hash != identity.profile_hash
            || tour.identity.mapping_id != identity.mapping_id
            || tour.identity.mapping_version != identity.mapping_version
            || tour.identity.mapping_hash != identity.mapping_hash
            || tour.identity.validation_run_id != identity.validation_run_id
            || tour.identity.validation_result_hash != identity.validation_result_hash
    }) {
        return Err(KernelError::IntegrityFailed(
            "registered tour differs from exact remediation validation evidence".to_owned(),
        ));
    }
    Ok(row)
}

#[allow(clippy::too_many_lines, clippy::type_complexity)]
fn load_ordinary_validation_authority(
    connection: &Connection,
    identity: &ValidationEvidenceIdentity,
) -> Result<ValidationAuthorityBinding> {
    let attempt_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_validation_attempts
         WHERE engagement_id = ?1 AND run_id = ?2",
        params![identity.engagement_id, identity.validation_run_id],
        |row| row.get(0),
    )?;
    if attempt_count != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "exact remediation validation has {attempt_count} ordinary attempts; expected one"
        )));
    }
    let row: (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = connection.query_row(
        "SELECT job_id, mapping_id, source_hash,
                profile_id, profile_version, profile_hash,
                profile_approval_id, mapping_version, mapping_hash,
                attempt_state, mapping_mode, mapping_approval_id,
                mapping_approval_hash, profile_approval_hash
         FROM engagement_validation_attempts
         WHERE engagement_id = ?1 AND run_id = ?2",
        params![identity.engagement_id, identity.validation_run_id],
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
            ))
        },
    )?;
    let (
        Some(source_hash),
        Some(profile_id),
        Some(profile_version),
        Some(profile_hash),
        Some(profile_approval_id),
        Some(mapping_mode),
    ) = (
        row.2.as_deref(),
        row.3.as_deref(),
        row.4.as_deref(),
        row.5.as_deref(),
        row.6.as_deref(),
        row.10.as_deref(),
    )
    else {
        return Err(KernelError::IntegrityFailed(
            "ordinary remediation validation attempt lacks exact authority".to_owned(),
        ));
    };
    if row.0 != identity.job_id
        || source_hash != identity.source_hash
        || profile_id != identity.profile_id
        || profile_version != identity.profile_version
        || profile_hash != identity.profile_hash
        || row.1 != identity.mapping_id
        || row.7 != identity.mapping_version
        || row.8 != identity.mapping_hash
        || row.9 != "COMPLETE"
    {
        return Err(KernelError::IntegrityFailed(
            "ordinary validation attempt differs from exact remediation validation".to_owned(),
        ));
    }
    let stored_mapping =
        load_stored_mapping(connection, &identity.mapping_id, &identity.mapping_version)?;
    if stored_mapping.mapping_hash != identity.mapping_hash
        || stored_mapping.document.source.package_sha256 != identity.source_hash
    {
        return Err(KernelError::IntegrityFailed(
            "ordinary remediation mapping origin differs from its active source".to_owned(),
        ));
    }
    let mut issues = Vec::new();
    super::verify_validation_attempt_integrity(connection, &mut issues)?;
    if !issues.is_empty() {
        return Err(KernelError::IntegrityFailed(format!(
            "ordinary validation authority failed independent replay: {}",
            issues.join("; ")
        )));
    }
    let profile_approval_hash = row.13;
    let mapping_authority = resolve_mapping_authority(
        connection,
        &identity.engagement_id,
        &identity.job_id,
        &identity.source_hash,
        &identity.mapping_id,
        &identity.mapping_version,
        &identity.mapping_hash,
        mapping_mode,
        row.11.as_deref(),
        row.12.as_deref(),
    )?;
    Ok(ValidationAuthorityBinding {
        profile_approval_id: profile_approval_id.to_owned(),
        profile_approval_hash,
        mapping_authority,
    })
}

fn options_hash(options: &RemediationPackOptionsView) -> Result<String> {
    let mut value = serde_json::to_value(options)?;
    value
        .as_object_mut()
        .ok_or_else(|| KernelError::IntegrityFailed("options are not an object".to_owned()))?
        .remove("optionsHash");
    canonical_hash(&value)
}

fn remediation_file_options(
    connection: &Connection,
    job_id: &str,
    mapping: &MappingDocument,
    profile: &Value,
) -> Result<Vec<RemediationFileOptionView>> {
    let key_fields = profile
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("profile has no entity array".to_owned()))?
        .iter()
        .map(|entity| {
            let entity_id = entity
                .get("entityId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    KernelError::IntegrityFailed("profile entity has no ID".to_owned())
                })?;
            let keys = entity
                .get("key")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    KernelError::IntegrityFailed("profile entity has no key".to_owned())
                })?
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        KernelError::IntegrityFailed("profile key is not a string".to_owned())
                    })
                })
                .collect::<Result<HashSet<_>>>()?;
            Ok((entity_id.to_owned(), keys))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let mut files = mapping.document_source_files_with_entities()?;
    files.sort();
    let mut result = Vec::with_capacity(files.len());
    for (source_file, entity_id) in files {
        let header_json: String = connection.query_row(
            "SELECT header_json FROM csv_files WHERE job_id = ?1 AND path = ?2",
            params![job_id, source_file],
            |row| row.get(0),
        )?;
        let header: Vec<String> = serde_json::from_str(&header_json)?;
        let suggested = mapping
            .bindings
            .iter()
            .filter(|binding| {
                mapping.effective_binding_entity(binding) == entity_id
                    && binding.kind != MappingKind::Constant
                    && binding.source_file.as_deref() == Some(source_file.as_str())
                    && key_fields
                        .get(&entity_id)
                        .is_some_and(|keys| keys.contains(&binding.target_field_id))
            })
            .filter_map(|binding| binding.source_column_ordinal)
            .collect::<HashSet<_>>();
        result.push(RemediationFileOptionView {
            entity_id,
            source_file,
            columns: header
                .into_iter()
                .enumerate()
                .map(
                    |(source_column_ordinal, source_column)| RemediationColumnOptionView {
                        source_column_ordinal,
                        source_column,
                        suggested: suggested.contains(&source_column_ordinal),
                        selected: false,
                    },
                )
                .collect(),
        });
    }
    Ok(result)
}

trait MappingSourceFilesWithEntities {
    fn document_source_files_with_entities(&self) -> Result<Vec<(String, String)>>;
}

impl MappingSourceFilesWithEntities for MappingDocument {
    fn document_source_files_with_entities(&self) -> Result<Vec<(String, String)>> {
        self.source
            .source_files
            .iter()
            .map(|file| Ok((file.clone(), self.entity_for_source_file(file)?.to_owned())))
            .collect()
    }
}

fn validate_selections(
    options: &RemediationPackOptionsView,
    selected: &[RemediationKeySelection],
) -> Result<Vec<RemediationKeySelection>> {
    let available = options
        .files
        .iter()
        .flat_map(|file| {
            file.columns.iter().map(move |column| {
                (
                    (
                        file.entity_id.clone(),
                        file.source_file.clone(),
                        column.source_column_ordinal,
                    ),
                    column.source_column.clone(),
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let required_files = options
        .files
        .iter()
        .map(|file| (file.entity_id.clone(), file.source_file.clone()))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut covered = HashSet::new();
    let mut normalized = selected.to_vec();
    for selection in &normalized {
        validate_text(&selection.entity_id, "remediation key entity", 256)?;
        validate_text(&selection.source_file, "remediation key source file", 1_024)?;
        validate_text(
            &selection.source_column,
            "remediation key source column",
            1_024,
        )?;
        let key = (
            selection.entity_id.clone(),
            selection.source_file.clone(),
            selection.source_column_ordinal,
        );
        if !seen.insert((
            selection.source_file.clone(),
            selection.source_column_ordinal,
        )) {
            return Err(KernelError::PolicyInvalid(
                "remediation key columns contain a duplicate source-file/ordinal".to_owned(),
            ));
        }
        if available.get(&key) != Some(&selection.source_column) {
            return Err(KernelError::Conflict(
                "remediation key selection is absent, stale, renamed, or cross-entity".to_owned(),
            ));
        }
        covered.insert((selection.entity_id.clone(), selection.source_file.clone()));
    }
    if covered != required_files {
        return Err(KernelError::PolicyInvalid(
            "at least one key column must be explicitly selected for every routed source file/entity"
                .to_owned(),
        ));
    }
    normalized.sort_by(|left, right| {
        (
            &left.entity_id,
            &left.source_file,
            left.source_column_ordinal,
        )
            .cmp(&(
                &right.entity_id,
                &right.source_file,
                right.source_column_ordinal,
            ))
    });
    Ok(normalized)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_mapping_authority(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    mapping_mode: &str,
    mapping_approval_id: Option<&str>,
    mapping_approval_hash: Option<&str>,
) -> Result<MappingAuthorityIdentity> {
    if let Some(existing) = load_mapping_authority(
        connection,
        engagement_id,
        job_id,
        source_hash,
        mapping_id,
        mapping_version,
        mapping_hash,
    )? {
        verify_mapping_authority_identity(
            connection,
            engagement_id,
            job_id,
            source_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
            &existing,
        )?;
        return Ok(existing);
    }
    let (
        kind,
        approval_id,
        approval_hash,
        preparation_id,
        validation_attempt_id,
        validation_attempt_hash,
        created_at,
    ) = match mapping_mode {
        "OPERATOR_GOVERNED" => {
            let approval_id = mapping_approval_id.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "operator mapping authority has no exact approval ID".to_owned(),
                )
            })?;
            let approval_hash = mapping_approval_hash.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "operator mapping authority has no exact approval hash".to_owned(),
                )
            })?;
            let approved_at: String = connection.query_row(
                "SELECT approved_at FROM engagement_mapping_approvals
                     WHERE approval_id = ?1 AND approval_hash = ?2
                       AND engagement_id = ?3 AND mapping_id = ?4
                       AND mapping_version = ?5 AND mapping_hash = ?6",
                params![
                    approval_id,
                    approval_hash,
                    engagement_id,
                    mapping_id,
                    mapping_version,
                    mapping_hash,
                ],
                |row| row.get(0),
            )?;
            (
                "OPERATOR_APPROVAL",
                Some(approval_id.to_owned()),
                Some(approval_hash.to_owned()),
                None,
                None,
                None,
                approved_at,
            )
        }
        "CONTROLLED_SUGGESTIONS" => {
            let supporting = connection
                .prepare(
                    "SELECT preparation.preparation_id, validation.id,
                            validation.attempt_hash, preparation.created_at
                     FROM engagement_mapping_preparations AS preparation
                     JOIN mapping_versions AS mapping
                       ON mapping.mapping_id = preparation.mapping_id
                      AND mapping.mapping_version = preparation.mapping_version
                      AND mapping.mapping_hash = preparation.mapping_hash
                     JOIN mapping_validation_attempts AS validation
                       ON validation.id = mapping.attempt_id
                     WHERE preparation.engagement_id = ?1
                       AND preparation.mapping_id = ?2
                       AND preparation.mapping_version = ?3
                       AND preparation.mapping_hash = ?4
                       AND validation.state = 'VALID'
                     ORDER BY preparation.preparation_id",
                )?
                .query_map(
                    params![engagement_id, mapping_id, mapping_version, mapping_hash],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if supporting.len() != 1 {
                return Err(KernelError::IntegrityFailed(format!(
                    "controlled mapping has {} exact preparation/validation authorities; expected one",
                    supporting.len()
                )));
            }
            let supporting = &supporting[0];
            validate_sha(&supporting.2, "mapping validation-attempt hash")?;
            (
                "CONTROLLED_PREPARATION",
                None,
                None,
                Some(supporting.0.clone()),
                Some(supporting.1.clone()),
                Some(supporting.2.clone()),
                supporting.3.clone(),
            )
        }
        other => {
            return Err(KernelError::IntegrityFailed(format!(
                "unknown mapping authority mode {other:?}"
            )));
        }
    };
    let seed = json!({
        "schemaVersion": MAPPING_AUTHORITY_SCHEMA,
        "authorityKind": kind,
        "engagementId": engagement_id,
        "jobId": job_id,
        "sourceHash": source_hash,
        "mappingId": mapping_id,
        "mappingVersion": mapping_version,
        "mappingHash": mapping_hash,
        "mappingApprovalId": approval_id,
        "mappingApprovalHash": approval_hash,
        "preparationId": preparation_id,
        "mappingValidationAttemptId": validation_attempt_id,
        "createdAt": created_at,
    });
    let mut seed = seed;
    if let Some(attempt_hash) = validation_attempt_hash.as_ref() {
        seed.as_object_mut()
            .expect("mapping authority seed is an object")
            .insert(
                "mappingValidationAttemptHash".to_owned(),
                Value::String(attempt_hash.clone()),
            );
    }
    let authority_id = format!("mapping-authority-{}", canonical_hash(&seed)?);
    let mut value = json!({
        "schemaVersion": MAPPING_AUTHORITY_SCHEMA,
        "authorityId": authority_id,
        "authorityKind": kind,
        "engagementId": engagement_id,
        "jobId": job_id,
        "sourceHash": source_hash,
        "mappingId": mapping_id,
        "mappingVersion": mapping_version,
        "mappingHash": mapping_hash,
        "mappingApprovalId": approval_id,
        "mappingApprovalHash": approval_hash,
        "preparationId": preparation_id,
        "mappingValidationAttemptId": validation_attempt_id,
        "createdAt": created_at,
    });
    if let Some(attempt_hash) = validation_attempt_hash.as_ref() {
        value
            .as_object_mut()
            .expect("mapping authority is an object")
            .insert(
                "mappingValidationAttemptHash".to_owned(),
                Value::String(attempt_hash.clone()),
            );
    }
    Ok(MappingAuthorityIdentity {
        authority_id,
        authority_hash: canonical_hash(&value)?,
        canonical_json: canonical_json_string(&value)?,
        authority_kind: kind.to_owned(),
        mapping_approval_id: approval_id,
        mapping_approval_hash: approval_hash,
        preparation_id,
        mapping_validation_attempt_id: validation_attempt_id,
        mapping_validation_attempt_hash: validation_attempt_hash,
        created_at,
    })
}

/// Independently replay one exact normalized mapping authority without
/// creating or changing any evidence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_exact_mapping_authority(
    connection: &Connection,
    authority_id: &str,
    authority_hash: &str,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<MappingAuthorityIdentity> {
    let authority = load_mapping_authority(
        connection,
        engagement_id,
        job_id,
        source_hash,
        mapping_id,
        mapping_version,
        mapping_hash,
    )?
    .ok_or_else(|| {
        KernelError::IntegrityFailed("exact normalized mapping authority is absent".to_owned())
    })?;
    if authority.authority_id != authority_id || authority.authority_hash != authority_hash {
        return Err(KernelError::IntegrityFailed(
            "normalized mapping authority identity/hash differs".to_owned(),
        ));
    }
    verify_mapping_authority_identity(
        connection,
        engagement_id,
        job_id,
        source_hash,
        mapping_id,
        mapping_version,
        mapping_hash,
        &authority,
    )?;
    Ok(authority)
}

#[allow(clippy::too_many_arguments)]
fn load_mapping_authority(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<Option<MappingAuthorityIdentity>> {
    let mut authority = connection
        .query_row(
            "SELECT authority_id, authority_hash, canonical_authority_json,
                    authority_kind, mapping_approval_id, mapping_approval_hash,
                    preparation_id, mapping_validation_attempt_id, created_at
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
            |row| {
                Ok(MappingAuthorityIdentity {
                    authority_id: row.get(0)?,
                    authority_hash: row.get(1)?,
                    canonical_json: row.get(2)?,
                    authority_kind: row.get(3)?,
                    mapping_approval_id: row.get(4)?,
                    mapping_approval_hash: row.get(5)?,
                    preparation_id: row.get(6)?,
                    mapping_validation_attempt_id: row.get(7)?,
                    mapping_validation_attempt_hash: None,
                    created_at: row.get(8)?,
                })
            },
        )
        .optional()?;
    if let Some(authority) = authority.as_mut() {
        let value = parse_unique_json(authority.canonical_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!("mapping authority JSON is invalid: {error}"))
        })?;
        authority.mapping_validation_attempt_hash = value
            .get("mappingValidationAttemptHash")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Ok(authority)
}

fn insert_mapping_authority(
    connection: &Connection,
    options: &RemediationPackOptionsView,
    authority: &MappingAuthorityIdentity,
) -> Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO engagement_mapping_authorities (
             authority_id, authority_kind, engagement_id, job_id, source_hash,
             mapping_id, mapping_version, mapping_hash,
             mapping_approval_id, mapping_approval_hash,
             preparation_id, mapping_validation_attempt_id,
             canonical_authority_json, authority_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            authority.authority_id,
            authority.authority_kind,
            options.engagement_id,
            options.job_id,
            options.source_hash,
            options.mapping_id,
            options.mapping_version,
            options.mapping_hash,
            authority.mapping_approval_id,
            authority.mapping_approval_hash,
            authority.preparation_id,
            authority.mapping_validation_attempt_id,
            authority.canonical_json,
            authority.authority_hash,
            authority.created_at,
        ],
    )?;
    let replay = load_mapping_authority(
        connection,
        &options.engagement_id,
        &options.job_id,
        &options.source_hash,
        &options.mapping_id,
        &options.mapping_version,
        &options.mapping_hash,
    )?
    .ok_or_else(|| KernelError::IntegrityFailed("mapping authority insert vanished".to_owned()))?;
    if replay != *authority {
        return Err(KernelError::IntegrityFailed(
            "stored mapping authority differs from exact canonical evidence".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn verify_mapping_authority_identity(
    connection: &Connection,
    engagement_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    authority: &MappingAuthorityIdentity,
) -> Result<()> {
    let value = parse_unique_json(authority.canonical_json.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!("mapping authority JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != authority.canonical_json
        || canonical_hash(&value)? != authority.authority_hash
        || value.get("schemaVersion").and_then(Value::as_str) != Some(MAPPING_AUTHORITY_SCHEMA)
        || value.get("authorityId").and_then(Value::as_str) != Some(authority.authority_id.as_str())
        || value.get("engagementId").and_then(Value::as_str) != Some(engagement_id)
        || value.get("jobId").and_then(Value::as_str) != Some(job_id)
        || value.get("sourceHash").and_then(Value::as_str) != Some(source_hash)
        || value.get("mappingId").and_then(Value::as_str) != Some(mapping_id)
        || value.get("mappingVersion").and_then(Value::as_str) != Some(mapping_version)
        || value.get("mappingHash").and_then(Value::as_str) != Some(mapping_hash)
        || value.get("authorityKind").and_then(Value::as_str)
            != Some(authority.authority_kind.as_str())
        || value.get("mappingApprovalId").and_then(Value::as_str)
            != authority.mapping_approval_id.as_deref()
        || value.get("mappingApprovalHash").and_then(Value::as_str)
            != authority.mapping_approval_hash.as_deref()
        || value.get("preparationId").and_then(Value::as_str) != authority.preparation_id.as_deref()
        || value
            .get("mappingValidationAttemptId")
            .and_then(Value::as_str)
            != authority.mapping_validation_attempt_id.as_deref()
        || value
            .get("mappingValidationAttemptHash")
            .and_then(Value::as_str)
            != authority.mapping_validation_attempt_hash.as_deref()
        || value.get("createdAt").and_then(Value::as_str) != Some(authority.created_at.as_str())
    {
        return Err(KernelError::IntegrityFailed(
            "mapping authority canonical fields/hash differ from exact row linkage".to_owned(),
        ));
    }
    let job: Option<(String, String)> = connection
        .query_row(
            "SELECT state, source_hash FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if job.as_ref().map(|row| (row.0.as_str(), row.1.as_str())) != Some(("COMPLETE", source_hash)) {
        return Err(KernelError::IntegrityFailed(
            "mapping authority source job/hash failed replay".to_owned(),
        ));
    }
    let mapping = load_stored_mapping(connection, mapping_id, mapping_version)?;
    if mapping.mapping_hash != mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "mapping authority immutable mapping failed replay".to_owned(),
        ));
    }
    match authority.authority_kind.as_str() {
        "OPERATOR_APPROVAL" => {
            let approval_id = authority.mapping_approval_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed("operator authority has no approval ID".to_owned())
            })?;
            let approval_hash = authority.mapping_approval_hash.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed("operator authority has no approval hash".to_owned())
            })?;
            let exists: bool = connection.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM engagement_mapping_approvals
                     WHERE approval_id = ?1 AND approval_hash = ?2
                       AND engagement_id = ?3 AND mapping_id = ?4
                       AND mapping_version = ?5 AND mapping_hash = ?6
                 )",
                params![
                    approval_id,
                    approval_hash,
                    engagement_id,
                    mapping_id,
                    mapping_version,
                    mapping_hash,
                ],
                |row| row.get(0),
            )?;
            if !exists
                || authority.preparation_id.is_some()
                || authority.mapping_validation_attempt_id.is_some()
                || authority.mapping_validation_attempt_hash.is_some()
            {
                return Err(KernelError::IntegrityFailed(
                    "operator mapping authority supporting approval failed replay".to_owned(),
                ));
            }
        }
        "CONTROLLED_PREPARATION" => {
            let preparation_id = authority.preparation_id.as_deref().ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "controlled authority has no preparation ID".to_owned(),
                )
            })?;
            let attempt_id = authority
                .mapping_validation_attempt_id
                .as_deref()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "controlled authority has no validation-attempt ID".to_owned(),
                    )
                })?;
            let expected_attempt_hash = authority
                .mapping_validation_attempt_hash
                .as_deref()
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "controlled authority has no validation-attempt hash".to_owned(),
                    )
                })?;
            let attempt_hash: Option<String> = connection
                .query_row(
                    "SELECT validation.attempt_hash
                     FROM engagement_mapping_preparations AS preparation
                     JOIN mapping_versions AS mapping
                       ON mapping.mapping_id = preparation.mapping_id
                      AND mapping.mapping_version = preparation.mapping_version
                      AND mapping.mapping_hash = preparation.mapping_hash
                     JOIN mapping_validation_attempts AS validation
                       ON validation.id = mapping.attempt_id
                     WHERE preparation.preparation_id = ?1
                       AND preparation.engagement_id = ?2
                       AND preparation.mapping_id = ?3
                       AND preparation.mapping_version = ?4
                       AND preparation.mapping_hash = ?5
                       AND validation.id = ?6 AND validation.state = 'VALID'",
                    params![
                        preparation_id,
                        engagement_id,
                        mapping_id,
                        mapping_version,
                        mapping_hash,
                        attempt_id,
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if attempt_hash.as_deref() != Some(expected_attempt_hash)
                || authority.mapping_approval_id.is_some()
                || authority.mapping_approval_hash.is_some()
            {
                return Err(KernelError::IntegrityFailed(
                    "controlled mapping preparation/validation evidence failed replay".to_owned(),
                ));
            }
        }
        other => {
            return Err(KernelError::IntegrityFailed(format!(
                "unsupported normalized mapping authority kind {other:?}"
            )));
        }
    }
    Ok(())
}

fn remediation_attempt_authority_value(
    attempt_id: &str,
    pack_id: &str,
    options: &RemediationPackOptionsView,
    command: &CreateRemediationPackCommand,
    selections: &[RemediationKeySelection],
    request_hash: &str,
    created_at: &str,
) -> Value {
    json!({
        "schemaVersion": REMEDIATION_PACK_AUTHORITY_SCHEMA,
        "attemptId": attempt_id,
        "plannedPackId": pack_id,
        "requestHash": request_hash,
        "expectedEngagementGeneration": command.expected_engagement_generation,
        "workspaceId": options.workspace_id,
        "workspaceDescriptorHash": options.workspace_descriptor_hash,
        "engagementId": options.engagement_id,
        "jobId": options.job_id,
        "sourceHash": options.source_hash,
        "manifestHash": options.manifest_hash,
        "profilingRunId": options.profiling_run_id,
        "profilingResultHash": options.profiling_result_hash,
        "profileId": options.profile_id,
        "profileVersion": options.profile_version,
        "profileHash": options.profile_hash,
        "profileApprovalId": options.profile_approval_id,
        "profileApprovalHash": options.profile_approval_hash,
        "mappingId": options.mapping_id,
        "mappingVersion": options.mapping_version,
        "mappingHash": options.mapping_hash,
        "mappingAuthorityId": options.mapping_authority_id,
        "mappingAuthorityHash": options.mapping_authority_hash,
        "validationRunId": options.validation_run_id,
        "validationResultHash": options.validation_result_hash,
        "optionsHash": options.options_hash,
        "selectedKeyColumns": selections,
        "actor": command.actor,
        "reason": command.reason,
        "localDataExportAcknowledgement": command.local_data_export_acknowledgement,
        "includeOffendingValues": command.include_offending_values,
        "offendingValuesAcknowledgement": command.offending_values_acknowledgement,
        "createdAt": created_at,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn insert_remediation_attempt(
    connection: &Connection,
    attempt_id: &str,
    pack_id: &str,
    options: &RemediationPackOptionsView,
    command: &CreateRemediationPackCommand,
    request_hash: &str,
    canonical_request: &str,
    canonical_authority: &str,
    authority_hash: &str,
    created_at: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO engagement_remediation_pack_attempts (
             attempt_id, planned_pack_id, engagement_id, idempotency_key,
             request_hash, canonical_request_json, expected_generation,
             workspace_id, workspace_descriptor_hash, job_id, source_hash,
             manifest_hash, profiling_run_id, profiling_result_hash,
             profile_id, profile_version, profile_hash,
             profile_approval_id, profile_approval_hash,
             mapping_id, mapping_version, mapping_hash,
             mapping_authority_id, mapping_authority_hash,
             validation_run_id, validation_result_hash, options_hash,
             actor, reason, local_export_acknowledgement,
             include_offending_values, offending_values_acknowledgement,
             canonical_authority_json, authority_hash, created_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
             ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
             ?31, ?32, ?33, ?34, ?35
         )",
        params![
            attempt_id,
            pack_id,
            options.engagement_id,
            command.idempotency_key,
            request_hash,
            canonical_request,
            sqlite_integer(
                command.expected_engagement_generation,
                "engagement generation"
            )?,
            options.workspace_id,
            options.workspace_descriptor_hash,
            options.job_id,
            options.source_hash,
            options.manifest_hash,
            options.profiling_run_id,
            options.profiling_result_hash,
            options.profile_id,
            options.profile_version,
            options.profile_hash,
            options.profile_approval_id,
            options.profile_approval_hash,
            options.mapping_id,
            options.mapping_version,
            options.mapping_hash,
            options.mapping_authority_id,
            options.mapping_authority_hash,
            options.validation_run_id,
            options.validation_result_hash,
            options.options_hash,
            command.actor,
            command.reason,
            command.local_data_export_acknowledgement,
            i64::from(command.include_offending_values),
            command.offending_values_acknowledgement,
            canonical_authority,
            authority_hash,
            created_at,
        ],
    )?;
    Ok(())
}

fn load_attempt_by_idempotency(
    connection: &Connection,
    engagement_id: &str,
    idempotency_key: &str,
) -> Result<Option<(String, String)>> {
    connection
        .query_row(
            "SELECT attempt_id, request_hash FROM engagement_remediation_pack_attempts
             WHERE engagement_id = ?1 AND idempotency_key = ?2",
            params![engagement_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
}

#[allow(clippy::type_complexity)]
fn load_planned_attempt(
    connection: &Connection,
    attempt_id: &str,
) -> Result<(
    String,
    String,
    String,
    String,
    Vec<RemediationKeySelection>,
    String,
    String,
    String,
)> {
    let terminal: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM engagement_remediation_pack_results WHERE attempt_id = ?1
         )",
        [attempt_id],
        |result| result.get(0),
    )?;
    if terminal {
        return Err(KernelError::Conflict(
            "terminal remediation attempt has no replayable completed pack".to_owned(),
        ));
    }
    load_attempt_for_integrity(connection, attempt_id)
}

#[allow(clippy::type_complexity)]
fn load_attempt_for_integrity(
    connection: &Connection,
    attempt_id: &str,
) -> Result<(
    String,
    String,
    String,
    String,
    Vec<RemediationKeySelection>,
    String,
    String,
    String,
)> {
    let row: (String, String, String, String, String, String, String) = connection.query_row(
        "SELECT planned_pack_id, created_at, authority_hash,
                canonical_authority_json, options_hash,
                mapping_authority_id, mapping_authority_hash
         FROM engagement_remediation_pack_attempts WHERE attempt_id = ?1",
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
            ))
        },
    )?;
    let mut statement = connection.prepare(
        "SELECT canonical_selection_json, selection_hash
         FROM engagement_remediation_pack_selections
         WHERE attempt_id = ?1
         ORDER BY entity_id, source_file, source_column_ordinal",
    )?;
    let selections = statement
        .query_map([attempt_id], |selection| {
            Ok((
                selection.get::<_, String>(0)?,
                selection.get::<_, String>(1)?,
            ))
        })?
        .map(|selection| {
            let (canonical, hash) = selection?;
            let value = parse_unique_json(canonical.as_bytes()).map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "planned remediation selection is invalid: {error}"
                ))
            })?;
            if canonical_json_string(&value)? != canonical || canonical_hash(&value)? != hash {
                return Err(KernelError::IntegrityFailed(
                    "planned remediation selection bytes/hash differ".to_owned(),
                ));
            }
            serde_json::from_value(value).map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((row.0, row.1, row.2, row.3, selections, row.4, row.5, row.6))
}

fn load_complete_issue_members(
    connection: &Connection,
    options: &RemediationPackOptionsView,
) -> Result<Vec<IssueMember>> {
    crate::validation::verify_validation_run_for_read(connection, &options.validation_run_id)?;
    if options.issue_count > u64::try_from(MAXIMUM_ISSUES).unwrap_or(u64::MAX) {
        return Err(KernelError::PolicyInvalid(format!(
            "validation has {} issues; remediation maximum is {MAXIMUM_ISSUES}",
            options.issue_count
        )));
    }
    let mut statement = connection.prepare(
        "SELECT issue_id, issue_hash, severity, rule_id, issue_code, display_message,
                entity_id, field_id, source_file, source_ordinal,
                line_start, line_end, lineage_json
         FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id",
    )?;
    let issues = statement
        .query_map([&options.validation_run_id], |row| {
            Ok(IssueRow {
                issue_id: row.get(0)?,
                issue_hash: row.get(1)?,
                severity: row.get(2)?,
                rule_id: row.get(3)?,
                issue_code: row.get(4)?,
                display_message: row.get(5)?,
                entity_id: row.get(6)?,
                field_id: row.get(7)?,
                source_file: row.get(8)?,
                source_ordinal: u64::try_from(row.get::<_, i64>(9)?)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, i64::MIN))?,
                line_start: u64::try_from(row.get::<_, i64>(10)?)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, i64::MIN))?,
                line_end: u64::try_from(row.get::<_, i64>(11)?)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, i64::MIN))?,
                lineage_json: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if u64::try_from(issues.len()).unwrap_or(u64::MAX) != options.issue_count {
        return Err(KernelError::IntegrityFailed(
            "validation issue membership differs from result issue count".to_owned(),
        ));
    }
    for page in issues.chunks(200) {
        let issue_ids = page
            .iter()
            .map(|issue| issue.issue_id.clone())
            .collect::<Vec<_>>();
        crate::validation::verify_validation_issue_page_for_read(
            connection,
            &options.validation_run_id,
            &issue_ids,
        )?;
    }
    let accepted_headers = load_accepted_headers(connection, &options.job_id)?;
    issues
        .into_iter()
        .map(|issue| {
            let lineage = parse_unique_json(issue.lineage_json.as_bytes()).map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "issue {} lineage is invalid: {error}",
                    issue.issue_id
                ))
            })?;
            let (kind, ordinal, column) =
                exact_offending_lineage(&issue, &lineage, &accepted_headers)?;
            let value = json!({
                "schemaVersion": REMEDIATION_ISSUE_MEMBER_SCHEMA,
                "validationRunId": options.validation_run_id,
                "validationResultHash": options.validation_result_hash,
                "issueId": issue.issue_id,
                "issueHash": issue.issue_hash,
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
                "offendingLineageKind": kind,
                "offendingSourceColumnOrdinal": ordinal,
                "offendingSourceColumn": column,
            });
            Ok(IssueMember {
                issue,
                lineage_kind: kind,
                offending_ordinal: ordinal,
                offending_column: column,
                canonical_json: canonical_json_string(&value)?,
                member_hash: canonical_hash(&value)?,
            })
        })
        .collect()
}

fn load_accepted_headers(
    connection: &Connection,
    job_id: &str,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut statement = connection
        .prepare("SELECT path, header_json FROM csv_files WHERE job_id = ?1 ORDER BY path")?;
    statement
        .query_map([job_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (path, header_json) = row?;
            let header: Vec<String> = serde_json::from_str(&header_json)?;
            if header.is_empty() || header.len() > 4096 {
                return Err(KernelError::IntegrityFailed(format!(
                    "accepted CSV header for {path:?} is outside its bound"
                )));
            }
            Ok((path, header))
        })
        .collect()
}

fn exact_offending_lineage(
    issue: &IssueRow,
    lineage: &Value,
    accepted_headers: &BTreeMap<String, Vec<String>>,
) -> Result<(&'static str, Option<usize>, Option<String>)> {
    let source_file = lineage.get("sourceFile").and_then(Value::as_str);
    let ordinal = lineage
        .get("sourceColumnOrdinal")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let column = lineage
        .get("sourceColumn")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (source_file, ordinal, column) {
        (Some(source_file), Some(ordinal), Some(column)) => {
            if source_file != issue.source_file {
                return Err(KernelError::IntegrityFailed(format!(
                    "issue {} offending lineage names a different accepted file",
                    issue.issue_id
                )));
            }
            let header = accepted_headers.get(source_file).ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "issue {} offending lineage names an unaccepted CSV file",
                    issue.issue_id
                ))
            })?;
            if header.get(ordinal).map(String::as_str) != Some(column.as_str()) {
                return Err(KernelError::IntegrityFailed(format!(
                    "issue {} offending lineage column does not match the accepted CSV header",
                    issue.issue_id
                )));
            }
            Ok(("EXACT_SOURCE_COLUMN", Some(ordinal), Some(column)))
        }
        (Some(source_file), None, None) if source_file == issue.source_file => {
            Ok(("NO_SINGLE_SOURCE_COLUMN", None, None))
        }
        (None, None, None) => Ok(("NO_SINGLE_SOURCE_COLUMN", None, None)),
        _ => Err(KernelError::IntegrityFailed(format!(
            "issue {} has incomplete or inconsistent offending-value lineage",
            issue.issue_id
        ))),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn build_archive_members(
    pack_id: &str,
    created_at: &str,
    options: &RemediationPackOptionsView,
    command: &CreateRemediationPackCommand,
    selections: &[RemediationKeySelection],
    issues: &[IssueMember],
    connection: &Connection,
) -> Result<(Vec<ArchiveMember>, String)> {
    let selection_by_file = selections.iter().fold(
        BTreeMap::<String, Vec<&RemediationKeySelection>>::new(),
        |mut grouped, selection| {
            grouped
                .entry(selection.source_file.clone())
                .or_default()
                .push(selection);
            grouped
        },
    );
    let mut rows = Vec::with_capacity(issues.len());
    let mut total_value_bytes = 0_usize;
    for member in issues {
        let fields_json: String = connection.query_row(
            "SELECT fields_json FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
            params![
                options.job_id,
                member.issue.source_file,
                sqlite_integer(member.issue.source_ordinal, "issue source ordinal")?,
            ],
            |row| row.get(0),
        )?;
        let fields: Vec<String> = serde_json::from_str(&fields_json)?;
        let keys = selection_by_file
            .get(&member.issue.source_file)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "issue source file has no explicitly selected business key".to_owned(),
                )
            })?
            .iter()
            .map(|selection| {
                let value = fields
                    .get(selection.source_column_ordinal)
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "selected key ordinal is absent from accepted source row".to_owned(),
                        )
                    })?
                    .clone();
                checked_export_value(&value, &mut total_value_bytes)?;
                Ok(json!({
                    "entityId": selection.entity_id,
                    "sourceFile": selection.source_file,
                    "sourceColumnOrdinal": selection.source_column_ordinal,
                    "sourceColumn": selection.source_column,
                    "value": value,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let offending = if command.include_offending_values {
            match (member.offending_ordinal, member.offending_column.as_deref()) {
                (Some(ordinal), Some(column)) => {
                    let value = fields.get(ordinal).ok_or_else(|| {
                        KernelError::IntegrityFailed(
                            "offending-value lineage ordinal is absent from accepted row"
                                .to_owned(),
                        )
                    })?;
                    checked_export_value(value, &mut total_value_bytes)?;
                    json!({
                        "applicability": "EXACT_SOURCE_COLUMN",
                        "sourceColumnOrdinal": ordinal,
                        "sourceColumn": column,
                        "value": value,
                    })
                }
                _ => json!({ "applicability": "NO_SINGLE_SOURCE_COLUMN" }),
            }
        } else {
            json!({ "applicability": member.lineage_kind })
        };
        rows.push(json!({
            "issueId": member.issue.issue_id,
            "issueHash": member.issue.issue_hash,
            "severity": member.issue.severity,
            "ruleId": member.issue.rule_id,
            "issueCode": member.issue.issue_code,
            "displayMessage": member.issue.display_message,
            "entityId": member.issue.entity_id,
            "fieldId": member.issue.field_id,
            "sourceFile": member.issue.source_file,
            "sourceOrdinal": member.issue.source_ordinal,
            "lineStart": member.issue.line_start,
            "lineEnd": member.issue.line_end,
            "selectedKeys": keys,
            "offendingValue": offending,
        }));
    }
    let membership_hash = membership_hash(issues)?;
    let json_document = json!({
        "schemaVersion": "migration-stream/remediation-issues/v1",
        "packId": pack_id,
        "validationRunId": options.validation_run_id,
        "validationResultHash": options.validation_result_hash,
        "membershipHash": membership_hash,
        "includeOffendingValues": command.include_offending_values,
        "issues": rows,
    });
    let json_bytes = canonical_json_string(&json_document)?.into_bytes();
    let csv_bytes = remediation_csv(&rows)?;
    let issues_json_member = ArchiveMember {
        filename: "remediation-issues.json",
        media_type: "application/json",
        row_count: Some(u64::try_from(issues.len()).unwrap_or(u64::MAX)),
        bytes: json_bytes,
    };
    let issues_csv_member = ArchiveMember {
        filename: "remediation-issues.csv",
        media_type: "text/csv",
        row_count: Some(u64::try_from(issues.len()).unwrap_or(u64::MAX)),
        bytes: csv_bytes,
    };
    let manifest_value = json!({
        "schemaVersion": "migration-stream/remediation-pack-manifest/v1",
        "packId": pack_id,
        "createdAt": created_at,
        "scope": "LOCAL_DATA_BEARING_REMEDIATION_ONLY_NOT_A_READINESS_PACKAGE_OR_TARGET_PAYLOAD",
        "authority": {
            "workspaceId": options.workspace_id,
            "workspaceDescriptorHash": options.workspace_descriptor_hash,
            "engagementId": options.engagement_id,
            "jobId": options.job_id,
            "sourceHash": options.source_hash,
            "manifestHash": options.manifest_hash,
            "profilingRunId": options.profiling_run_id,
            "profilingResultHash": options.profiling_result_hash,
            "profileId": options.profile_id,
            "profileVersion": options.profile_version,
            "profileHash": options.profile_hash,
            "profileApprovalId": options.profile_approval_id,
            "profileApprovalHash": options.profile_approval_hash,
            "mappingId": options.mapping_id,
            "mappingVersion": options.mapping_version,
            "mappingHash": options.mapping_hash,
            "mappingAuthorityId": options.mapping_authority_id,
            "mappingAuthorityHash": options.mapping_authority_hash,
            "validationRunId": options.validation_run_id,
            "validationResultHash": options.validation_result_hash,
        },
        "export": {
            "actor": command.actor,
            "reason": command.reason,
            "localDataExportAcknowledgement": command.local_data_export_acknowledgement,
            "includeOffendingValues": command.include_offending_values,
            "offendingValuesAcknowledgement": command.offending_values_acknowledgement,
            "selectedKeyColumns": selections,
            "issueCount": issues.len(),
            "membershipHash": membership_hash,
        },
        "members": [
            member_manifest_value(&issues_json_member),
            member_manifest_value(&issues_csv_member),
        ],
    });
    let manifest_bytes = canonical_json_string(&manifest_value)?.into_bytes();
    let manifest_hash = sha256_hex(&manifest_bytes);
    let members = vec![
        ArchiveMember {
            filename: "manifest.json",
            media_type: "application/json",
            row_count: None,
            bytes: manifest_bytes,
        },
        issues_json_member,
        issues_csv_member,
    ];
    Ok((members, manifest_hash))
}

#[allow(clippy::too_many_arguments)]
fn build_and_publish_archive(
    workspace: &Path,
    pack_id: &str,
    created_at: &str,
    options: &RemediationPackOptionsView,
    command: &CreateRemediationPackCommand,
    selections: &[RemediationKeySelection],
    issues: &[IssueMember],
    connection: &Connection,
) -> Result<PublishedArchive> {
    let (members, manifest_hash) = build_archive_members(
        pack_id, created_at, options, command, selections, issues, connection,
    )?;
    publish_archive(workspace, pack_id, members, manifest_hash)
}

fn checked_export_value(value: &str, total: &mut usize) -> Result<()> {
    if value.len() > MAXIMUM_VALUE_BYTES {
        return Err(KernelError::PolicyInvalid(format!(
            "one selected remediation value exceeds {MAXIMUM_VALUE_BYTES} bytes"
        )));
    }
    *total = total.checked_add(value.len()).ok_or_else(|| {
        KernelError::PolicyInvalid("remediation exported-value byte count overflow".to_owned())
    })?;
    if *total > MAXIMUM_TOTAL_EXPORTED_VALUE_BYTES {
        return Err(KernelError::PolicyInvalid(format!(
            "remediation exported values exceed {MAXIMUM_TOTAL_EXPORTED_VALUE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn member_manifest_value(member: &ArchiveMember) -> Value {
    json!({
        "filename": member.filename,
        "mediaType": member.media_type,
        "rowCount": member.row_count,
        "byteSize": member.bytes.len(),
        "sha256": sha256_hex(&member.bytes),
    })
}

fn remediation_csv(rows: &[Value]) -> Result<Vec<u8>> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    writer
        .write_record([
            "issue_id",
            "issue_hash",
            "severity",
            "rule_id",
            "issue_code",
            "message",
            "entity_id",
            "field_id",
            "source_file",
            "source_ordinal",
            "line_start",
            "line_end",
            "selected_keys_json",
            "offending_value_json",
        ])
        .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
    for row in rows {
        let selected_keys = canonical_json_string(&row["selectedKeys"])?;
        let offending = canonical_json_string(&row["offendingValue"])?;
        writer
            .write_record([
                formula_safe(row["issueId"].as_str().unwrap_or_default()),
                formula_safe(row["issueHash"].as_str().unwrap_or_default()),
                formula_safe(row["severity"].as_str().unwrap_or_default()),
                formula_safe(row["ruleId"].as_str().unwrap_or_default()),
                formula_safe(row["issueCode"].as_str().unwrap_or_default()),
                formula_safe(row["displayMessage"].as_str().unwrap_or_default()),
                formula_safe(row["entityId"].as_str().unwrap_or_default()),
                formula_safe(row["fieldId"].as_str().unwrap_or_default()),
                formula_safe(row["sourceFile"].as_str().unwrap_or_default()),
                row["sourceOrdinal"]
                    .as_u64()
                    .unwrap_or_default()
                    .to_string(),
                row["lineStart"].as_u64().unwrap_or_default().to_string(),
                row["lineEnd"].as_u64().unwrap_or_default().to_string(),
                formula_safe(&selected_keys),
                formula_safe(&offending),
            ])
            .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
    }
    writer
        .into_inner()
        .map_err(|error| KernelError::PolicyInvalid(error.into_error().to_string()))
}

fn formula_safe(value: &str) -> String {
    if value
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r'))
    {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

#[allow(clippy::too_many_lines)]
fn publish_archive(
    workspace: &Path,
    pack_id: &str,
    members: Vec<ArchiveMember>,
    manifest_hash: String,
) -> Result<PublishedArchive> {
    let directory = ensure_real_directory(workspace, Path::new(REMEDIATION_DIRECTORY))?;
    let partial = directory.join(format!(".{pack_id}.part"));
    match fs::symlink_metadata(&partial) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata_is_reparse_point(&metadata) {
                return Err(KernelError::IntegrityFailed(
                    "owned remediation retry partial is not a regular non-link file".to_owned(),
                ));
            }
            verify_named_regular_file(&partial, None, false)?;
            fs::remove_file(&partial).map_err(|error| KernelError::io(&partial, error))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(KernelError::io(&partial, error)),
    }
    let mut partial_options = OpenOptions::new();
    partial_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        partial_options.mode(0o600);
    }
    let partial_file = partial_options
        .open(&partial)
        .map_err(|error| KernelError::io(&partial, error))?;
    let mut cleanup = OwnedArchiveCleanup::new(partial.clone(), partial_file);
    let writer_file = cleanup
        .file
        .as_ref()
        .ok_or_else(|| {
            KernelError::IntegrityFailed(
                "owned remediation partial lost its retained file handle".to_owned(),
            )
        })?
        .try_clone()
        .map_err(|error| KernelError::io(&partial, error))?;
    let mut writer = ZipWriter::new(writer_file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o400)
        .with_alignment(1);
    for (member_index, member) in members.iter().enumerate() {
        writer
            .start_file(member.filename, options)
            .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
        writer
            .write_all(&member.bytes)
            .map_err(|error| KernelError::io(&partial, error))?;
        #[cfg(test)]
        inject_archive_write_failure(member_index)?;
        #[cfg(not(test))]
        let _ = member_index;
    }
    let file = writer
        .finish()
        .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
    file.sync_all()
        .map_err(|error| KernelError::io(&partial, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o400))
            .map_err(|error| KernelError::io(&partial, error))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = file
            .metadata()
            .map_err(|error| KernelError::io(&partial, error))?
            .permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
            .map_err(|error| KernelError::io(&partial, error))?;
    }
    file.sync_all()
        .map_err(|error| KernelError::io(&partial, error))?;
    if !named_file_matches(&partial, &file)? {
        return Err(KernelError::IntegrityFailed(
            "owned remediation partial changed pathname identity".to_owned(),
        ));
    }
    drop(file);
    let (archive_sha256, archive_size) = hash_regular_file(&partial)?;
    if archive_size == 0 || archive_size > MAXIMUM_ARCHIVE_BYTES {
        let _ = fs::remove_file(&partial);
        return Err(KernelError::PolicyInvalid(format!(
            "remediation archive size must be in 1..={MAXIMUM_ARCHIVE_BYTES} bytes"
        )));
    }
    let filename = format!("{archive_sha256}.zip");
    let final_path = directory.join(&filename);
    cleanup.add_candidate(final_path.clone());
    let created_file = match crate::durability::publish_new(&partial, &final_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let verification = verify_archive_file(&final_path, &archive_sha256, archive_size);
            let removal =
                fs::remove_file(&partial).map_err(|error| KernelError::io(&partial, error));
            verification?;
            removal?;
            false
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            return Err(KernelError::io(&final_path, error));
        }
    };
    verify_archive_file(&final_path, &archive_sha256, archive_size)?;
    let cleanup = created_file.then_some(cleanup);
    Ok(PublishedArchive {
        relative_path: format!("{REMEDIATION_DIRECTORY}/{filename}"),
        archive_sha256,
        archive_size,
        manifest_hash,
        members,
        cleanup,
    })
}

fn ensure_real_directory(workspace: &Path, relative: &Path) -> Result<PathBuf> {
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(KernelError::IntegrityFailed(
            "controlled remediation directory is not a normalized relative path".to_owned(),
        ));
    }
    let mut path = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            unreachable!("validated normal path component")
        };
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                verify_real_directory(&path, &metadata)?;
            }
            Ok(metadata) => {
                let _ = metadata;
                return Err(KernelError::IntegrityFailed(format!(
                    "remediation directory component is not a real directory: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|error| KernelError::io(&path, error))?;
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
                verify_real_directory(&path, &metadata)?;
            }
            Err(error) => return Err(KernelError::io(&path, error)),
        }
    }
    Ok(path)
}

fn checked_archive_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let name = validate_archive_relative_path(relative)?;
    let directory = ensure_existing_real_directory(workspace, Path::new(REMEDIATION_DIRECTORY))?;
    Ok(directory.join(name))
}

fn validate_archive_relative_path(relative: &str) -> Result<&str> {
    let expected_prefix = format!("{REMEDIATION_DIRECTORY}/");
    let name = relative.strip_prefix(&expected_prefix).ok_or_else(|| {
        KernelError::IntegrityFailed("remediation archive path escapes its namespace".to_owned())
    })?;
    if name.len() != 68
        || Path::new(name).extension() != Some(std::ffi::OsStr::new("zip"))
        || !name[..64].bytes().all(|byte| byte.is_ascii_hexdigit())
        || name[..64].bytes().any(|byte| byte.is_ascii_uppercase())
        || Path::new(name).components().count() != 1
    {
        return Err(KernelError::IntegrityFailed(
            "remediation archive path is not content addressed".to_owned(),
        ));
    }
    Ok(name)
}

fn ensure_existing_real_directory(workspace: &Path, relative: &Path) -> Result<PathBuf> {
    let mut path = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(KernelError::IntegrityFailed(
                "remediation directory path is not normalized".to_owned(),
            ));
        };
        path.push(component);
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        verify_real_directory(&path, &metadata)?;
    }
    Ok(path)
}

fn verify_real_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_dir() || metadata_is_reparse_point(metadata) {
        return Err(KernelError::IntegrityFailed(format!(
            "remediation directory is not a real non-link directory: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        let directory = crate::durability::open_directory_nofollow(path)
            .map_err(|error| KernelError::io(path, error))?;
        let opened = directory
            .metadata()
            .map_err(|error| KernelError::io(path, error))?;
        if !opened.file_type().is_dir()
            || crate::durability::is_reparse_point(&opened)
            || !crate::durability::path_names_open_directory(path, &directory)
                .map_err(|error| KernelError::io(path, error))?
        {
            return Err(KernelError::IntegrityFailed(format!(
                "remediation directory pathname/handle authority differs: {}",
                path.display()
            )));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err(KernelError::IntegrityFailed(format!(
            "remediation directory no-follow checks are unsupported: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    crate::durability::is_reparse_point(metadata)
}

#[cfg(not(windows))]
const fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn open_remediation_file_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| KernelError::io(path, error.into()))?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn open_remediation_file_nofollow(path: &Path) -> Result<File> {
    crate::durability::open_file_nofollow(path).map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_remediation_file_nofollow(path: &Path) -> Result<File> {
    Err(KernelError::IntegrityFailed(format!(
        "remediation archive no-follow reads are unsupported: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn named_file_matches(path: &Path, file: &File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    Ok(named.file_type().is_file() && named.dev() == opened.dev() && named.ino() == opened.ino())
}

#[cfg(windows)]
fn named_file_matches(path: &Path, file: &File) -> Result<bool> {
    crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn named_file_matches(path: &Path, _file: &File) -> Result<bool> {
    Err(KernelError::IntegrityFailed(format!(
        "remediation archive identity checks are unsupported: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn remediation_file_link_count(file: &File, path: &Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt as _;

    file.metadata()
        .map(|metadata| metadata.nlink())
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(windows)]
fn remediation_file_link_count(file: &File, path: &Path) -> Result<u64> {
    crate::durability::file_link_count(file).map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn remediation_file_link_count(_file: &File, path: &Path) -> Result<u64> {
    Err(KernelError::IntegrityFailed(format!(
        "remediation archive link checks are unsupported: {}",
        path.display()
    )))
}

fn verify_open_file_constraints(path: &Path, file: &File, require_read_only: bool) -> Result<u64> {
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file()
        || metadata_is_reparse_point(&metadata)
        || remediation_file_link_count(file, path)? != 1
    {
        return Err(KernelError::IntegrityFailed(
            "remediation archive handle type, reparse state, or link count differs".to_owned(),
        ));
    }
    if require_read_only {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.mode() & 0o777 != 0o400 {
                return Err(KernelError::IntegrityFailed(
                    "remediation archive owner-only read mode differs".to_owned(),
                ));
            }
        }
        #[cfg(windows)]
        if !metadata.permissions().readonly() {
            return Err(KernelError::IntegrityFailed(
                "remediation archive read-only mode differs".to_owned(),
            ));
        }
    }
    Ok(metadata.len())
}

fn verify_named_regular_file(
    path: &Path,
    expected_size: Option<u64>,
    require_read_only: bool,
) -> Result<File> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !named.file_type().is_file() || metadata_is_reparse_point(&named) {
        return Err(KernelError::IntegrityFailed(format!(
            "remediation archive is not one real regular non-link file: {}",
            path.display()
        )));
    }
    let file = open_remediation_file_nofollow(path)?;
    let opened_size = verify_open_file_constraints(path, &file, require_read_only)?;
    if opened_size != named.len()
        || expected_size.is_some_and(|expected| opened_size != expected)
        || opened_size > MAXIMUM_ARCHIVE_BYTES
        || !named_file_matches(path, &file)?
    {
        return Err(KernelError::IntegrityFailed(
            "remediation archive pathname/handle/size authority differs".to_owned(),
        ));
    }
    Ok(file)
}

fn hash_regular_file(path: &Path) -> Result<(String, u64)> {
    let mut file = verify_named_regular_file(path, None, true)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| KernelError::io(path, error))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                KernelError::IntegrityFailed("remediation archive size overflow".to_owned())
            })?;
        if size > MAXIMUM_ARCHIVE_BYTES {
            return Err(KernelError::IntegrityFailed(
                "remediation archive exceeds its byte bound".to_owned(),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    if !named_file_matches(path, &file)? {
        return Err(KernelError::IntegrityFailed(
            "remediation archive pathname changed during hashing".to_owned(),
        ));
    }
    verify_open_file_constraints(path, &file, true)?;
    Ok((crate::canonical::lower_hex(hasher.finalize()), size))
}

fn read_verified_archive(path: &Path, expected_hash: &str, expected_size: u64) -> Result<Vec<u8>> {
    if expected_size == 0 || expected_size > MAXIMUM_ARCHIVE_BYTES {
        return Err(KernelError::IntegrityFailed(
            "remediation download is not the expected bounded regular file".to_owned(),
        ));
    }
    let file = verify_named_regular_file(path, Some(expected_size), true)?;
    let capacity = usize::try_from(expected_size).map_err(|_| {
        KernelError::IntegrityFailed("remediation download size cannot be addressed".to_owned())
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    (&file)
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() != capacity || sha256_hex(&bytes) != expected_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation download bytes differ from immutable authority".to_owned(),
        ));
    }
    if !named_file_matches(path, &file)? {
        return Err(KernelError::IntegrityFailed(
            "remediation download pathname changed during verification".to_owned(),
        ));
    }
    verify_open_file_constraints(path, &file, true)?;
    Ok(bytes)
}

fn verify_archive_file(path: &Path, expected_hash: &str, expected_size: u64) -> Result<()> {
    let (actual_hash, actual_size) = hash_regular_file(path)?;
    if actual_hash != expected_hash || actual_size != expected_size {
        return Err(KernelError::IntegrityFailed(
            "remediation archive differs from its immutable digest/size".to_owned(),
        ));
    }
    Ok(())
}

fn membership_hash(issues: &[IssueMember]) -> Result<String> {
    canonical_hash(&json!({
        "schemaVersion": "migration-stream/engagement/remediation-membership/v1",
        "members": issues.iter().map(|issue| &issue.member_hash).collect::<Vec<_>>(),
    }))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn persist_completed_pack(
    connection: &Connection,
    pack_id: &str,
    attempt_id: &str,
    options: &RemediationPackOptionsView,
    command: &CreateRemediationPackCommand,
    selections: &[RemediationKeySelection],
    issues: &[IssueMember],
    archive: &PublishedArchive,
    authority_hash: &str,
    completed_at: &str,
) -> Result<()> {
    let selected_columns_json = canonical_json_string(&serde_json::to_value(selections)?)?;
    let selected_columns_hash = sha256_hex(selected_columns_json.as_bytes());
    let membership_hash = membership_hash(issues)?;
    let record_value = json!({
        "schemaVersion": REMEDIATION_PACK_RECORD_SCHEMA,
        "packId": pack_id,
        "attemptId": attempt_id,
        "engagementId": options.engagement_id,
        "validationRunId": options.validation_run_id,
        "sourceHash": options.source_hash,
        "selectedColumnsHash": selected_columns_hash,
        "includeOffendingValues": command.include_offending_values,
        "actor": command.actor,
        "reason": command.reason,
        "issueCount": issues.len(),
        "membershipHash": membership_hash,
        "memberCount": archive.members.len(),
        "archiveRelativePath": archive.relative_path,
        "archiveSize": archive.archive_size,
        "archiveSha256": archive.archive_sha256,
        "manifestHash": archive.manifest_hash,
        "completedAt": completed_at,
    });
    let canonical_record = canonical_json_string(&record_value)?;
    let record_hash = canonical_hash(&record_value)?;
    connection.execute(
        "INSERT INTO engagement_remediation_packs (
             pack_id, attempt_id, engagement_id, validation_run_id, source_hash,
             selected_columns_json, selected_columns_hash,
             include_offending_values, actor, reason, issue_count,
             membership_hash, member_count, archive_relative_path,
             archive_size, archive_sha256, manifest_hash,
             canonical_record_json, record_hash, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            pack_id,
            attempt_id,
            options.engagement_id,
            options.validation_run_id,
            options.source_hash,
            selected_columns_json,
            selected_columns_hash,
            i64::from(command.include_offending_values),
            command.actor,
            command.reason,
            sqlite_integer(
                u64::try_from(issues.len()).unwrap_or(u64::MAX),
                "issue count"
            )?,
            membership_hash,
            sqlite_integer(
                u64::try_from(archive.members.len()).unwrap_or(u64::MAX),
                "archive member count"
            )?,
            archive.relative_path,
            sqlite_integer(archive.archive_size, "archive size")?,
            archive.archive_sha256,
            archive.manifest_hash,
            canonical_record,
            record_hash,
            completed_at,
        ],
    )?;
    for member in &archive.members {
        connection.execute(
            "INSERT INTO engagement_remediation_pack_members (
                 pack_id, filename, media_type, row_count, byte_size, sha256
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                pack_id,
                member.filename,
                member.media_type,
                member
                    .row_count
                    .map(|count| sqlite_integer(count, "member row count"))
                    .transpose()?,
                sqlite_integer(
                    u64::try_from(member.bytes.len()).unwrap_or(u64::MAX),
                    "member byte size"
                )?,
                sha256_hex(&member.bytes),
            ],
        )?;
    }
    for member in issues {
        connection.execute(
            "INSERT INTO engagement_remediation_issue_members (
                 pack_id, issue_id, issue_hash, severity, rule_id, issue_code,
                 display_message, entity_id, field_id, source_file,
                 source_ordinal, line_start, line_end, offending_lineage_kind,
                 offending_source_column_ordinal, offending_source_column,
                 canonical_member_json, member_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                pack_id,
                member.issue.issue_id,
                member.issue.issue_hash,
                member.issue.severity,
                member.issue.rule_id,
                member.issue.issue_code,
                member.issue.display_message,
                member.issue.entity_id,
                member.issue.field_id,
                member.issue.source_file,
                sqlite_integer(member.issue.source_ordinal, "source ordinal")?,
                sqlite_integer(member.issue.line_start, "line start")?,
                sqlite_integer(member.issue.line_end, "line end")?,
                member.lineage_kind,
                member
                    .offending_ordinal
                    .map(|value| sqlite_integer(
                        u64::try_from(value).unwrap_or(u64::MAX),
                        "offending ordinal"
                    ))
                    .transpose()?,
                member.offending_column,
                member.canonical_json,
                member.member_hash,
            ],
        )?;
    }
    let result_value = json!({
        "attemptId": attempt_id,
        "outcome": "COMPLETE",
        "packId": pack_id,
        "recordHash": record_hash,
        "authorityHash": authority_hash,
        "completedAt": completed_at,
    });
    connection.execute(
        "INSERT INTO engagement_remediation_pack_results (
             attempt_id, outcome, pack_id, result_hash, error_code, completed_at
         ) VALUES (?1, 'COMPLETE', ?2, ?3, NULL, ?4)",
        params![
            attempt_id,
            pack_id,
            canonical_hash(&result_value)?,
            completed_at
        ],
    )?;
    Ok(())
}

#[allow(clippy::type_complexity)]
fn load_pack_for_attempt(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<RemediationPackView>> {
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
        i64,
        i64,
        i64,
        String,
        String,
        String,
        String,
    )> = connection
        .query_row(
            "SELECT pack.pack_id, pack.engagement_id, pack.validation_run_id,
                    pack.source_hash, pack.selected_columns_json,
                    pack.include_offending_values, pack.actor, pack.reason,
                    pack.issue_count, pack.member_count, pack.archive_size,
                    pack.archive_sha256, pack.membership_hash,
                    attempt.authority_hash, attempt.created_at
             FROM engagement_remediation_packs AS pack
             JOIN engagement_remediation_pack_attempts AS attempt
               ON attempt.attempt_id = pack.attempt_id
             JOIN engagement_remediation_pack_results AS result
               ON result.attempt_id = attempt.attempt_id
              AND result.outcome = 'COMPLETE' AND result.pack_id = pack.pack_id
             WHERE attempt.attempt_id = ?1",
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
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else { return Ok(None) };
    let completed_at: String = connection.query_row(
        "SELECT completed_at FROM engagement_remediation_packs WHERE pack_id = ?1",
        [&row.0],
        |record| record.get(0),
    )?;
    Ok(Some(RemediationPackView {
        pack_id: row.0,
        state: "COMPLETE".to_owned(),
        engagement_id: row.1,
        validation_run_id: row.2,
        source_hash: row.3,
        selected_key_columns: serde_json::from_str(&row.4)?,
        include_offending_values: row.5 == 1,
        actor: row.6,
        reason: row.7,
        issue_count: u64::try_from(row.8).unwrap_or(u64::MAX),
        member_count: u64::try_from(row.9).unwrap_or(u64::MAX),
        archive_size_bytes: u64::try_from(row.10).unwrap_or(u64::MAX),
        archive_sha256: row.11.clone(),
        membership_hash: row.12,
        authority_hash: row.13,
        download_token: row.11,
        created_at: row.14,
        completed_at,
    }))
}

fn load_pack_integrity_row(connection: &Connection, pack_id: &str) -> Result<PackIntegrityRow> {
    connection
        .query_row(
            "SELECT attempt_id, engagement_id, validation_run_id, source_hash,
                    selected_columns_json, selected_columns_hash,
                    include_offending_values, actor, reason, issue_count,
                    membership_hash, member_count, archive_relative_path,
                    archive_size, archive_sha256, manifest_hash,
                    canonical_record_json, record_hash, completed_at
             FROM engagement_remediation_packs WHERE pack_id = ?1",
            [pack_id],
            |row| {
                Ok(PackIntegrityRow {
                    attempt_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    validation_run_id: row.get(2)?,
                    source_hash: row.get(3)?,
                    selected_columns_json: row.get(4)?,
                    selected_columns_hash: row.get(5)?,
                    include_offending_values: row.get(6)?,
                    actor: row.get(7)?,
                    reason: row.get(8)?,
                    issue_count: row.get(9)?,
                    membership_hash: row.get(10)?,
                    member_count: row.get(11)?,
                    archive_relative_path: row.get(12)?,
                    archive_size: row.get(13)?,
                    archive_sha256: row.get(14)?,
                    manifest_hash: row.get(15)?,
                    canonical_record_json: row.get(16)?,
                    record_hash: row.get(17)?,
                    completed_at: row.get(18)?,
                })
            },
        )
        .map_err(Into::into)
}

fn integrity_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    validate_text(value, label, maximum).map_err(|_| {
        KernelError::IntegrityFailed(format!(
            "remediation history {label} violates its bounded text contract"
        ))
    })
}

fn integrity_sha(value: &str, label: &str) -> Result<()> {
    validate_sha(value, label).map_err(|_| {
        KernelError::IntegrityFailed(format!(
            "remediation history {label} is not a lowercase SHA-256 digest"
        ))
    })
}

fn validate_pack_summary_fields(view: &RemediationPackView, row: &PackIntegrityRow) -> Result<()> {
    for (value, label, maximum) in [
        (view.pack_id.as_str(), "pack ID", 256),
        (view.engagement_id.as_str(), "engagement ID", 256),
        (view.validation_run_id.as_str(), "validation run ID", 256),
        (view.actor.as_str(), "actor", 256),
        (view.reason.as_str(), "reason", 4_096),
        (view.created_at.as_str(), "created timestamp", 128),
        (view.completed_at.as_str(), "completed timestamp", 128),
    ] {
        integrity_text(value, label, maximum)?;
    }
    for (value, label) in [
        (view.source_hash.as_str(), "source hash"),
        (row.selected_columns_hash.as_str(), "selected-columns hash"),
        (view.archive_sha256.as_str(), "archive hash"),
        (view.membership_hash.as_str(), "membership hash"),
        (view.authority_hash.as_str(), "authority hash"),
        (row.manifest_hash.as_str(), "manifest hash"),
        (row.record_hash.as_str(), "record hash"),
    ] {
        integrity_sha(value, label)?;
    }
    let issue_count = u64::try_from(row.issue_count).map_err(|_| {
        KernelError::IntegrityFailed("remediation history issue count is negative".to_owned())
    })?;
    let member_count = u64::try_from(row.member_count).map_err(|_| {
        KernelError::IntegrityFailed("remediation history member count is negative".to_owned())
    })?;
    let archive_size = u64::try_from(row.archive_size).map_err(|_| {
        KernelError::IntegrityFailed("remediation history archive size is negative".to_owned())
    })?;
    if view.state != "COMPLETE"
        || !matches!(row.include_offending_values, 0 | 1)
        || issue_count > u64::try_from(MAXIMUM_ISSUES).unwrap_or(u64::MAX)
        || member_count != 3
        || archive_size == 0
        || archive_size > MAXIMUM_ARCHIVE_BYTES
        || view.issue_count != issue_count
        || view.member_count != member_count
        || view.archive_size_bytes != archive_size
        || view.download_token != view.archive_sha256
    {
        return Err(KernelError::IntegrityFailed(
            "remediation history typed summary fields violate their immutable bounds".to_owned(),
        ));
    }
    validate_archive_relative_path(&row.archive_relative_path)?;
    Ok(())
}

fn verify_pack_record_and_result(
    connection: &Connection,
    pack_id: &str,
    row: &PackIntegrityRow,
    authority_hash: &str,
) -> Result<()> {
    let record_value =
        parse_unique_json(row.canonical_record_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!("pack record JSON is invalid: {error}"))
        })?;
    let expected_record = json!({
        "schemaVersion": REMEDIATION_PACK_RECORD_SCHEMA,
        "packId": pack_id,
        "attemptId": row.attempt_id,
        "engagementId": row.engagement_id,
        "validationRunId": row.validation_run_id,
        "sourceHash": row.source_hash,
        "selectedColumnsHash": row.selected_columns_hash,
        "includeOffendingValues": row.include_offending_values == 1,
        "actor": row.actor,
        "reason": row.reason,
        "issueCount": row.issue_count,
        "membershipHash": row.membership_hash,
        "memberCount": row.member_count,
        "archiveRelativePath": row.archive_relative_path,
        "archiveSize": row.archive_size,
        "archiveSha256": row.archive_sha256,
        "manifestHash": row.manifest_hash,
        "completedAt": row.completed_at,
    });
    if canonical_json_string(&record_value)? != row.canonical_record_json
        || canonical_hash(&record_value)? != row.record_hash
        || record_value != expected_record
    {
        return Err(KernelError::IntegrityFailed(
            "pack canonical record bytes/hash/linkage differ".to_owned(),
        ));
    }
    let result: Option<(String, String, String, Option<String>, String)> = connection
        .query_row(
            "SELECT outcome, pack_id, result_hash, error_code, completed_at
             FROM engagement_remediation_pack_results WHERE attempt_id = ?1",
            [&row.attempt_id],
            |record| {
                Ok((
                    record.get(0)?,
                    record.get(1)?,
                    record.get(2)?,
                    record.get(3)?,
                    record.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((outcome, result_pack_id, result_hash, error_code, result_completed_at)) = result
    else {
        return Err(KernelError::IntegrityFailed(
            "remediation pack has no exact completed attempt result".to_owned(),
        ));
    };
    integrity_sha(&result_hash, "result hash")?;
    let expected_result = json!({
        "attemptId": row.attempt_id,
        "outcome": "COMPLETE",
        "packId": pack_id,
        "recordHash": row.record_hash,
        "authorityHash": authority_hash,
        "completedAt": row.completed_at,
    });
    if outcome != "COMPLETE"
        || result_pack_id != pack_id
        || error_code.is_some()
        || result_completed_at != row.completed_at
        || canonical_hash(&expected_result)? != result_hash
    {
        return Err(KernelError::IntegrityFailed(
            "remediation completed result hash/linkage failed replay".to_owned(),
        ));
    }
    Ok(())
}

/// Replay the bounded, value-free authority needed to serialize one history
/// item. This deliberately does not rebuild or read the data-bearing ZIP; the
/// download and global-integrity paths perform that heavier replay.
#[allow(clippy::too_many_lines)]
fn load_verified_pack_summary(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<RemediationPackView>> {
    let Some(view) = load_pack_for_attempt(connection, attempt_id)? else {
        return Ok(None);
    };
    let row = load_pack_integrity_row(connection, &view.pack_id)?;
    let attempt = load_attempt_integrity_row(connection, attempt_id)?;
    if row.attempt_id != attempt_id
        || row.engagement_id != attempt.engagement_id
        || row.validation_run_id != attempt.validation_run_id
        || row.source_hash != attempt.source_hash
        || row.actor != attempt.actor
        || row.reason != attempt.reason
        || row.include_offending_values != attempt.include_offending_values
        || row.completed_at != view.completed_at
    {
        return Err(KernelError::IntegrityFailed(
            "remediation history summary differs from its immutable attempt".to_owned(),
        ));
    }
    let command: CreateRemediationPackCommand =
        serde_json::from_str(&attempt.canonical_request_json)?;
    let selected_value =
        parse_unique_json(row.selected_columns_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "remediation history selected-column JSON is invalid: {error}"
            ))
        })?;
    let selections: Vec<RemediationKeySelection> = serde_json::from_value(selected_value.clone())?;
    if canonical_json_string(&selected_value)? != row.selected_columns_json
        || canonical_hash(&selected_value)? != row.selected_columns_hash
        || selections != command.selected_key_columns
        || selections != view.selected_key_columns
    {
        return Err(KernelError::IntegrityFailed(
            "remediation history selected columns failed exact attempt replay".to_owned(),
        ));
    }
    validate_pack_summary_fields(&view, &row)?;
    verify_pack_record_and_result(connection, &view.pack_id, &row, &attempt.authority_hash)?;
    Ok(Some(view))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn verify_integrity(
    workspace: &Path,
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<RemediationIntegritySummary> {
    let mut summary = RemediationIntegritySummary::default();
    let has_remediation_attempts: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM engagement_remediation_pack_attempts)",
        [],
        |row| row.get(0),
    )?;
    if let Err(error) = verify_v14_schema_authorities(connection, has_remediation_attempts) {
        issues.push(format!(
            "v14 remediation/mapping schema authority failed replay: {error}"
        ));
    }
    let authority_ids = collect_ids(
        connection,
        "SELECT authority_id FROM engagement_mapping_authorities ORDER BY authority_id",
    )?;
    summary.mapping_authorities = authority_ids.len();
    for authority_id in authority_ids {
        if let Err(error) = verify_mapping_authority_row(connection, &authority_id) {
            issues.push(format!(
                "mapping authority {authority_id} integrity replay failed: {error}"
            ));
        }
    }
    let attempt_ids = collect_ids(
        connection,
        "SELECT attempt_id FROM engagement_remediation_pack_attempts ORDER BY attempt_id",
    )?;
    summary.attempts = attempt_ids.len();
    for attempt_id in &attempt_ids {
        if let Err(error) = verify_attempt_row(connection, attempt_id) {
            issues.push(format!(
                "remediation attempt {attempt_id} integrity replay failed: {error}"
            ));
        }
    }
    for result in load_remediation_terminal_results(connection)? {
        if let Err(error) = verify_remediation_terminal_result(connection, &result) {
            issues.push(format!(
                "remediation result for attempt {} integrity replay failed: {error}",
                result.attempt_id
            ));
        }
    }
    let pack_ids = collect_ids(
        connection,
        "SELECT pack_id FROM engagement_remediation_packs ORDER BY pack_id",
    )?;
    summary.packs = pack_ids.len();
    for pack_id in &pack_ids {
        match verify_pack_row(workspace, connection, pack_id) {
            Ok((members, issue_members)) => {
                summary.members = summary.members.saturating_add(members);
                summary.issue_members = summary.issue_members.saturating_add(issue_members);
            }
            Err(error) => issues.push(format!(
                "remediation pack {pack_id} integrity replay failed: {error}"
            )),
        }
    }
    verify_no_untracked_archives(workspace, connection, issues)?;
    Ok(summary)
}

fn load_remediation_terminal_results(
    connection: &Connection,
) -> Result<Vec<RemediationTerminalResultRow>> {
    let mut statement = connection.prepare(
        "SELECT attempt_id, outcome, pack_id, result_hash, error_code, completed_at
         FROM engagement_remediation_pack_results
         ORDER BY attempt_id, result_hash",
    )?;
    statement
        .query_map([], |row| {
            Ok(RemediationTerminalResultRow {
                attempt_id: row.get(0)?,
                outcome: row.get(1)?,
                pack_id: row.get(2)?,
                result_hash: row.get(3)?,
                error_code: row.get(4)?,
                completed_at: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn verify_remediation_terminal_result(
    connection: &Connection,
    result: &RemediationTerminalResultRow,
) -> Result<()> {
    let pack_id = verify_remediation_terminal_result_shape(connection, result)?;
    let attempt = load_attempt_integrity_row(connection, &result.attempt_id)?;
    if attempt.planned_pack_id != pack_id {
        return Err(KernelError::IntegrityFailed(
            "completed remediation result differs from the attempt's planned pack".to_owned(),
        ));
    }
    let pack = load_pack_integrity_row(connection, &pack_id)?;
    if pack.attempt_id != result.attempt_id || pack.completed_at != result.completed_at {
        return Err(KernelError::IntegrityFailed(
            "completed remediation result differs from its exact pack record".to_owned(),
        ));
    }
    verify_pack_record_and_result(connection, &pack_id, &pack, &attempt.authority_hash)
}

fn verify_remediation_terminal_result_shape(
    connection: &Connection,
    result: &RemediationTerminalResultRow,
) -> Result<String> {
    integrity_text(&result.attempt_id, "result attempt ID", 256)?;
    integrity_text(&result.completed_at, "result completion timestamp", 128)?;
    integrity_sha(&result.result_hash, "result hash")?;
    if result.outcome != "COMPLETE" {
        return Err(KernelError::IntegrityFailed(format!(
            "remediation result outcome {:?} has no implemented canonical terminal semantics",
            result.outcome
        )));
    }
    if result.error_code.is_some() {
        return Err(KernelError::IntegrityFailed(
            "completed remediation result unexpectedly contains an error code".to_owned(),
        ));
    }
    let pack_id = result.pack_id.as_deref().ok_or_else(|| {
        KernelError::IntegrityFailed("completed remediation result has no pack ID".to_owned())
    })?;
    integrity_text(pack_id, "result pack ID", 256)?;
    let counts: (i64, i64, i64, i64, i64, i64) = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM engagement_remediation_pack_attempts
              WHERE attempt_id = ?1),
             (SELECT COUNT(*) FROM engagement_remediation_pack_attempts
              WHERE attempt_id = ?1 AND planned_pack_id = ?2),
             (SELECT COUNT(*) FROM engagement_remediation_packs
              WHERE pack_id = ?2 AND attempt_id = ?1 AND completed_at = ?3),
             (SELECT COUNT(*) FROM engagement_remediation_packs
              WHERE attempt_id = ?1),
             (SELECT COUNT(*) FROM engagement_remediation_pack_results
              WHERE attempt_id = ?1),
             (SELECT COUNT(*) FROM engagement_remediation_pack_results
              WHERE pack_id = ?2)",
        params![result.attempt_id, pack_id, result.completed_at],
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
    if counts != (1, 1, 1, 1, 1, 1) {
        return Err(KernelError::IntegrityFailed(format!(
            "remediation terminal graph cardinality is {counts:?}; expected (1, 1, 1, 1, 1, 1)"
        )));
    }
    Ok(pack_id.to_owned())
}

fn collect_ids(connection: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn verify_mapping_authority_row(connection: &Connection, authority_id: &str) -> Result<()> {
    let row: (String, String, String, String, String, String, String) = connection.query_row(
        "SELECT engagement_id, job_id, source_hash, mapping_id, mapping_version,
                mapping_hash, authority_hash
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
            ))
        },
    )?;
    let loaded =
        load_mapping_authority(connection, &row.0, &row.1, &row.2, &row.3, &row.4, &row.5)?
            .ok_or_else(|| {
                KernelError::IntegrityFailed("mapping authority disappeared".to_owned())
            })?;
    let value = parse_unique_json(loaded.canonical_json.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!("mapping authority JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&value)? != loaded.canonical_json
        || canonical_hash(&value)? != loaded.authority_hash
        || loaded.authority_id != authority_id
        || loaded.authority_hash != row.6
    {
        return Err(KernelError::IntegrityFailed(
            "mapping authority canonical bytes/hash differ".to_owned(),
        ));
    }
    resolve_mapping_authority(
        connection,
        &row.0,
        &row.1,
        &row.2,
        &row.3,
        &row.4,
        &row.5,
        if loaded.authority_kind == "OPERATOR_APPROVAL" {
            "OPERATOR_GOVERNED"
        } else {
            "CONTROLLED_SUGGESTIONS"
        },
        loaded.mapping_approval_id.as_deref(),
        loaded.mapping_approval_hash.as_deref(),
    )?;
    Ok(())
}

fn load_attempt_integrity_row(
    connection: &Connection,
    attempt_id: &str,
) -> Result<AttemptIntegrityRow> {
    connection
        .query_row(
            "SELECT planned_pack_id, engagement_id, idempotency_key,
                request_hash, canonical_request_json, expected_generation,
                workspace_id, workspace_descriptor_hash, job_id, source_hash,
                manifest_hash, profiling_run_id, profiling_result_hash,
                profile_id, profile_version, profile_hash,
                profile_approval_id, profile_approval_hash,
                mapping_id, mapping_version, mapping_hash,
                mapping_authority_id, mapping_authority_hash,
                validation_run_id, validation_result_hash, options_hash,
                actor, reason, local_export_acknowledgement,
                include_offending_values, offending_values_acknowledgement,
                canonical_authority_json, authority_hash, created_at
         FROM engagement_remediation_pack_attempts WHERE attempt_id = ?1",
            [attempt_id],
            |row| {
                Ok(AttemptIntegrityRow {
                    planned_pack_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    idempotency_key: row.get(2)?,
                    request_hash: row.get(3)?,
                    canonical_request_json: row.get(4)?,
                    expected_generation: row.get(5)?,
                    workspace_id: row.get(6)?,
                    workspace_descriptor_hash: row.get(7)?,
                    job_id: row.get(8)?,
                    source_hash: row.get(9)?,
                    manifest_hash: row.get(10)?,
                    profiling_run_id: row.get(11)?,
                    profiling_result_hash: row.get(12)?,
                    profile_id: row.get(13)?,
                    profile_version: row.get(14)?,
                    profile_hash: row.get(15)?,
                    profile_approval_id: row.get(16)?,
                    profile_approval_hash: row.get(17)?,
                    mapping_id: row.get(18)?,
                    mapping_version: row.get(19)?,
                    mapping_hash: row.get(20)?,
                    mapping_authority_id: row.get(21)?,
                    mapping_authority_hash: row.get(22)?,
                    validation_run_id: row.get(23)?,
                    validation_result_hash: row.get(24)?,
                    options_hash: row.get(25)?,
                    actor: row.get(26)?,
                    reason: row.get(27)?,
                    local_export_acknowledgement: row.get(28)?,
                    include_offending_values: row.get(29)?,
                    offending_values_acknowledgement: row.get(30)?,
                    canonical_authority_json: row.get(31)?,
                    authority_hash: row.get(32)?,
                    created_at: row.get(33)?,
                })
            },
        )
        .map_err(Into::into)
}

#[allow(clippy::too_many_lines)]
fn verify_attempt_row(connection: &Connection, attempt_id: &str) -> Result<()> {
    let row = load_attempt_integrity_row(connection, attempt_id)?;
    let request_value =
        parse_unique_json(row.canonical_request_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!("attempt request JSON is invalid: {error}"))
        })?;
    let authority_value =
        parse_unique_json(row.canonical_authority_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!("attempt authority JSON is invalid: {error}"))
        })?;
    for (label, value, json_bytes, hash) in [
        (
            "request",
            &request_value,
            row.canonical_request_json.as_str(),
            row.request_hash.as_str(),
        ),
        (
            "authority",
            &authority_value,
            row.canonical_authority_json.as_str(),
            row.authority_hash.as_str(),
        ),
    ] {
        if canonical_json_string(value)? != json_bytes || canonical_hash(value)? != hash {
            return Err(KernelError::IntegrityFailed(format!(
                "attempt {label} canonical bytes/hash differ"
            )));
        }
    }
    let command: CreateRemediationPackCommand = serde_json::from_value(request_value)?;
    validate_command(&command)?;
    let expected_generation = u64::try_from(row.expected_generation).map_err(|_| {
        KernelError::IntegrityFailed("remediation attempt generation is negative".to_owned())
    })?;
    if command.idempotency_key != row.idempotency_key
        || command.expected_engagement_generation != expected_generation
        || command.expected_validation_run_id != row.validation_run_id
        || command.expected_options_hash != row.options_hash
        || command.actor != row.actor
        || command.reason != row.reason
        || command.local_data_export_acknowledgement != row.local_export_acknowledgement
        || i64::from(command.include_offending_values) != row.include_offending_values
        || command.offending_values_acknowledgement != row.offending_values_acknowledgement
    {
        return Err(KernelError::IntegrityFailed(
            "remediation typed request differs from immutable attempt columns".to_owned(),
        ));
    }
    let stored_mapping = load_stored_mapping(connection, &row.mapping_id, &row.mapping_version)?;
    if stored_mapping.mapping_hash != row.mapping_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation attempt mapping failed replay".to_owned(),
        ));
    }
    let profile = crate::profile_registry::load_verified_profile_document(
        connection,
        &row.profile_id,
        &row.profile_version,
        &row.profile_hash,
    )?;
    let issue_count: i64 = connection.query_row(
        "SELECT issue_count FROM validation_runs
         WHERE id = ?1 AND state = 'COMPLETE' AND result_hash = ?2
           AND job_id = ?3 AND source_hash = ?4
           AND profile_id = ?5 AND profile_version = ?6 AND profile_hash = ?7
           AND mapping_id = ?8 AND mapping_version = ?9 AND mapping_hash = ?10",
        params![
            row.validation_run_id,
            row.validation_result_hash,
            row.job_id,
            row.source_hash,
            row.profile_id,
            row.profile_version,
            row.profile_hash,
            row.mapping_id,
            row.mapping_version,
            row.mapping_hash,
        ],
        |record| record.get(0),
    )?;
    crate::validation::verify_validation_run_for_read(connection, &row.validation_run_id)?;
    let files =
        remediation_file_options(connection, &row.job_id, &stored_mapping.document, &profile)?;
    let mut options = RemediationPackOptionsView {
        schema_version: REMEDIATION_PACK_OPTIONS_SCHEMA.to_owned(),
        engagement_id: row.engagement_id.clone(),
        engagement_generation: expected_generation,
        workspace_id: row.workspace_id.clone(),
        workspace_descriptor_hash: row.workspace_descriptor_hash.clone(),
        job_id: row.job_id.clone(),
        source_hash: row.source_hash.clone(),
        manifest_hash: row.manifest_hash.clone(),
        profiling_run_id: row.profiling_run_id.clone(),
        profiling_result_hash: row.profiling_result_hash.clone(),
        profile_id: row.profile_id.clone(),
        profile_version: row.profile_version.clone(),
        profile_hash: row.profile_hash.clone(),
        profile_approval_id: row.profile_approval_id.clone(),
        profile_approval_hash: row.profile_approval_hash.clone(),
        mapping_id: row.mapping_id.clone(),
        mapping_version: row.mapping_version.clone(),
        mapping_hash: row.mapping_hash.clone(),
        mapping_authority_id: row.mapping_authority_id.clone(),
        mapping_authority_hash: row.mapping_authority_hash.clone(),
        validation_run_id: row.validation_run_id.clone(),
        validation_result_hash: row.validation_result_hash.clone(),
        issue_count: u64::try_from(issue_count).map_err(|_| {
            KernelError::IntegrityFailed("remediation issue count is negative".to_owned())
        })?,
        options_hash: String::new(),
        files,
    };
    options.options_hash = options_hash(&options)?;
    if options.options_hash != row.options_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation options hash failed immutable evidence replay".to_owned(),
        ));
    }
    let selections = validate_selections(&options, &command.selected_key_columns)?;
    let (_, _, _, stored_authority_json, stored_selections, _, _, _) =
        load_attempt_for_integrity(connection, attempt_id)?;
    if selections != stored_selections || stored_authority_json != row.canonical_authority_json {
        return Err(KernelError::IntegrityFailed(
            "remediation selected-key membership differs from exact request/attempt".to_owned(),
        ));
    }
    let expected_authority = remediation_attempt_authority_value(
        attempt_id,
        &row.planned_pack_id,
        &options,
        &command,
        &selections,
        &row.request_hash,
        &row.created_at,
    );
    if canonical_json_string(&expected_authority)? != row.canonical_authority_json
        || canonical_hash(&expected_authority)? != row.authority_hash
    {
        return Err(KernelError::IntegrityFailed(
            "remediation attempt authority differs from exact linked evidence".to_owned(),
        ));
    }
    let workspace_exact: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM object_workspace_bindings workspace
             JOIN object_workspace_engagement_bindings binding
               ON binding.workspace_id = workspace.workspace_id
             WHERE workspace.workspace_id = ?1 AND workspace.descriptor_hash = ?2
               AND binding.engagement_id = ?3
         )",
        params![
            row.workspace_id,
            row.workspace_descriptor_hash,
            row.engagement_id
        ],
        |record| record.get(0),
    )?;
    let source_exact: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM jobs job
             JOIN profiling_runs profiling ON profiling.id = ?2
             JOIN engagement_acceptance_attempts acceptance
               ON acceptance.job_id = job.id AND acceptance.profiling_run_id = profiling.id
             JOIN engagement_source_preflights preflight
               ON preflight.preflight_id = acceptance.preflight_id
             WHERE job.id = ?1 AND job.state = 'COMPLETE' AND job.source_hash = ?3
               AND profiling.job_id = job.id AND profiling.state = 'COMPLETE'
               AND profiling.result_hash = ?4
               AND acceptance.engagement_id = ?5
               AND acceptance.accepted_source_hash = ?3
               AND acceptance.accepted_manifest_hash = ?6
               AND preflight.manifest_hash = ?6
         )",
        params![
            row.job_id,
            row.profiling_run_id,
            row.source_hash,
            row.profiling_result_hash,
            row.engagement_id,
            row.manifest_hash,
        ],
        |record| record.get(0),
    )?;
    let profile_approval_exact: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM profile_approvals
             WHERE approval_id = ?1 AND approval_hash = ?2
               AND profile_id = ?3 AND profile_version = ?4 AND profile_hash = ?5
         )",
        params![
            row.profile_approval_id,
            row.profile_approval_hash,
            row.profile_id,
            row.profile_version,
            row.profile_hash,
        ],
        |record| record.get(0),
    )?;
    if !workspace_exact || !source_exact || !profile_approval_exact {
        return Err(KernelError::IntegrityFailed(
            "remediation attempt source/workspace/profile/validation linkage failed replay"
                .to_owned(),
        ));
    }
    let validation_authority = replay_validation_authority(
        connection,
        &ValidationEvidenceIdentity {
            engagement_id: row.engagement_id.clone(),
            job_id: row.job_id.clone(),
            source_hash: row.source_hash.clone(),
            manifest_hash: row.manifest_hash.clone(),
            profiling_run_id: row.profiling_run_id.clone(),
            profiling_result_hash: row.profiling_result_hash.clone(),
            profile_id: row.profile_id.clone(),
            profile_version: row.profile_version.clone(),
            profile_hash: row.profile_hash.clone(),
            mapping_id: row.mapping_id.clone(),
            mapping_version: row.mapping_version.clone(),
            mapping_hash: row.mapping_hash.clone(),
            validation_run_id: row.validation_run_id.clone(),
            validation_result_hash: row.validation_result_hash.clone(),
        },
        false,
    )?;
    if validation_authority.profile_approval_id != row.profile_approval_id
        || validation_authority.profile_approval_hash != row.profile_approval_hash
        || validation_authority.mapping_authority.authority_id != row.mapping_authority_id
        || validation_authority.mapping_authority.authority_hash != row.mapping_authority_hash
    {
        return Err(KernelError::IntegrityFailed(
            "remediation attempt profile/mapping validation authority differs".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_issue_member_rows(
    connection: &Connection,
    pack_id: &str,
    validation_run_id: &str,
    validation_result_hash: &str,
    job_id: &str,
) -> Result<Vec<String>> {
    let rows = {
        let mut statement = connection.prepare(
            "SELECT issue_id, issue_hash, severity, rule_id, issue_code,
                    display_message, entity_id, field_id, source_file,
                    source_ordinal, line_start, line_end,
                    offending_lineage_kind, offending_source_column_ordinal,
                    offending_source_column, canonical_member_json, member_hash
             FROM engagement_remediation_issue_members
             WHERE pack_id = ?1 ORDER BY issue_id",
        )?;
        statement
            .query_map([pack_id], |row| {
                Ok(IssueMemberIntegrityRow {
                    issue_id: row.get(0)?,
                    issue_hash: row.get(1)?,
                    severity: row.get(2)?,
                    rule_id: row.get(3)?,
                    issue_code: row.get(4)?,
                    display_message: row.get(5)?,
                    entity_id: row.get(6)?,
                    field_id: row.get(7)?,
                    source_file: row.get(8)?,
                    source_ordinal: row.get(9)?,
                    line_start: row.get(10)?,
                    line_end: row.get(11)?,
                    offending_lineage_kind: row.get(12)?,
                    offending_source_column_ordinal: row.get(13)?,
                    offending_source_column: row.get(14)?,
                    canonical_member_json: row.get(15)?,
                    member_hash: row.get(16)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for page in rows.chunks(200) {
        let issue_ids = page
            .iter()
            .map(|row| row.issue_id.clone())
            .collect::<Vec<_>>();
        crate::validation::verify_validation_issue_page_for_read(
            connection,
            validation_run_id,
            &issue_ids,
        )?;
    }
    let accepted_headers = load_accepted_headers(connection, job_id)?;
    let mut hashes = Vec::with_capacity(rows.len());
    for row in rows {
        let issue: IssueRow = connection.query_row(
            "SELECT issue_id, issue_hash, severity, rule_id, issue_code,
                    display_message, entity_id, field_id, source_file,
                    source_ordinal, line_start, line_end, lineage_json
             FROM validation_issues WHERE run_id = ?1 AND issue_id = ?2",
            params![validation_run_id, row.issue_id],
            |issue| {
                Ok(IssueRow {
                    issue_id: issue.get(0)?,
                    issue_hash: issue.get(1)?,
                    severity: issue.get(2)?,
                    rule_id: issue.get(3)?,
                    issue_code: issue.get(4)?,
                    display_message: issue.get(5)?,
                    entity_id: issue.get(6)?,
                    field_id: issue.get(7)?,
                    source_file: issue.get(8)?,
                    source_ordinal: u64::try_from(issue.get::<_, i64>(9)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, i64::MIN))?,
                    line_start: u64::try_from(issue.get::<_, i64>(10)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, i64::MIN))?,
                    line_end: u64::try_from(issue.get::<_, i64>(11)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, i64::MIN))?,
                    lineage_json: issue.get(12)?,
                })
            },
        )?;
        let lineage = parse_unique_json(issue.lineage_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "validation issue {} lineage is invalid: {error}",
                row.issue_id
            ))
        })?;
        let (lineage_kind, offending_ordinal, offending_column) =
            exact_offending_lineage(&issue, &lineage, &accepted_headers)?;
        let typed_equal = row.issue_id == issue.issue_id
            && row.issue_hash == issue.issue_hash
            && row.severity == issue.severity
            && row.rule_id == issue.rule_id
            && row.issue_code == issue.issue_code
            && row.display_message == issue.display_message
            && row.entity_id == issue.entity_id
            && row.field_id == issue.field_id
            && row.source_file == issue.source_file
            && u64::try_from(row.source_ordinal).ok() == Some(issue.source_ordinal)
            && u64::try_from(row.line_start).ok() == Some(issue.line_start)
            && u64::try_from(row.line_end).ok() == Some(issue.line_end)
            && row.offending_lineage_kind == lineage_kind
            && row
                .offending_source_column_ordinal
                .and_then(|value| usize::try_from(value).ok())
                == offending_ordinal
            && row.offending_source_column == offending_column;
        if !typed_equal {
            return Err(KernelError::IntegrityFailed(format!(
                "remediation issue member {} differs from exact validation/header lineage",
                row.issue_id
            )));
        }
        let expected = json!({
            "schemaVersion": REMEDIATION_ISSUE_MEMBER_SCHEMA,
            "validationRunId": validation_run_id,
            "validationResultHash": validation_result_hash,
            "issueId": issue.issue_id,
            "issueHash": issue.issue_hash,
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
            "offendingLineageKind": lineage_kind,
            "offendingSourceColumnOrdinal": offending_ordinal,
            "offendingSourceColumn": offending_column,
        });
        if canonical_json_string(&expected)? != row.canonical_member_json
            || canonical_hash(&expected)? != row.member_hash
        {
            return Err(KernelError::IntegrityFailed(format!(
                "remediation issue member {} canonical bytes/hash failed replay",
                row.issue_id
            )));
        }
        hashes.push(row.member_hash);
    }
    Ok(hashes)
}

#[allow(clippy::too_many_lines)]
fn verify_pack_row(
    workspace: &Path,
    connection: &Connection,
    pack_id: &str,
) -> Result<(usize, usize)> {
    let row = load_pack_integrity_row(connection, pack_id)?;
    let attempt = load_attempt_integrity_row(connection, &row.attempt_id)?;
    let selected_value =
        parse_unique_json(row.selected_columns_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!("selected-column JSON is invalid: {error}"))
        })?;
    let selected: Vec<RemediationKeySelection> = serde_json::from_value(selected_value.clone())?;
    if canonical_json_string(&selected_value)? != row.selected_columns_json
        || canonical_hash(&selected_value)? != row.selected_columns_hash
        || selected.is_empty()
        || selected.len() > MAXIMUM_SELECTIONS
    {
        return Err(KernelError::IntegrityFailed(
            "pack selected-column bytes/hash/membership differ".to_owned(),
        ));
    }
    verify_pack_record_and_result(connection, pack_id, &row, &attempt.authority_hash)?;
    let path = checked_archive_path(workspace, &row.archive_relative_path)?;
    let archive_bytes = read_verified_archive(
        &path,
        &row.archive_sha256,
        u64::try_from(row.archive_size).unwrap_or(u64::MAX),
    )?;
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|error| KernelError::IntegrityFailed(error.to_string()))?;
    if archive.len() != 3 {
        return Err(KernelError::IntegrityFailed(
            "remediation archive must contain exactly three members".to_owned(),
        ));
    }
    let db_members = load_db_members(connection, pack_id)?;
    if db_members.len() != 3 {
        return Err(KernelError::IntegrityFailed(
            "remediation member authority must contain exactly three rows".to_owned(),
        ));
    }
    let issue_count = Some(row.issue_count);
    let member_metadata_exact = db_members.get("manifest.json").is_some_and(|member| {
        member.media_type == "application/json" && member.row_count.is_none()
    }) && db_members.get("remediation-issues.json").is_some_and(
        |member| member.media_type == "application/json" && member.row_count == issue_count,
    ) && db_members
        .get("remediation-issues.csv")
        .is_some_and(|member| member.media_type == "text/csv" && member.row_count == issue_count);
    if !member_metadata_exact {
        return Err(KernelError::IntegrityFailed(
            "remediation member media/row-count authority differs".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut manifest_bytes = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| KernelError::IntegrityFailed(error.to_string()))?;
        let entry_name = entry
            .name()
            .map_err(|error| KernelError::IntegrityFailed(error.to_string()))?
            .into_owned();
        if !entry.is_file() || !seen.insert(entry_name.clone()) {
            return Err(KernelError::IntegrityFailed(
                "remediation ZIP contains a non-file or duplicate member".to_owned(),
            ));
        }
        let expected = db_members.get(&entry_name).ok_or_else(|| {
            KernelError::IntegrityFailed("remediation ZIP has an undeclared member".to_owned())
        })?;
        let mut bytes = Vec::new();
        (&mut entry)
            .take(
                u64::try_from(expected.byte_size)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)
            .map_err(|error| KernelError::io(&path, error))?;
        if bytes.len() != expected.byte_size || sha256_hex(&bytes) != expected.sha256 {
            return Err(KernelError::IntegrityFailed(
                "remediation ZIP member differs from immutable member evidence".to_owned(),
            ));
        }
        if entry_name == "manifest.json" {
            if sha256_hex(&bytes) != row.manifest_hash {
                return Err(KernelError::IntegrityFailed(
                    "remediation manifest hash differs".to_owned(),
                ));
            }
            manifest_bytes = Some(bytes);
        }
    }
    let job_id: String = connection.query_row(
        "SELECT job_id FROM engagement_remediation_pack_attempts WHERE attempt_id = ?1",
        [&row.attempt_id],
        |record| record.get(0),
    )?;
    let issue_hashes = verify_issue_member_rows(
        connection,
        pack_id,
        &row.validation_run_id,
        &connection.query_row(
            "SELECT validation_result_hash FROM engagement_remediation_pack_attempts
             WHERE attempt_id = ?1",
            [&row.attempt_id],
            |record| record.get::<_, String>(0),
        )?,
        &job_id,
    )?;
    if i64::try_from(issue_hashes.len()).unwrap_or(i64::MAX) != row.issue_count {
        return Err(KernelError::IntegrityFailed(
            "remediation issue membership count differs".to_owned(),
        ));
    }
    let expected_membership_hash = canonical_hash(&json!({
        "schemaVersion": "migration-stream/engagement/remediation-membership/v1",
        "members": issue_hashes,
    }))?;
    if expected_membership_hash != row.membership_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation issue membership hash differs".to_owned(),
        ));
    }
    let selections: Vec<RemediationKeySelection> =
        serde_json::from_str(&row.selected_columns_json)?;
    let command: CreateRemediationPackCommand =
        serde_json::from_str(&attempt.canonical_request_json)?;
    let mapping = load_stored_mapping(connection, &attempt.mapping_id, &attempt.mapping_version)?;
    let profile = crate::profile_registry::load_verified_profile_document(
        connection,
        &attempt.profile_id,
        &attempt.profile_version,
        &attempt.profile_hash,
    )?;
    let mut replay_options = RemediationPackOptionsView {
        schema_version: REMEDIATION_PACK_OPTIONS_SCHEMA.to_owned(),
        engagement_id: attempt.engagement_id.clone(),
        engagement_generation: u64::try_from(attempt.expected_generation).map_err(|_| {
            KernelError::IntegrityFailed("remediation attempt generation is negative".to_owned())
        })?,
        workspace_id: attempt.workspace_id.clone(),
        workspace_descriptor_hash: attempt.workspace_descriptor_hash.clone(),
        job_id: attempt.job_id.clone(),
        source_hash: attempt.source_hash.clone(),
        manifest_hash: attempt.manifest_hash.clone(),
        profiling_run_id: attempt.profiling_run_id.clone(),
        profiling_result_hash: attempt.profiling_result_hash.clone(),
        profile_id: attempt.profile_id.clone(),
        profile_version: attempt.profile_version.clone(),
        profile_hash: attempt.profile_hash.clone(),
        profile_approval_id: attempt.profile_approval_id.clone(),
        profile_approval_hash: attempt.profile_approval_hash.clone(),
        mapping_id: attempt.mapping_id.clone(),
        mapping_version: attempt.mapping_version.clone(),
        mapping_hash: attempt.mapping_hash.clone(),
        mapping_authority_id: attempt.mapping_authority_id.clone(),
        mapping_authority_hash: attempt.mapping_authority_hash.clone(),
        validation_run_id: attempt.validation_run_id.clone(),
        validation_result_hash: attempt.validation_result_hash.clone(),
        issue_count: u64::try_from(row.issue_count).map_err(|_| {
            KernelError::IntegrityFailed("remediation pack issue count is negative".to_owned())
        })?,
        options_hash: String::new(),
        files: remediation_file_options(connection, &attempt.job_id, &mapping.document, &profile)?,
    };
    replay_options.options_hash = options_hash(&replay_options)?;
    if replay_options.options_hash != attempt.options_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation pack options failed replay".to_owned(),
        ));
    }
    let replay_issues = load_complete_issue_members(connection, &replay_options)?;
    let (expected_archive_members, expected_manifest_hash) = build_archive_members(
        pack_id,
        &attempt.created_at,
        &replay_options,
        &command,
        &selections,
        &replay_issues,
        connection,
    )?;
    if expected_manifest_hash != row.manifest_hash {
        return Err(KernelError::IntegrityFailed(
            "remediation manifest hash differs from exact source replay".to_owned(),
        ));
    }
    for expected in &expected_archive_members {
        let stored = db_members.get(expected.filename).ok_or_else(|| {
            KernelError::IntegrityFailed("replayed remediation member is absent".to_owned())
        })?;
        let expected_row_count = expected
            .row_count
            .map(|count| i64::try_from(count).unwrap_or(i64::MAX));
        if stored.media_type != expected.media_type
            || stored.row_count != expected_row_count
            || stored.byte_size != expected.bytes.len()
            || stored.sha256 != sha256_hex(&expected.bytes)
        {
            return Err(KernelError::IntegrityFailed(format!(
                "remediation member {} differs from exact source replay",
                expected.filename
            )));
        }
    }
    let issue_members = db_members.get("remediation-issues.json").ok_or_else(|| {
        KernelError::IntegrityFailed("remediation JSON member authority is absent".to_owned())
    })?;
    let csv_member = db_members.get("remediation-issues.csv").ok_or_else(|| {
        KernelError::IntegrityFailed("remediation CSV member authority is absent".to_owned())
    })?;
    let expected_manifest = json!({
        "schemaVersion": "migration-stream/remediation-pack-manifest/v1",
        "packId": pack_id,
        "createdAt": attempt.created_at,
        "scope": "LOCAL_DATA_BEARING_REMEDIATION_ONLY_NOT_A_READINESS_PACKAGE_OR_TARGET_PAYLOAD",
        "authority": {
            "workspaceId": attempt.workspace_id,
            "workspaceDescriptorHash": attempt.workspace_descriptor_hash,
            "engagementId": attempt.engagement_id,
            "jobId": attempt.job_id,
            "sourceHash": attempt.source_hash,
            "manifestHash": attempt.manifest_hash,
            "profilingRunId": attempt.profiling_run_id,
            "profilingResultHash": attempt.profiling_result_hash,
            "profileId": attempt.profile_id,
            "profileVersion": attempt.profile_version,
            "profileHash": attempt.profile_hash,
            "profileApprovalId": attempt.profile_approval_id,
            "profileApprovalHash": attempt.profile_approval_hash,
            "mappingId": attempt.mapping_id,
            "mappingVersion": attempt.mapping_version,
            "mappingHash": attempt.mapping_hash,
            "mappingAuthorityId": attempt.mapping_authority_id,
            "mappingAuthorityHash": attempt.mapping_authority_hash,
            "validationRunId": attempt.validation_run_id,
            "validationResultHash": attempt.validation_result_hash,
        },
        "export": {
            "actor": attempt.actor,
            "reason": attempt.reason,
            "localDataExportAcknowledgement": attempt.local_export_acknowledgement,
            "includeOffendingValues": attempt.include_offending_values == 1,
            "offendingValuesAcknowledgement": attempt.offending_values_acknowledgement,
            "selectedKeyColumns": selections,
            "issueCount": issue_hashes.len(),
            "membershipHash": row.membership_hash,
        },
        "members": [
            {
                "filename": "remediation-issues.json",
                "mediaType": "application/json",
                "rowCount": issue_hashes.len(),
                "byteSize": issue_members.byte_size,
                "sha256": issue_members.sha256,
            },
            {
                "filename": "remediation-issues.csv",
                "mediaType": "text/csv",
                "rowCount": issue_hashes.len(),
                "byteSize": csv_member.byte_size,
                "sha256": csv_member.sha256,
            }
        ],
    });
    let manifest_bytes = manifest_bytes.ok_or_else(|| {
        KernelError::IntegrityFailed("remediation ZIP manifest is absent".to_owned())
    })?;
    let manifest_value = parse_unique_json(&manifest_bytes).map_err(|error| {
        KernelError::IntegrityFailed(format!("remediation manifest JSON is invalid: {error}"))
    })?;
    if manifest_value != expected_manifest
        || canonical_json_string(&manifest_value)?.as_bytes() != manifest_bytes
    {
        return Err(KernelError::IntegrityFailed(
            "remediation manifest authority/export/member graph failed replay".to_owned(),
        ));
    }
    Ok((db_members.len(), issue_hashes.len()))
}

fn load_db_members(
    connection: &Connection,
    pack_id: &str,
) -> Result<BTreeMap<String, DatabaseArchiveMember>> {
    let mut statement = connection.prepare(
        "SELECT filename, media_type, row_count, byte_size, sha256
         FROM engagement_remediation_pack_members
         WHERE pack_id = ?1 ORDER BY filename",
    )?;
    statement
        .query_map([pack_id], |row| {
            let size = row.get::<_, i64>(3)?;
            Ok((
                row.get::<_, String>(0)?,
                DatabaseArchiveMember {
                    media_type: row.get(1)?,
                    row_count: row.get(2)?,
                    byte_size: usize::try_from(size).unwrap_or(usize::MAX),
                    sha256: row.get(4)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .map_err(Into::into)
}

fn verify_no_untracked_archives(
    workspace: &Path,
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    let directory = workspace.join(REMEDIATION_DIRECTORY);
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(KernelError::io(&directory, error)),
        Ok(metadata) => {
            if let Err(error) = verify_real_directory(&directory, &metadata) {
                issues.push(format!(
                    "remediation archive namespace is not a real directory: {error}"
                ));
                return Ok(());
            }
        }
    }
    let tracked = collect_ids(
        connection,
        "SELECT archive_relative_path FROM engagement_remediation_packs ORDER BY archive_relative_path",
    )?
    .into_iter()
    .filter_map(|path| {
        Path::new(&path)
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
    })
    .collect::<HashSet<_>>();
    for entry in fs::read_dir(&directory).map_err(|error| KernelError::io(&directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(&directory, error))?;
        if !tracked.contains(&entry.file_name()) {
            issues.push(format!(
                "remediation archive namespace contains untracked entry {}",
                entry.file_name().display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(index: usize) -> RemediationKeySelection {
        RemediationKeySelection {
            entity_id: "customerAccount".to_owned(),
            source_file: "customer_accounts.csv".to_owned(),
            source_column_ordinal: index,
            source_column: format!("key_{index}"),
        }
    }

    fn terminal_graph_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("terminal graph database");
        connection
            .execute_batch(
                "CREATE TABLE engagement_remediation_pack_attempts (
                     attempt_id TEXT NOT NULL,
                     planned_pack_id TEXT NOT NULL
                 );
                 CREATE TABLE engagement_remediation_packs (
                     pack_id TEXT NOT NULL,
                     attempt_id TEXT NOT NULL,
                     completed_at TEXT NOT NULL
                 );
                 CREATE TABLE engagement_remediation_pack_results (
                     attempt_id TEXT NOT NULL,
                     outcome TEXT NOT NULL,
                     pack_id TEXT,
                     result_hash TEXT NOT NULL,
                     error_code TEXT,
                     completed_at TEXT NOT NULL
                 );",
            )
            .expect("terminal graph schema");
        connection
    }

    fn complete_terminal_result() -> RemediationTerminalResultRow {
        RemediationTerminalResultRow {
            attempt_id: "attempt-1".to_owned(),
            outcome: "COMPLETE".to_owned(),
            pack_id: Some("pack-1".to_owned()),
            result_hash: "a".repeat(64),
            error_code: None,
            completed_at: "2026-08-03T12:00:00Z".to_owned(),
        }
    }

    fn command(selections: Vec<RemediationKeySelection>) -> CreateRemediationPackCommand {
        CreateRemediationPackCommand {
            schema_version: CREATE_REMEDIATION_PACK_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: 1,
            expected_validation_run_id: "validation-run".to_owned(),
            expected_options_hash: "a".repeat(64),
            selected_key_columns: selections,
            actor: "Synthetic remediation tester".to_owned(),
            reason: "Prove bounded remediation command validation.".to_owned(),
            local_data_export_acknowledgement: LOCAL_REMEDIATION_DATA_EXPORT_ACKNOWLEDGEMENT
                .to_owned(),
            include_offending_values: false,
            offending_values_acknowledgement: None,
            idempotency_key: "remediation-unit-idempotency-0001".to_owned(),
        }
    }

    fn archive_members() -> Vec<ArchiveMember> {
        vec![
            ArchiveMember {
                filename: "manifest.json",
                media_type: "application/json",
                bytes: b"{}".to_vec(),
                row_count: None,
            },
            ArchiveMember {
                filename: "remediation-issues.json",
                media_type: "application/json",
                bytes: b"[]".to_vec(),
                row_count: Some(0),
            },
            ArchiveMember {
                filename: "remediation-issues.csv",
                media_type: "text/csv",
                bytes: b"issue_id\n".to_vec(),
                row_count: Some(0),
            },
        ]
    }

    fn selection_options() -> RemediationPackOptionsView {
        RemediationPackOptionsView {
            schema_version: REMEDIATION_PACK_OPTIONS_SCHEMA.to_owned(),
            engagement_id: "engagement".to_owned(),
            engagement_generation: 1,
            workspace_id: "workspace".to_owned(),
            workspace_descriptor_hash: "1".repeat(64),
            job_id: "job".to_owned(),
            source_hash: "2".repeat(64),
            manifest_hash: "3".repeat(64),
            profiling_run_id: "profiling".to_owned(),
            profiling_result_hash: "4".repeat(64),
            profile_id: "profile".to_owned(),
            profile_version: "1.0.0".to_owned(),
            profile_hash: "5".repeat(64),
            profile_approval_id: "profile-approval".to_owned(),
            profile_approval_hash: "6".repeat(64),
            mapping_id: "mapping".to_owned(),
            mapping_version: "1.0.0".to_owned(),
            mapping_hash: "7".repeat(64),
            mapping_authority_id: "mapping-authority".to_owned(),
            mapping_authority_hash: "8".repeat(64),
            validation_run_id: "validation-run".to_owned(),
            validation_result_hash: "9".repeat(64),
            issue_count: 1,
            options_hash: "a".repeat(64),
            files: vec![
                RemediationFileOptionView {
                    entity_id: "root".to_owned(),
                    source_file: "root.csv".to_owned(),
                    columns: vec![RemediationColumnOptionView {
                        source_column_ordinal: 0,
                        source_column: "root_key".to_owned(),
                        suggested: true,
                        selected: false,
                    }],
                },
                RemediationFileOptionView {
                    entity_id: "child".to_owned(),
                    source_file: "child.csv".to_owned(),
                    columns: vec![RemediationColumnOptionView {
                        source_column_ordinal: 1,
                        source_column: "child_key".to_owned(),
                        suggested: true,
                        selected: false,
                    }],
                },
            ],
        }
    }

    #[test]
    fn remediation_command_limits_and_separate_value_acknowledgement_fail_closed() {
        assert!(validate_command(&command(Vec::new())).is_err());
        assert!(
            validate_command(&command((0..=MAXIMUM_SELECTIONS).map(selection).collect())).is_err()
        );
        let mut missing_ack = command(vec![selection(0)]);
        missing_ack.include_offending_values = true;
        assert!(validate_command(&missing_ack).is_err());
        missing_ack.offending_values_acknowledgement =
            Some(LOCAL_REMEDIATION_OFFENDING_VALUES_EXPORT_ACKNOWLEDGEMENT.to_owned());
        assert!(validate_command(&missing_ack).is_ok());
    }

    #[test]
    fn remediation_selection_missing_duplicate_stale_and_oversize_inputs_fail_closed() {
        let options = selection_options();
        let root = RemediationKeySelection {
            entity_id: "root".to_owned(),
            source_file: "root.csv".to_owned(),
            source_column_ordinal: 0,
            source_column: "root_key".to_owned(),
        };
        let child = RemediationKeySelection {
            entity_id: "child".to_owned(),
            source_file: "child.csv".to_owned(),
            source_column_ordinal: 1,
            source_column: "child_key".to_owned(),
        };
        assert!(validate_selections(&options, std::slice::from_ref(&root)).is_err());
        assert!(
            validate_selections(&options, &[root.clone(), root.clone(), child.clone()]).is_err()
        );
        let mut stale = child.clone();
        stale.source_column = "renamed_key".to_owned();
        assert!(validate_selections(&options, &[root.clone(), stale]).is_err());
        let mut oversized = child;
        oversized.source_column = "x".repeat(1_025);
        assert!(validate_selections(&options, &[root, oversized]).is_err());
    }

    #[test]
    fn remediation_value_and_total_byte_overflow_fail_closed() {
        let mut total = 0;
        assert!(checked_export_value(&"x".repeat(MAXIMUM_VALUE_BYTES + 1), &mut total).is_err());
        let mut total = MAXIMUM_TOTAL_EXPORTED_VALUE_BYTES;
        assert!(checked_export_value("x", &mut total).is_err());
    }

    #[test]
    fn archive_publication_recovers_owned_partial_and_reuses_exact_content() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let namespace = workspace.path().join(REMEDIATION_DIRECTORY);
        fs::create_dir_all(&namespace).expect("remediation namespace");
        let partial = namespace.join(".test-pack.part");
        fs::write(&partial, b"interrupted owned bytes").expect("interrupted partial");

        let first = publish_archive(
            workspace.path(),
            "test-pack",
            archive_members(),
            "b".repeat(64),
        )
        .expect("recover partial and publish archive");
        assert!(first.cleanup.is_some());
        assert!(!partial.exists());
        let second = publish_archive(
            workspace.path(),
            "test-pack",
            archive_members(),
            "b".repeat(64),
        )
        .expect("reuse exact content-addressed archive");
        assert!(second.cleanup.is_none());
        assert_eq!(second.archive_sha256, first.archive_sha256);
        assert_eq!(second.archive_size, first.archive_size);
        assert!(!partial.exists());
    }

    #[test]
    fn post_publication_registration_failure_removes_owned_data_archive() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let archive = publish_archive(
            workspace.path(),
            "post-publication-failure-pack",
            archive_members(),
            "d".repeat(64),
        )
        .expect("publish owned archive");
        let path = workspace.path().join(&archive.relative_path);
        assert!(path.is_file());

        let error = archive
            .complete_registration(|_| {
                Err(KernelError::IntegrityFailed(
                    "injected post-publication database replay failure".to_owned(),
                ))
            })
            .expect_err("registration failure must be returned");
        assert!(error.to_string().contains("injected post-publication"));
        assert!(!path.exists(), "unregistered data archive must be removed");
    }

    #[test]
    fn partial_archive_write_failure_removes_owned_partial() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        ARCHIVE_WRITE_FAILURE_AFTER_MEMBER.with(|checkpoint| checkpoint.set(Some(0)));
        let error = publish_archive(
            workspace.path(),
            "partial-write-failure-pack",
            archive_members(),
            "e".repeat(64),
        )
        .expect_err("injected partial write must fail");
        assert!(error.to_string().contains("partial-write failure"));
        let namespace = workspace.path().join(REMEDIATION_DIRECTORY);
        assert!(
            fs::read_dir(&namespace)
                .expect("read remediation namespace")
                .next()
                .is_none(),
            "partial-write failure must leave no partial or final archive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_tamper_and_hard_link_alias_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = tempfile::tempdir().expect("temporary workspace");
        let published = publish_archive(
            workspace.path(),
            "tamper-pack",
            archive_members(),
            "c".repeat(64),
        )
        .expect("publish archive");
        let path = workspace.path().join(&published.relative_path);
        let alias = workspace.path().join("archive-alias.zip");
        fs::hard_link(&path, &alias).expect("hard-link alias");
        assert!(
            verify_archive_file(&path, &published.archive_sha256, published.archive_size).is_err(),
            "hard-link aliases must fail closed"
        );
        fs::remove_file(&alias).expect("remove test alias");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("make test writable");
        fs::write(&path, b"tampered archive bytes").expect("tamper archive");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("restore mode");
        assert!(
            publish_archive(
                workspace.path(),
                "tamper-pack",
                archive_members(),
                "c".repeat(64),
            )
            .is_err(),
            "an existing content-addressed name with altered bytes must not be reused"
        );
        assert!(
            !workspace
                .path()
                .join(REMEDIATION_DIRECTORY)
                .join(".tamper-pack.part")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn remediation_namespace_symlink_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("temporary workspace");
        let outside = tempfile::tempdir().expect("outside directory");
        let engagement = workspace.path().join("engagement");
        fs::create_dir(&engagement).expect("engagement directory");
        symlink(outside.path(), engagement.join("remediation-packs"))
            .expect("remediation namespace symlink");
        assert!(ensure_real_directory(workspace.path(), Path::new(REMEDIATION_DIRECTORY)).is_err());
        assert!(
            fs::read_dir(outside.path())
                .expect("outside directory remains")
                .next()
                .is_none(),
            "failed namespace validation must not touch the external target"
        );
    }

    #[test]
    fn csv_formula_prefixes_are_rendered_inert() {
        for value in ["=SUM(A1:A2)", "+1", "-1", "@command", "\tvalue", "\rvalue"] {
            assert_eq!(formula_safe(value), format!("'{value}"));
        }
        assert_eq!(formula_safe("ordinary"), "ordinary");
    }

    #[test]
    fn orphan_complete_remediation_result_fails_terminal_graph_replay() {
        let connection = terminal_graph_connection();
        let result = complete_terminal_result();
        connection
            .execute(
                "INSERT INTO engagement_remediation_pack_attempts
                 (attempt_id, planned_pack_id) VALUES (?1, ?2)",
                params![result.attempt_id, result.pack_id],
            )
            .expect("attempt row");
        connection
            .execute(
                "INSERT INTO engagement_remediation_pack_results
                 (attempt_id, outcome, pack_id, result_hash, error_code, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    result.attempt_id,
                    result.outcome,
                    result.pack_id,
                    result.result_hash,
                    result.error_code,
                    result.completed_at,
                ],
            )
            .expect("orphan result row");
        let stored = load_remediation_terminal_results(&connection).expect("enumerate results");
        assert_eq!(stored.len(), 1);
        assert!(verify_remediation_terminal_result_shape(&connection, &stored[0]).is_err());
    }

    #[test]
    fn unsupported_failed_remediation_result_fails_closed() {
        let connection = terminal_graph_connection();
        let failed = RemediationTerminalResultRow {
            attempt_id: "attempt-failed".to_owned(),
            outcome: "FAILED".to_owned(),
            pack_id: None,
            result_hash: "b".repeat(64),
            error_code: Some("SYNTHETIC_FAILURE".to_owned()),
            completed_at: "2026-08-03T12:00:00Z".to_owned(),
        };
        connection
            .execute(
                "INSERT INTO engagement_remediation_pack_results
                 (attempt_id, outcome, pack_id, result_hash, error_code, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    failed.attempt_id,
                    failed.outcome,
                    failed.pack_id,
                    failed.result_hash,
                    failed.error_code,
                    failed.completed_at,
                ],
            )
            .expect("failed result row");
        let stored = load_remediation_terminal_results(&connection).expect("enumerate results");
        assert_eq!(stored.len(), 1);
        assert!(verify_remediation_terminal_result_shape(&connection, &stored[0]).is_err());
    }

    #[test]
    fn duplicate_remediation_results_fail_exact_cardinality() {
        let connection = terminal_graph_connection();
        let result = complete_terminal_result();
        connection
            .execute(
                "INSERT INTO engagement_remediation_pack_attempts
                 (attempt_id, planned_pack_id) VALUES (?1, ?2)",
                params![result.attempt_id, result.pack_id],
            )
            .expect("attempt row");
        connection
            .execute(
                "INSERT INTO engagement_remediation_packs
                 (pack_id, attempt_id, completed_at) VALUES (?1, ?2, ?3)",
                params![result.pack_id, result.attempt_id, result.completed_at],
            )
            .expect("pack row");
        for hash in ["c".repeat(64), "d".repeat(64)] {
            connection
                .execute(
                    "INSERT INTO engagement_remediation_pack_results
                     (attempt_id, outcome, pack_id, result_hash, error_code, completed_at)
                     VALUES (?1, 'COMPLETE', ?2, ?3, NULL, ?4)",
                    params![result.attempt_id, result.pack_id, hash, result.completed_at],
                )
                .expect("duplicate result row");
        }
        let stored = load_remediation_terminal_results(&connection).expect("enumerate results");
        assert_eq!(stored.len(), 2);
        assert!(
            stored
                .iter()
                .all(|row| { verify_remediation_terminal_result_shape(&connection, row).is_err() }),
            "every duplicate result row must fail the exact-one graph"
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::{
        fs, io,
        os::windows::fs::{symlink_dir, symlink_file},
    };

    use super::*;

    #[test]
    fn remediation_namespace_reparse_is_rejected() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("target");
        let link = temporary.path().join("remediation-packs");
        fs::create_dir(&target).expect("target directory");
        match symlink_dir(&target, &link) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("create test reparse directory: {error}"),
        }
        let metadata = fs::symlink_metadata(&link).expect("reparse metadata");
        assert!(
            verify_real_directory(&link, &metadata).is_err(),
            "a Windows reparse namespace must fail closed"
        );
    }

    #[test]
    fn remediation_archive_reparse_is_rejected_without_reading_target() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("target.zip");
        let link = temporary.path().join("archive.zip");
        fs::write(&target, b"outside archive bytes").expect("target file");
        match symlink_file(&target, &link) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("create test file reparse point: {error}"),
        }
        assert!(verify_named_regular_file(&link, None, false).is_err());
        assert_eq!(
            fs::read(&target).expect("target remains"),
            b"outside archive bytes"
        );
    }
}

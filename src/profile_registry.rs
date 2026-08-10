//! Trusted local registry for immutable Migration Object Profile versions.
//!
//! Registration validates and freezes canonical profile bytes but never grants
//! runtime authority. Authority is created only by a separate attributable
//! approval action over one exact profile ID, version, and hash. Lifecycle
//! transitions append hash-chained evidence and never rewrite profile bytes.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json, sha256_hex},
    profile::{
        ApprovedProfile, read_profile_document_bounded, read_profile_schema_bounded,
        trust_registry_approval, validate_profile_document_with_schema,
    },
    state::{DATABASE_FILENAME, SCHEMA_VERSION, WorkspaceWriter, now_rfc3339},
};

const REGISTRY_CONTRACT_VERSION: u32 = 1;
const VALIDATION_HASH_CONTRACT: &str = "migration-stream-profile-validation/v1";
const APPROVAL_HASH_CONTRACT: &str = "migration-stream-profile-approval/v1";
const LIFECYCLE_HASH_CONTRACT: &str = "migration-stream-profile-lifecycle-event/v1";
const REGISTRATION_ACTOR: &str = "migration-stream-registry";
const REGISTRATION_REASON: &str = "immutable profile version registered";
const MAX_LIFECYCLE_EVENTS: usize = 8;
const ABSOLUTE_MAX_REGISTRY_METADATA_BYTES: usize = 64 * 1024;
const ABSOLUTE_MAX_LIFECYCLE_BYTES: usize =
    MAX_LIFECYCLE_EVENTS * (2 * ABSOLUTE_MAX_STRING_BYTES + 1_024);
const ABSOLUTE_MAX_APPROVAL_BYTES: usize = 2 * ABSOLUTE_MAX_STRING_BYTES + 1_024;
const PROFILE_VERSIONS_UPDATE_TRIGGER: &str = "CREATE TRIGGER profile_versions_no_update BEFORE UPDATE ON profile_versions BEGIN SELECT RAISE(ABORT, 'profile version history is immutable'); END";
const PROFILE_VERSIONS_DELETE_TRIGGER: &str = "CREATE TRIGGER profile_versions_no_delete BEFORE DELETE ON profile_versions BEGIN SELECT RAISE(ABORT, 'profile version history is immutable'); END";
const PROFILE_APPROVALS_UPDATE_TRIGGER: &str = "CREATE TRIGGER profile_approvals_no_update BEFORE UPDATE ON profile_approvals BEGIN SELECT RAISE(ABORT, 'profile approval history is immutable'); END";
const PROFILE_APPROVALS_DELETE_TRIGGER: &str = "CREATE TRIGGER profile_approvals_no_delete BEFORE DELETE ON profile_approvals BEGIN SELECT RAISE(ABORT, 'profile approval history is immutable'); END";
const PROFILE_LIFECYCLE_UPDATE_TRIGGER: &str = "CREATE TRIGGER profile_lifecycle_events_no_update BEFORE UPDATE ON profile_lifecycle_events BEGIN SELECT RAISE(ABORT, 'profile lifecycle history is immutable'); END";
const PROFILE_LIFECYCLE_DELETE_TRIGGER: &str = "CREATE TRIGGER profile_lifecycle_events_no_delete BEFORE DELETE ON profile_lifecycle_events BEGIN SELECT RAISE(ABORT, 'profile lifecycle history is immutable'); END";

/// Absolute registry ceilings. Configured limits may tighten but never exceed
/// these values, so every registered document remains safe to load later.
const ABSOLUTE_MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const ABSOLUTE_MAX_ENTITIES: usize = 256;
const ABSOLUTE_MAX_FIELDS: usize = 8_192;
const ABSOLUTE_MAX_RULES: usize = 8_192;
const ABSOLUTE_MAX_RELATIONSHIPS: usize = 4_096;
const ABSOLUTE_MAX_STRING_BYTES: usize = 64 * 1024;

/// Bounded policy for profile registration and governance inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RegistryLimits {
    pub maximum_document_bytes: usize,
    pub maximum_entities: usize,
    pub maximum_fields: usize,
    pub maximum_rules: usize,
    pub maximum_relationships: usize,
    pub maximum_string_bytes: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            maximum_document_bytes: ABSOLUTE_MAX_DOCUMENT_BYTES,
            maximum_entities: ABSOLUTE_MAX_ENTITIES,
            maximum_fields: ABSOLUTE_MAX_FIELDS,
            maximum_rules: ABSOLUTE_MAX_RULES,
            maximum_relationships: ABSOLUTE_MAX_RELATIONSHIPS,
            maximum_string_bytes: ABSOLUTE_MAX_STRING_BYTES,
        }
    }
}

impl RegistryLimits {
    fn validate(&self) -> Result<()> {
        validate_limit(
            "maximum_document_bytes",
            self.maximum_document_bytes,
            ABSOLUTE_MAX_DOCUMENT_BYTES,
        )?;
        validate_limit(
            "maximum_entities",
            self.maximum_entities,
            ABSOLUTE_MAX_ENTITIES,
        )?;
        validate_limit("maximum_fields", self.maximum_fields, ABSOLUTE_MAX_FIELDS)?;
        validate_limit("maximum_rules", self.maximum_rules, ABSOLUTE_MAX_RULES)?;
        validate_limit(
            "maximum_relationships",
            self.maximum_relationships,
            ABSOLUTE_MAX_RELATIONSHIPS,
        )?;
        validate_limit(
            "maximum_string_bytes",
            self.maximum_string_bytes,
            ABSOLUTE_MAX_STRING_BYTES,
        )
    }
}

/// Local lifecycle state, separate from the immutable status declared in the
/// registered profile document.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProfileLifecycleStatus {
    GeneratedDraft,
    ReviewedDraft,
    Approved,
    Deprecated,
    Retired,
}

impl ProfileLifecycleStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GeneratedDraft => "GENERATED_DRAFT",
            Self::ReviewedDraft => "REVIEWED_DRAFT",
            Self::Approved => "APPROVED",
            Self::Deprecated => "DEPRECATED",
            Self::Retired => "RETIRED",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "GENERATED_DRAFT" => Ok(Self::GeneratedDraft),
            "REVIEWED_DRAFT" => Ok(Self::ReviewedDraft),
            "APPROVED" => Ok(Self::Approved),
            "DEPRECATED" => Ok(Self::Deprecated),
            "RETIRED" => Ok(Self::Retired),
            other => Err(registry_integrity(format!(
                "unknown profile lifecycle state {other:?}"
            ))),
        }
    }

    const fn grants_authority(self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// Authority granted by the registry. It is intentionally narrower than a
/// general approval or target-compatibility claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProfileAuthority {
    None,
    LocalMappingValidation,
}

/// Immutable approval evidence for one exact registered profile version/hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileApprovalView {
    pub approval_id: String,
    pub approver: String,
    pub reason: String,
    pub acknowledged: bool,
    pub approved_at: String,
    pub approval_hash: String,
}

/// One immutable, hash-chained lifecycle event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileLifecycleEventView {
    pub sequence: u64,
    pub from_state: Option<String>,
    pub to_state: ProfileLifecycleStatus,
    pub actor: String,
    pub reason: String,
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
    pub occurred_at: String,
}

/// Stable inspection view that keeps document status, local lifecycle, and
/// runtime authority visibly separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileVersionView {
    pub schema_version: u32,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub schema_hash: String,
    pub validation_hash: String,
    pub declared_status: String,
    pub lifecycle_status: ProfileLifecycleStatus,
    pub authority: ProfileAuthority,
    pub can_start_new_runs: bool,
    pub registered_at: String,
    pub approval: Option<ProfileApprovalView>,
    pub lifecycle_events: Vec<ProfileLifecycleEventView>,
}

/// Registry-backed authority capability for an exact immutable profile.
#[derive(Debug)]
pub(crate) struct ApprovedProfileHandle {
    pub(crate) profile: ApprovedProfile,
    pub(crate) profile_hash: String,
    pub(crate) view: ProfileVersionView,
}

#[derive(Debug)]
struct RegistryRecord {
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    schema_hash: String,
    document_json: String,
    declared_status: String,
    validation_hash: String,
    registered_at: String,
}

/// Validate and immutably register one exact profile document.
///
/// An exact repeat is idempotent. Reusing an ID/version with different profile
/// bytes or validation-schema evidence is an explicit conflict.
///
/// # Errors
///
/// Returns a typed policy, profile-validation, conflict, persistence, or I/O
/// error without creating partial authority evidence.
pub fn register_profile(
    workspace: &Path,
    schema_path: &Path,
    profile_path: &Path,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    limits.validate()?;
    let schema_before = read_profile_schema_bounded(schema_path, limits.maximum_document_bytes)?;
    let input_document =
        read_profile_document_bounded(profile_path, limits.maximum_document_bytes)?;
    register_profile_document_with_schema(workspace, &schema_before, &input_document, limits)
}

/// Register one exact in-memory profile document under the trusted local schema.
///
/// This keeps a startup-pinned object-pack profile from being re-read through
/// a mutable path when the operator later selects the pack. Registration still
/// validates the exact document and creates no approval authority.
pub(crate) fn register_profile_document(
    workspace: &Path,
    schema_path: &Path,
    input_document: &Value,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    limits.validate()?;
    let schema_before = read_profile_schema_bounded(schema_path, limits.maximum_document_bytes)?;
    register_profile_document_with_schema(workspace, &schema_before, input_document, limits)
}

fn register_profile_document_with_schema(
    workspace: &Path,
    schema: &Value,
    input_document: &Value,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    // Preserve the public API's fail-before-persistence contract: bounded
    // schema/document validation happens before opening or creating SQLite.
    limits.validate()?;
    let validated = validate_profile_document_with_schema(schema, input_document)?;
    enforce_document_limits(validated.document(), limits)?;
    let canonical = canonical_json_string(validated.document())?;
    if canonical.len() > limits.maximum_document_bytes {
        return Err(limit_error(
            "canonical profile document bytes",
            canonical.len(),
            limits.maximum_document_bytes,
        ));
    }
    let writer = WorkspaceWriter::open(workspace)?;
    let transaction = writer.connection.unchecked_transaction()?;
    let view = register_profile_document_with_schema_transaction(
        &transaction,
        schema,
        input_document,
        limits,
    )?;
    transaction.commit()?;
    Ok(view)
}

pub(crate) fn register_profile_document_transaction(
    transaction: &Transaction<'_>,
    schema_path: &Path,
    input_document: &Value,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    limits.validate()?;
    let schema = read_profile_schema_bounded(schema_path, limits.maximum_document_bytes)?;
    register_profile_document_with_schema_transaction(transaction, &schema, input_document, limits)
}

fn register_profile_document_with_schema_transaction(
    transaction: &Transaction<'_>,
    schema: &Value,
    input_document: &Value,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    let schema_hash = canonical_hash(schema)?;
    let validated = validate_profile_document_with_schema(schema, input_document)?;
    let document = validated.document();
    enforce_document_limits(document, limits)?;

    let summary = validated.summary();
    let initial_status = registration_status(&summary.status)?;
    let document_json = canonical_json_string(document)?;
    if document_json.len() > limits.maximum_document_bytes {
        return Err(limit_error(
            "canonical profile document bytes",
            document_json.len(),
            limits.maximum_document_bytes,
        ));
    }
    let profile_hash = sha256_hex(document_json.as_bytes());
    let validation_hash = validation_hash(&schema_hash, &profile_hash)?;

    verify_profile_immutability_guards(transaction)?;
    if let Some(existing) =
        load_registry_record_optional(transaction, &summary.profile_id, &summary.version)?
    {
        let existing = verified_profile_view(transaction, existing)?;
        if existing.profile_hash != profile_hash {
            return Err(KernelError::Conflict(format!(
                "profile {}/{} is already registered with different canonical bytes",
                summary.profile_id, summary.version
            )));
        }
        if existing.schema_hash != schema_hash || existing.validation_hash != validation_hash {
            return Err(KernelError::Conflict(format!(
                "profile {}/{} is already registered under different validation evidence",
                summary.profile_id, summary.version
            )));
        }
        return Ok(existing);
    }

    let registered_at = now_rfc3339();
    verify_profile_immutability_guards(transaction)?;
    transaction.execute(
        "INSERT INTO profile_versions (
             profile_id, profile_version, profile_hash, schema_hash,
             document_json, declared_status, validation_hash, registered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            summary.profile_id,
            summary.version,
            profile_hash,
            schema_hash,
            document_json,
            summary.status,
            validation_hash,
            registered_at,
        ],
    )?;
    append_lifecycle_event(
        transaction,
        &summary.profile_id,
        &summary.version,
        &profile_hash,
        None,
        initial_status,
        REGISTRATION_ACTOR,
        REGISTRATION_REASON,
        &registered_at,
    )?;
    let record = load_registry_record(transaction, &summary.profile_id, &summary.version)?;
    verified_profile_view(transaction, record)
}

/// Inspect one exact registered identity/version without mutating workspace
/// state. Stored hashes and the complete bounded lifecycle chain are verified
/// before a view is returned.
///
/// # Errors
///
/// Returns a typed not-found, integrity, or persistence error.
pub fn inspect_profile(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
) -> Result<ProfileVersionView> {
    let connection = open_registry_read_only(workspace)?;
    let transaction = connection.unchecked_transaction()?;
    let record = load_registry_record(&transaction, profile_id, profile_version)?;
    let view = verified_profile_view(&transaction, record)?;
    transaction.commit()?;
    Ok(view)
}

/// Verify one exact registered profile identity through an existing read
/// transaction. This keeps practitioner reads on their original `SQLite`
/// snapshot while reusing the registry's complete document, approval, and
/// lifecycle integrity replay.
pub(crate) fn inspect_profile_connection(
    connection: &Connection,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<ProfileVersionView> {
    let record = load_registry_record(connection, profile_id, profile_version)?;
    require_exact_hash(&record, profile_hash)?;
    verified_profile_view(connection, record)
}

/// Return one exact immutable profile document after replaying its registry,
/// approval, and lifecycle evidence through the same bounded snapshot.
pub(crate) fn load_verified_profile_document(
    connection: &Connection,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<Value> {
    let record = load_registry_record(connection, profile_id, profile_version)?;
    require_exact_hash(&record, profile_hash)?;
    let document_json = record.document_json.clone();
    verified_profile_view(connection, record)?;
    parse_unique_json(document_json.as_bytes()).map_err(|error| {
        registry_integrity(format!(
            "profile {profile_id}/{profile_version} document JSON is invalid: {error}"
        ))
    })
}

/// Grant local mapping/validation authority to one exact reviewed profile.
///
/// The action requires attributable human evidence and an explicit narrow-scope
/// acknowledgement. It appends approval and lifecycle evidence atomically; the
/// registered profile bytes and declared status remain unchanged.
///
/// # Errors
///
/// Returns a typed policy, authority, conflict, integrity, or persistence error
/// without modifying profile bytes or granting partial authority.
#[allow(clippy::too_many_arguments)]
pub fn approve_profile(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    approver: &str,
    reason: &str,
    acknowledged: bool,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    let writer = WorkspaceWriter::open(workspace)?;
    let transaction = writer.connection.unchecked_transaction()?;
    let view = approve_profile_transaction(
        &transaction,
        profile_id,
        profile_version,
        profile_hash,
        approver,
        reason,
        acknowledged,
        limits,
    )?;
    transaction.commit()?;
    Ok(view)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn approve_profile_transaction(
    transaction: &Transaction<'_>,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    approver: &str,
    reason: &str,
    acknowledged: bool,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    limits.validate()?;
    validate_governance_inputs(
        profile_id,
        profile_version,
        profile_hash,
        approver,
        reason,
        limits,
    )?;
    if !acknowledged {
        return Err(KernelError::PolicyInvalid(
            "profile approval must explicitly acknowledge local mapping/validation authority only"
                .to_owned(),
        ));
    }

    let record = load_registry_record(transaction, profile_id, profile_version)?;
    require_exact_hash(&record, profile_hash)?;
    let current = verified_profile_view(transaction, record)?;
    if let Some(existing) = &current.approval {
        if existing.approver == approver
            && existing.reason == reason
            && existing.acknowledged == acknowledged
        {
            return Ok(current);
        }
        return Err(KernelError::Conflict(format!(
            "profile {profile_id}/{profile_version}/{profile_hash} already has different immutable approval evidence"
        )));
    }
    if current.lifecycle_status != ProfileLifecycleStatus::ReviewedDraft {
        return Err(KernelError::ProfileNotApproved(format!(
            "profile approval requires REVIEWED_DRAFT lifecycle; found {}",
            current.lifecycle_status.as_str()
        )));
    }

    let approval_id = Uuid::new_v4().to_string();
    let approved_at = now_rfc3339();
    let approval_hash = approval_hash(
        &approval_id,
        profile_id,
        profile_version,
        profile_hash,
        approver,
        reason,
        acknowledged,
        &approved_at,
    )?;
    transaction.execute(
        "INSERT INTO profile_approvals (
             approval_id, profile_id, profile_version, profile_hash, approver,
             reason, acknowledged, approved_at, approval_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            approval_id,
            profile_id,
            profile_version,
            profile_hash,
            approver,
            reason,
            acknowledged,
            approved_at,
            approval_hash,
        ],
    )?;
    append_lifecycle_event(
        transaction,
        profile_id,
        profile_version,
        profile_hash,
        Some(ProfileLifecycleStatus::ReviewedDraft.as_str()),
        ProfileLifecycleStatus::Approved,
        approver,
        reason,
        &approved_at,
    )?;
    let record = load_registry_record(transaction, profile_id, profile_version)?;
    verified_profile_view(transaction, record)
}

/// Deprecate an approved profile for new mapping/validation runs.
///
/// # Errors
///
/// Returns a typed policy, conflict, integrity, or persistence error.
#[allow(clippy::too_many_arguments)]
pub fn deprecate_profile(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    actor: &str,
    reason: &str,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    transition_profile(
        workspace,
        profile_id,
        profile_version,
        profile_hash,
        actor,
        reason,
        ProfileLifecycleStatus::Deprecated,
        limits,
    )
}

/// Retire a registered profile. Retirement is terminal and blocks new work.
///
/// # Errors
///
/// Returns a typed policy, conflict, integrity, or persistence error.
#[allow(clippy::too_many_arguments)]
pub fn retire_profile(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    actor: &str,
    reason: &str,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    transition_profile(
        workspace,
        profile_id,
        profile_version,
        profile_hash,
        actor,
        reason,
        ProfileLifecycleStatus::Retired,
        limits,
    )
}

/// Load the exact registry-approved capability used by mapping and validation.
///
/// Deprecated, retired, draft, hash-mismatched, schema-mismatched, or tampered
/// records fail closed and cannot start new work.
///
/// # Errors
///
/// Returns a typed authority, integrity, schema, not-found, or persistence error.
pub(crate) fn load_approved_profile(
    writer: &WorkspaceWriter,
    schema_path: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
) -> Result<ApprovedProfileHandle> {
    let record = load_registry_record(&writer.connection, profile_id, profile_version)?;
    require_exact_hash(&record, profile_hash)?;
    let view = verified_profile_view(&writer.connection, record)?;
    if !view.lifecycle_status.grants_authority()
        || view.authority != ProfileAuthority::LocalMappingValidation
    {
        return Err(KernelError::ProfileNotApproved(format!(
            "profile {profile_id}/{profile_version}/{profile_hash} lifecycle is {}",
            view.lifecycle_status.as_str()
        )));
    }

    let schema = read_profile_schema_bounded(schema_path, ABSOLUTE_MAX_DOCUMENT_BYTES)?;
    let schema_hash = canonical_hash(&schema)?;
    if schema_hash != view.schema_hash {
        return Err(KernelError::ProfileNotApproved(format!(
            "profile {profile_id}/{profile_version}/{profile_hash} was validated against a different schema hash"
        )));
    }
    let document: Value = serde_json::from_str(
        &load_registry_record(&writer.connection, profile_id, profile_version)?.document_json,
    )?;
    enforce_document_limits(&document, &RegistryLimits::default())?;
    let validated = validate_profile_document_with_schema(&schema, &document)?;
    let summary = validated.summary();
    if summary.profile_id != profile_id
        || summary.version != profile_version
        || summary.status != view.declared_status
    {
        return Err(registry_integrity(format!(
            "validated profile identity/status differs from registry key {profile_id}/{profile_version}"
        )));
    }
    let profile = trust_registry_approval(validated);
    Ok(ApprovedProfileHandle {
        profile,
        profile_hash: profile_hash.to_owned(),
        view,
    })
}

/// Verify every registered profile, lifecycle chain, and approval without
/// granting authority. Registry mismatches are accumulated as integrity issues
/// so the caller can report them alongside the rest of workspace replay.
///
/// # Errors
///
/// Returns an error only when registry rows cannot be read or bounded counts
/// cannot be represented; semantic mismatches are appended to `issues`.
#[allow(clippy::too_many_lines)] // One bounded registry/lifecycle replay produces one report.
pub(crate) fn verify_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<(usize, usize)> {
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version < 5 {
        return Ok((0, 0));
    }
    verify_profile_immutability_guards(connection)?;

    let mut statement = connection.prepare(
        "SELECT rowid, length(CAST(document_json AS BLOB)),
                length(CAST(profile_id AS BLOB))
              + length(CAST(profile_version AS BLOB))
              + length(CAST(profile_hash AS BLOB))
              + length(CAST(schema_hash AS BLOB))
              + length(CAST(declared_status AS BLOB))
              + length(CAST(validation_hash AS BLOB))
              + length(CAST(registered_at AS BLOB))
         FROM profile_versions ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut checked_versions = 0_usize;
    for row in rows {
        let (rowid, document_bytes, metadata_bytes) = row?;
        checked_versions = checked_versions.checked_add(1).ok_or_else(|| {
            registry_integrity("profile-version integrity count overflow".to_owned())
        })?;
        if usize::try_from(document_bytes).unwrap_or(usize::MAX) > ABSOLUTE_MAX_DOCUMENT_BYTES
            || usize::try_from(metadata_bytes).unwrap_or(usize::MAX)
                > ABSOLUTE_MAX_REGISTRY_METADATA_BYTES
        {
            issues.push(format!(
                "profile registry row {rowid} stored document/metadata exceeds immutable bounds"
            ));
            continue;
        }
        let record = connection.query_row(
            "SELECT profile_id, profile_version, profile_hash, schema_hash,
                    document_json, declared_status, validation_hash, registered_at
             FROM profile_versions WHERE rowid = ?1",
            [rowid],
            |row| {
                Ok(RegistryRecord {
                    profile_id: row.get(0)?,
                    profile_version: row.get(1)?,
                    profile_hash: row.get(2)?,
                    schema_hash: row.get(3)?,
                    document_json: row.get(4)?,
                    declared_status: row.get(5)?,
                    validation_hash: row.get(6)?,
                    registered_at: row.get(7)?,
                })
            },
        )?;
        if let Err(error) = verified_profile_view(connection, record) {
            issues.push(error.to_string());
        }
    }
    drop(statement);

    let approval_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM profile_approvals", [], |row| {
            row.get(0)
        })?;
    let checked_approvals = usize::try_from(approval_count).map_err(|_| {
        registry_integrity("profile-approval integrity count is invalid".to_owned())
    })?;
    let orphaned_approvals: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM profile_approvals AS approval
         LEFT JOIN profile_versions AS profile
           ON profile.profile_id = approval.profile_id
          AND profile.profile_version = approval.profile_version
          AND profile.profile_hash = approval.profile_hash
         WHERE profile.profile_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    if orphaned_approvals != 0 {
        issues.push(format!(
            "profile registry contains {orphaned_approvals} approval(s) without an exact registered profile"
        ));
    }
    let orphaned_events: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM profile_lifecycle_events AS event
         LEFT JOIN profile_versions AS profile
           ON profile.profile_id = event.profile_id
          AND profile.profile_version = event.profile_version
          AND profile.profile_hash = event.profile_hash
         WHERE profile.profile_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    if orphaned_events != 0 {
        issues.push(format!(
            "profile registry contains {orphaned_events} lifecycle event(s) without an exact registered profile"
        ));
    }
    Ok((checked_versions, checked_approvals))
}

#[allow(clippy::too_many_arguments)]
fn transition_profile(
    workspace: &Path,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    actor: &str,
    reason: &str,
    target: ProfileLifecycleStatus,
    limits: &RegistryLimits,
) -> Result<ProfileVersionView> {
    limits.validate()?;
    validate_governance_inputs(
        profile_id,
        profile_version,
        profile_hash,
        actor,
        reason,
        limits,
    )?;
    let writer = WorkspaceWriter::open(workspace)?;
    let transaction = writer.connection.unchecked_transaction()?;
    let record = load_registry_record(&transaction, profile_id, profile_version)?;
    require_exact_hash(&record, profile_hash)?;
    let current = verified_profile_view(&transaction, record)?;
    if current.lifecycle_status == target {
        let last = current.lifecycle_events.last().ok_or_else(|| {
            registry_integrity("registered profile has no lifecycle event".to_owned())
        })?;
        if last.actor == actor && last.reason == reason {
            return Ok(current);
        }
        return Err(KernelError::Conflict(format!(
            "profile {profile_id}/{profile_version}/{profile_hash} is already {} under different immutable lifecycle evidence",
            target.as_str()
        )));
    }
    if !transition_is_allowed(current.lifecycle_status, target) {
        return Err(KernelError::Conflict(format!(
            "profile lifecycle transition {} -> {} is not allowed",
            current.lifecycle_status.as_str(),
            target.as_str()
        )));
    }
    let occurred_at = now_rfc3339();
    append_lifecycle_event(
        &transaction,
        profile_id,
        profile_version,
        profile_hash,
        Some(current.lifecycle_status.as_str()),
        target,
        actor,
        reason,
        &occurred_at,
    )?;
    transaction.commit()?;
    let record = load_registry_record(&writer.connection, profile_id, profile_version)?;
    verified_profile_view(&writer.connection, record)
}

fn transition_is_allowed(from: ProfileLifecycleStatus, to: ProfileLifecycleStatus) -> bool {
    matches!(
        (from, to),
        (
            ProfileLifecycleStatus::ReviewedDraft,
            ProfileLifecycleStatus::Approved
        ) | (
            ProfileLifecycleStatus::Approved,
            ProfileLifecycleStatus::Deprecated | ProfileLifecycleStatus::Retired
        ) | (
            ProfileLifecycleStatus::Deprecated
                | ProfileLifecycleStatus::GeneratedDraft
                | ProfileLifecycleStatus::ReviewedDraft,
            ProfileLifecycleStatus::Retired
        )
    )
}

fn registration_status(status: &str) -> Result<ProfileLifecycleStatus> {
    match ProfileLifecycleStatus::parse(status)? {
        initial @ (ProfileLifecycleStatus::GeneratedDraft
        | ProfileLifecycleStatus::ReviewedDraft) => Ok(initial),
        ProfileLifecycleStatus::Approved
        | ProfileLifecycleStatus::Deprecated
        | ProfileLifecycleStatus::Retired => Err(KernelError::ProfileNotApproved(format!(
            "caller-declared {status} status is not trusted registry authority; register a draft and use an explicit lifecycle action"
        ))),
    }
}

fn validate_governance_inputs(
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    actor: &str,
    reason: &str,
    limits: &RegistryLimits,
) -> Result<()> {
    require_nonblank_bounded("profile ID", profile_id, limits.maximum_string_bytes)?;
    require_nonblank_bounded(
        "profile version",
        profile_version,
        limits.maximum_string_bytes,
    )?;
    if !is_sha256(profile_hash) {
        return Err(KernelError::PolicyInvalid(
            "profile hash must be 64 lowercase hexadecimal characters".to_owned(),
        ));
    }
    require_nonblank_bounded("actor/approver", actor, limits.maximum_string_bytes)?;
    require_nonblank_bounded("reason", reason, limits.maximum_string_bytes)
}

fn require_nonblank_bounded(label: &str, value: &str, limit: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must not be blank"
        )));
    }
    if value.len() > limit {
        return Err(limit_error(label, value.len(), limit));
    }
    Ok(())
}

fn require_exact_hash(record: &RegistryRecord, supplied_hash: &str) -> Result<()> {
    if record.profile_hash != supplied_hash {
        return Err(KernelError::Conflict(format!(
            "profile {}/{} hash mismatch: registered {}, supplied {supplied_hash}",
            record.profile_id, record.profile_version, record.profile_hash
        )));
    }
    Ok(())
}

fn load_registry_record(
    connection: &Connection,
    profile_id: &str,
    profile_version: &str,
) -> Result<RegistryRecord> {
    load_registry_record_optional(connection, profile_id, profile_version)?.ok_or_else(|| {
        KernelError::NotFound(format!("registered profile {profile_id}/{profile_version}"))
    })
}

fn load_registry_record_optional(
    connection: &Connection,
    profile_id: &str,
    profile_version: &str,
) -> Result<Option<RegistryRecord>> {
    let sizes: Option<(i64, i64)> = connection
        .query_row(
            "SELECT length(CAST(document_json AS BLOB)),
                    length(CAST(profile_id AS BLOB))
                  + length(CAST(profile_version AS BLOB))
                  + length(CAST(profile_hash AS BLOB))
                  + length(CAST(schema_hash AS BLOB))
                  + length(CAST(declared_status AS BLOB))
                  + length(CAST(validation_hash AS BLOB))
                  + length(CAST(registered_at AS BLOB))
             FROM profile_versions
             WHERE profile_id = ?1 AND profile_version = ?2",
            params![profile_id, profile_version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((document_bytes, metadata_bytes)) = sizes else {
        return Ok(None);
    };
    if usize::try_from(document_bytes).unwrap_or(usize::MAX) > ABSOLUTE_MAX_DOCUMENT_BYTES
        || usize::try_from(metadata_bytes).unwrap_or(usize::MAX)
            > ABSOLUTE_MAX_REGISTRY_METADATA_BYTES
    {
        return Err(registry_integrity(format!(
            "profile {profile_id}/{profile_version} stored document/metadata exceeds immutable registry bounds"
        )));
    }
    connection
        .query_row(
            "SELECT profile_id, profile_version, profile_hash, schema_hash,
                    document_json, declared_status, validation_hash, registered_at
             FROM profile_versions
             WHERE profile_id = ?1 AND profile_version = ?2",
            params![profile_id, profile_version],
            |row| {
                Ok(RegistryRecord {
                    profile_id: row.get(0)?,
                    profile_version: row.get(1)?,
                    profile_hash: row.get(2)?,
                    schema_hash: row.get(3)?,
                    document_json: row.get(4)?,
                    declared_status: row.get(5)?,
                    validation_hash: row.get(6)?,
                    registered_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn verified_profile_view(
    connection: &Connection,
    record: RegistryRecord,
) -> Result<ProfileVersionView> {
    verify_profile_immutability_guards(connection)?;
    verify_registry_record(&record)?;
    let lifecycle_events = load_and_verify_lifecycle(connection, &record)?;
    let lifecycle_status = lifecycle_events
        .last()
        .map(|event| event.to_state)
        .ok_or_else(|| {
            registry_integrity("registered profile has no lifecycle evidence".to_owned())
        })?;
    let approval = load_and_verify_approval(connection, &record)?;
    verify_approval_lifecycle_binding(&record, &lifecycle_events, approval.as_ref())?;
    let authority = if lifecycle_status.grants_authority() && approval.is_some() {
        ProfileAuthority::LocalMappingValidation
    } else {
        ProfileAuthority::None
    };
    Ok(ProfileVersionView {
        schema_version: REGISTRY_CONTRACT_VERSION,
        profile_id: record.profile_id,
        profile_version: record.profile_version,
        profile_hash: record.profile_hash,
        schema_hash: record.schema_hash,
        validation_hash: record.validation_hash,
        declared_status: record.declared_status,
        lifecycle_status,
        authority,
        can_start_new_runs: authority == ProfileAuthority::LocalMappingValidation,
        registered_at: record.registered_at,
        approval,
        lifecycle_events,
    })
}

fn normalized_schema_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_profile_immutability_guards(connection: &Connection) -> Result<()> {
    for (table, name, expected) in [
        (
            "profile_versions",
            "profile_versions_no_update",
            PROFILE_VERSIONS_UPDATE_TRIGGER,
        ),
        (
            "profile_versions",
            "profile_versions_no_delete",
            PROFILE_VERSIONS_DELETE_TRIGGER,
        ),
        (
            "profile_approvals",
            "profile_approvals_no_update",
            PROFILE_APPROVALS_UPDATE_TRIGGER,
        ),
        (
            "profile_approvals",
            "profile_approvals_no_delete",
            PROFILE_APPROVALS_DELETE_TRIGGER,
        ),
        (
            "profile_lifecycle_events",
            "profile_lifecycle_events_no_update",
            PROFILE_LIFECYCLE_UPDATE_TRIGGER,
        ),
        (
            "profile_lifecycle_events",
            "profile_lifecycle_events_no_delete",
            PROFILE_LIFECYCLE_DELETE_TRIGGER,
        ),
    ] {
        let expected = normalized_schema_sql(expected);
        let sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                params![name, table],
                |row| row.get(0),
            )
            .optional()?;
        if sql.as_deref().map(normalized_schema_sql).as_deref() != Some(expected.as_str()) {
            return Err(registry_integrity(format!(
                "immutable guard {name} is missing or altered"
            )));
        }
    }
    Ok(())
}

fn verify_registry_record(record: &RegistryRecord) -> Result<()> {
    require_integrity_text("registered profile ID", &record.profile_id)?;
    require_integrity_text("registered profile version", &record.profile_version)?;
    require_rfc3339("profile registration timestamp", &record.registered_at)?;
    if !is_sha256(&record.profile_hash)
        || !is_sha256(&record.schema_hash)
        || !is_sha256(&record.validation_hash)
    {
        return Err(registry_integrity(format!(
            "profile {}/{} contains an invalid registry hash",
            record.profile_id, record.profile_version
        )));
    }
    let document = parse_unique_json(record.document_json.as_bytes()).map_err(|error| {
        registry_integrity(format!(
            "profile {}/{} document JSON is invalid: {error}",
            record.profile_id, record.profile_version
        ))
    })?;
    enforce_document_limits(&document, &RegistryLimits::default())?;
    let canonical = canonical_json_string(&document)?;
    if canonical != record.document_json || sha256_hex(canonical.as_bytes()) != record.profile_hash
    {
        return Err(registry_integrity(format!(
            "profile {}/{} canonical bytes/hash do not match registry evidence",
            record.profile_id, record.profile_version
        )));
    }
    if document.get("profileId").and_then(Value::as_str) != Some(record.profile_id.as_str())
        || document.get("version").and_then(Value::as_str) != Some(record.profile_version.as_str())
        || document.get("status").and_then(Value::as_str) != Some(record.declared_status.as_str())
    {
        return Err(registry_integrity(format!(
            "profile {}/{} identity/status differs from its immutable document",
            record.profile_id, record.profile_version
        )));
    }
    registration_status(&record.declared_status).map_err(|error| {
        registry_integrity(format!(
            "profile {}/{} has an invalid registered declared status: {error}",
            record.profile_id, record.profile_version
        ))
    })?;
    let expected_validation_hash = validation_hash(&record.schema_hash, &record.profile_hash)?;
    if record.validation_hash != expected_validation_hash {
        return Err(registry_integrity(format!(
            "profile {}/{} validation hash does not match its schema/profile evidence",
            record.profile_id, record.profile_version
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn load_and_verify_lifecycle(
    connection: &Connection,
    record: &RegistryRecord,
) -> Result<Vec<ProfileLifecycleEventView>> {
    let (event_count, event_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
               COALESCE(length(CAST(from_state AS BLOB)), 0)
             + length(CAST(to_state AS BLOB))
             + length(CAST(actor AS BLOB))
             + length(CAST(reason AS BLOB))
             + COALESCE(length(CAST(previous_event_hash AS BLOB)), 0)
             + length(CAST(event_hash AS BLOB))
             + length(CAST(occurred_at AS BLOB))
         ), 0)
         FROM profile_lifecycle_events
         WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3",
        params![
            record.profile_id,
            record.profile_version,
            record.profile_hash
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if usize::try_from(event_count).unwrap_or(usize::MAX) > MAX_LIFECYCLE_EVENTS
        || usize::try_from(event_bytes).unwrap_or(usize::MAX) > ABSOLUTE_MAX_LIFECYCLE_BYTES
    {
        return Err(registry_integrity(format!(
            "profile {}/{} lifecycle count/bytes exceed immutable bounds",
            record.profile_id, record.profile_version
        )));
    }
    let mut statement = connection.prepare(
        "SELECT sequence, from_state, to_state, actor, reason,
                previous_event_hash, event_hash, occurred_at
         FROM profile_lifecycle_events
         WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3
         ORDER BY sequence
         LIMIT ?4",
    )?;
    let query_limit = i64::try_from(MAX_LIFECYCLE_EVENTS + 1)
        .map_err(|_| registry_integrity("lifecycle query limit overflow".to_owned()))?;
    let rows = statement.query_map(
        params![
            record.profile_id,
            record.profile_version,
            record.profile_hash,
            query_limit
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        },
    )?;
    let mut events = Vec::new();
    let mut previous_state: Option<ProfileLifecycleStatus> = None;
    let mut previous_hash: Option<String> = None;
    for row in rows {
        let (
            sequence,
            from_state,
            to_state,
            actor,
            reason,
            stored_previous,
            event_hash,
            occurred_at,
        ) = row?;
        if events.len() == MAX_LIFECYCLE_EVENTS {
            return Err(registry_integrity(format!(
                "profile {}/{} exceeds the lifecycle-event bound",
                record.profile_id, record.profile_version
            )));
        }
        let sequence = u64::try_from(sequence).map_err(|_| {
            registry_integrity(format!(
                "profile {}/{} has an invalid lifecycle sequence",
                record.profile_id, record.profile_version
            ))
        })?;
        require_integrity_text("lifecycle actor", &actor)?;
        require_integrity_text("lifecycle reason", &reason)?;
        if stored_previous != previous_hash {
            return Err(registry_integrity(format!(
                "profile {}/{} lifecycle previous-event hash is broken",
                record.profile_id, record.profile_version
            )));
        }
        let parsed_to = ProfileLifecycleStatus::parse(&to_state)?;
        require_rfc3339("profile lifecycle timestamp", &occurred_at)?;
        if let Some(previous_state) = previous_state {
            if from_state.as_deref() != Some(previous_state.as_str())
                || !transition_is_allowed(previous_state, parsed_to)
            {
                return Err(registry_integrity(format!(
                    "profile {}/{} has invalid lifecycle transition {from_state:?} -> {to_state}",
                    record.profile_id, record.profile_version
                )));
            }
        } else {
            let expected_initial =
                registration_status(&record.declared_status).map_err(|error| {
                    registry_integrity(format!("invalid initial lifecycle status: {error}"))
                })?;
            if from_state.is_some() || parsed_to != expected_initial {
                return Err(registry_integrity(format!(
                    "profile {}/{} lifecycle does not start from its declared draft status",
                    record.profile_id, record.profile_version
                )));
            }
            if actor != REGISTRATION_ACTOR
                || reason != REGISTRATION_REASON
                || occurred_at != record.registered_at
            {
                return Err(registry_integrity(format!(
                    "profile {}/{} initial lifecycle event does not bind its registration evidence",
                    record.profile_id, record.profile_version
                )));
            }
        }
        let expected_hash = lifecycle_event_hash(
            &record.profile_id,
            &record.profile_version,
            &record.profile_hash,
            from_state.as_deref(),
            parsed_to,
            &actor,
            &reason,
            stored_previous.as_deref(),
            &occurred_at,
        )?;
        if !is_sha256(&event_hash) || event_hash != expected_hash {
            return Err(registry_integrity(format!(
                "profile {}/{} lifecycle event hash is invalid",
                record.profile_id, record.profile_version
            )));
        }
        previous_state = Some(parsed_to);
        previous_hash = Some(event_hash.clone());
        events.push(ProfileLifecycleEventView {
            sequence,
            from_state,
            to_state: parsed_to,
            actor,
            reason,
            previous_event_hash: stored_previous,
            event_hash,
            occurred_at,
        });
    }
    Ok(events)
}

fn load_and_verify_approval(
    connection: &Connection,
    record: &RegistryRecord,
) -> Result<Option<ProfileApprovalView>> {
    let approval_bytes: Option<i64> = connection
        .query_row(
            "SELECT length(CAST(approval_id AS BLOB))
                  + length(CAST(approver AS BLOB))
                  + length(CAST(reason AS BLOB))
                  + length(CAST(approved_at AS BLOB))
                  + length(CAST(approval_hash AS BLOB))
             FROM profile_approvals
             WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3",
            params![
                record.profile_id,
                record.profile_version,
                record.profile_hash
            ],
            |row| row.get(0),
        )
        .optional()?;
    if approval_bytes.is_some_and(|bytes| {
        usize::try_from(bytes).unwrap_or(usize::MAX) > ABSOLUTE_MAX_APPROVAL_BYTES
    }) {
        return Err(registry_integrity(format!(
            "profile {}/{} approval bytes exceed immutable bounds",
            record.profile_id, record.profile_version
        )));
    }
    let approval = connection
        .query_row(
            "SELECT approval_id, approver, reason, acknowledged, approved_at, approval_hash
             FROM profile_approvals
             WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3",
            params![
                record.profile_id,
                record.profile_version,
                record.profile_hash
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((approval_id, approver, reason, acknowledged, approved_at, stored_approval_hash)) =
        approval
    else {
        return Ok(None);
    };
    if !Uuid::parse_str(&approval_id).is_ok_and(|parsed| parsed.to_string() == approval_id)
        || acknowledged != 1
    {
        return Err(registry_integrity(format!(
            "profile {}/{} approval identity/acknowledgement is invalid",
            record.profile_id, record.profile_version
        )));
    }
    require_integrity_text("approval approver", &approver)?;
    require_integrity_text("approval reason", &reason)?;
    require_rfc3339("profile approval timestamp", &approved_at)?;
    let expected_hash = approval_hash(
        &approval_id,
        &record.profile_id,
        &record.profile_version,
        &record.profile_hash,
        &approver,
        &reason,
        true,
        &approved_at,
    )?;
    if !is_sha256(&stored_approval_hash) || stored_approval_hash != expected_hash {
        return Err(registry_integrity(format!(
            "profile {}/{} approval hash is invalid",
            record.profile_id, record.profile_version
        )));
    }
    Ok(Some(ProfileApprovalView {
        approval_id,
        approver,
        reason,
        acknowledged: true,
        approved_at,
        approval_hash: stored_approval_hash,
    }))
}

fn verify_approval_lifecycle_binding(
    record: &RegistryRecord,
    events: &[ProfileLifecycleEventView],
    approval: Option<&ProfileApprovalView>,
) -> Result<()> {
    let mut approval_events = events
        .iter()
        .filter(|event| event.to_state == ProfileLifecycleStatus::Approved);
    let first_approval_event = approval_events.next();
    let has_additional_approval_event = approval_events.next().is_some();
    match (
        approval,
        first_approval_event,
        has_additional_approval_event,
    ) {
        (None, None, false) => Ok(()),
        (Some(approval), Some(event), false)
            if event.from_state.as_deref()
                == Some(ProfileLifecycleStatus::ReviewedDraft.as_str())
                && event.actor == approval.approver
                && event.reason == approval.reason
                && event.occurred_at == approval.approved_at =>
        {
            Ok(())
        }
        _ => Err(registry_integrity(format!(
            "profile {}/{} approval and lifecycle evidence do not bind exactly",
            record.profile_id, record.profile_version
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn append_lifecycle_event(
    transaction: &Transaction<'_>,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    from_state: Option<&str>,
    to_state: ProfileLifecycleStatus,
    actor: &str,
    reason: &str,
    occurred_at: &str,
) -> Result<()> {
    let previous_event_hash: Option<String> = transaction
        .query_row(
            "SELECT event_hash FROM profile_lifecycle_events
             WHERE profile_id = ?1 AND profile_version = ?2 AND profile_hash = ?3
             ORDER BY sequence DESC LIMIT 1",
            params![profile_id, profile_version, profile_hash],
            |row| row.get(0),
        )
        .optional()?;
    let event_hash = lifecycle_event_hash(
        profile_id,
        profile_version,
        profile_hash,
        from_state,
        to_state,
        actor,
        reason,
        previous_event_hash.as_deref(),
        occurred_at,
    )?;
    transaction.execute(
        "INSERT INTO profile_lifecycle_events (
             profile_id, profile_version, profile_hash, from_state, to_state,
             actor, reason, previous_event_hash, event_hash, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            profile_id,
            profile_version,
            profile_hash,
            from_state,
            to_state.as_str(),
            actor,
            reason,
            previous_event_hash,
            event_hash,
            occurred_at,
        ],
    )?;
    Ok(())
}

fn validation_hash(schema_hash: &str, profile_hash: &str) -> Result<String> {
    canonical_hash(&json!({
        "contract": VALIDATION_HASH_CONTRACT,
        "profileHash": profile_hash,
        "schemaHash": schema_hash,
        "validator": "migration-stream-profile-schema-and-semantic/v1",
    }))
}

#[allow(clippy::too_many_arguments)]
fn approval_hash(
    approval_id: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    approver: &str,
    reason: &str,
    acknowledged: bool,
    approved_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "acknowledged": acknowledged,
        "approvalId": approval_id,
        "approvedAt": approved_at,
        "approver": approver,
        "contract": APPROVAL_HASH_CONTRACT,
        "profileHash": profile_hash,
        "profileId": profile_id,
        "profileVersion": profile_version,
        "reason": reason,
    }))
}

#[allow(clippy::too_many_arguments)]
fn lifecycle_event_hash(
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    from_state: Option<&str>,
    to_state: ProfileLifecycleStatus,
    actor: &str,
    reason: &str,
    previous_event_hash: Option<&str>,
    occurred_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "actor": actor,
        "contract": LIFECYCLE_HASH_CONTRACT,
        "fromState": from_state,
        "occurredAt": occurred_at,
        "previousEventHash": previous_event_hash,
        "profileHash": profile_hash,
        "profileId": profile_id,
        "profileVersion": profile_version,
        "reason": reason,
        "toState": to_state.as_str(),
    }))
}

fn enforce_document_limits(document: &Value, limits: &RegistryLimits) -> Result<()> {
    limits.validate()?;
    let canonical_length = canonical_json_string(document)?.len();
    if canonical_length > limits.maximum_document_bytes {
        return Err(limit_error(
            "canonical profile document bytes",
            canonical_length,
            limits.maximum_document_bytes,
        ));
    }
    let entities = required_array(document, "entities")?;
    if entities.len() > limits.maximum_entities {
        return Err(limit_error(
            "profile entities",
            entities.len(),
            limits.maximum_entities,
        ));
    }
    let mut field_count = 0_usize;
    for entity in entities {
        field_count = field_count
            .checked_add(required_array(entity, "fields")?.len())
            .ok_or_else(|| KernelError::PolicyInvalid("profile field count overflow".to_owned()))?;
    }
    if field_count > limits.maximum_fields {
        return Err(limit_error(
            "profile fields",
            field_count,
            limits.maximum_fields,
        ));
    }
    let rules = required_array(document, "rules")?;
    if rules.len() > limits.maximum_rules {
        return Err(limit_error(
            "profile rules",
            rules.len(),
            limits.maximum_rules,
        ));
    }
    let relationships = required_array(document, "relationships")?;
    if relationships.len() > limits.maximum_relationships {
        return Err(limit_error(
            "profile relationships",
            relationships.len(),
            limits.maximum_relationships,
        ));
    }
    enforce_string_limits(document, limits.maximum_string_bytes)
}

fn enforce_string_limits(document: &Value, limit: usize) -> Result<()> {
    let mut pending = vec![document];
    while let Some(value) = pending.pop() {
        match value {
            Value::String(text) if text.len() > limit => {
                return Err(limit_error("profile string bytes", text.len(), limit));
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > limit {
                        return Err(limit_error("profile key bytes", key.len(), limit));
                    }
                    pending.push(value);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn required_array<'a>(document: &'a Value, name: &str) -> Result<&'a [Value]> {
    document
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| KernelError::ProfileSemantic(format!("profile {name} must be an array")))
}

fn open_registry_read_only(workspace: &Path) -> Result<Connection> {
    let database_path = workspace.join(DATABASE_FILENAME);
    if !database_path.exists() {
        return Err(KernelError::NotFound(format!(
            "workspace registry database {}",
            database_path.display()
        )));
    }
    let connection = crate::state::open_read_only(&database_path)?;
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != SCHEMA_VERSION {
        return Err(KernelError::Conflict(format!(
            "profile inspection requires schema version {SCHEMA_VERSION}; found {schema_version}"
        )));
    }
    Ok(connection)
}

fn validate_limit(name: &str, value: usize, absolute: usize) -> Result<()> {
    if !(1..=absolute).contains(&value) {
        return Err(KernelError::PolicyInvalid(format!(
            "profile registry {name} must be between 1 and {absolute}"
        )));
    }
    Ok(())
}

fn limit_error(label: &str, actual: usize, limit: usize) -> KernelError {
    KernelError::PolicyInvalid(format!(
        "{label} count/size {actual} exceeds registry limit {limit}"
    ))
}

fn require_integrity_text(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > ABSOLUTE_MAX_STRING_BYTES {
        return Err(registry_integrity(format!(
            "{label} is blank or exceeds the registry string bound"
        )));
    }
    Ok(())
}

fn require_rfc3339(label: &str, value: &str) -> Result<()> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|error| registry_integrity(format!("{label} is invalid: {error}")))?;
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn registry_integrity(detail: impl Into<String>) -> KernelError {
    KernelError::IntegrityFailed(format!("profile registry: {}", detail.into()))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;
    use crate::profile::assert_runtime_authority;

    fn with_immutability_guard_disabled<T>(
        connection: &Connection,
        name: &str,
        exact_sql: &str,
        action: impl FnOnce(&Connection) -> T,
    ) -> T {
        connection
            .execute_batch(&format!("DROP TRIGGER {name};"))
            .expect("open test-only immutable-history mutation window");
        let result = action(connection);
        connection
            .execute_batch(exact_sql)
            .expect("restore exact immutable-history guard");
        result
    }

    struct RegistryFixture {
        _temporary: TempDir,
        workspace: PathBuf,
        profile_path: PathBuf,
    }

    impl RegistryFixture {
        fn reviewed() -> Self {
            Self::from_value(&reviewed_profile())
        }

        fn from_value(profile: &Value) -> Self {
            let temporary = tempfile::tempdir().expect("temporary registry fixture");
            let workspace = temporary.path().join("workspace");
            let inputs = temporary.path().join("inputs");
            fs::create_dir_all(&inputs).expect("profile input directory");
            let profile_path = inputs.join("profile.json");
            fs::write(
                &profile_path,
                serde_json::to_vec_pretty(&profile).expect("serialize profile"),
            )
            .expect("write profile");
            Self {
                _temporary: temporary,
                workspace,
                profile_path,
            }
        }

        fn register(&self) -> ProfileVersionView {
            register_profile(
                &self.workspace,
                &schema_path(),
                &self.profile_path,
                &RegistryLimits::default(),
            )
            .expect("register profile")
        }
    }

    fn project_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    fn schema_path() -> PathBuf {
        project_path("schemas/migration-object-profile.schema.json")
    }

    fn reviewed_profile() -> Value {
        serde_json::from_slice(
            &fs::read(project_path(
                "profiles/synthetic.customer-account.readiness/profile.example.json",
            ))
            .expect("read reviewed profile"),
        )
        .expect("parse reviewed profile")
    }

    fn approve(fixture: &RegistryFixture, view: &ProfileVersionView) -> ProfileVersionView {
        approve_profile(
            &fixture.workspace,
            &view.profile_id,
            &view.profile_version,
            &view.profile_hash,
            "practitioner@example.test",
            "Synthetic profile reviewed for local WP-003 validation",
            true,
            &RegistryLimits::default(),
        )
        .expect("approve profile")
    }

    #[test]
    fn registration_is_bounded_idempotent_and_collision_safe() {
        let fixture = RegistryFixture::reviewed();
        let first = fixture.register();
        assert_eq!(first.declared_status, "REVIEWED_DRAFT");
        assert_eq!(
            first.lifecycle_status,
            ProfileLifecycleStatus::ReviewedDraft
        );
        assert_eq!(first.authority, ProfileAuthority::None);
        assert!(!first.can_start_new_runs);
        assert_eq!(first.lifecycle_events.len(), 1);
        assert_eq!(first.lifecycle_events[0].from_state, None);

        let repeated = fixture.register();
        assert_eq!(repeated, first);
        assert_eq!(
            inspect_profile(
                &fixture.workspace,
                &first.profile_id,
                &first.profile_version
            )
            .expect("inspect registered profile"),
            first
        );

        let mut conflicting = reviewed_profile();
        conflicting["title"] = json!("Conflicting immutable bytes");
        fs::write(
            &fixture.profile_path,
            serde_json::to_vec_pretty(&conflicting).expect("serialize collision"),
        )
        .expect("write collision");
        let error = register_profile(
            &fixture.workspace,
            &schema_path(),
            &fixture.profile_path,
            &RegistryLimits::default(),
        )
        .expect_err("same identity/version with changed bytes must conflict");
        assert!(matches!(error, KernelError::Conflict(_)));
    }

    #[test]
    fn registration_enforces_configured_shape_and_string_bounds() {
        let fixture = RegistryFixture::reviewed();
        let limits = RegistryLimits {
            maximum_entities: 1,
            ..RegistryLimits::default()
        };
        let error = register_profile(
            &fixture.workspace,
            &schema_path(),
            &fixture.profile_path,
            &limits,
        )
        .expect_err("entity bound must be enforced before registration");
        assert!(matches!(error, KernelError::PolicyInvalid(_)));
        assert!(!fixture.workspace.join(DATABASE_FILENAME).exists());

        let limits = RegistryLimits {
            maximum_string_bytes: 8,
            ..RegistryLimits::default()
        };
        let error = register_profile(
            &fixture.workspace,
            &schema_path(),
            &fixture.profile_path,
            &limits,
        )
        .expect_err("string bound must be enforced before registration");
        assert!(matches!(error, KernelError::PolicyInvalid(_)));
        assert!(!fixture.workspace.join(DATABASE_FILENAME).exists());
    }

    #[test]
    fn approval_requires_acknowledgement_and_is_immutable_and_idempotent() {
        let fixture = RegistryFixture::reviewed();
        let registered = fixture.register();
        let error = approve_profile(
            &fixture.workspace,
            &registered.profile_id,
            &registered.profile_version,
            &registered.profile_hash,
            "practitioner@example.test",
            "Synthetic review",
            false,
            &RegistryLimits::default(),
        )
        .expect_err("approval acknowledgement is mandatory");
        assert!(matches!(error, KernelError::PolicyInvalid(_)));

        let approved = approve(&fixture, &registered);
        assert_eq!(approved.declared_status, "REVIEWED_DRAFT");
        assert_eq!(approved.lifecycle_status, ProfileLifecycleStatus::Approved);
        assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);
        assert!(approved.can_start_new_runs);
        assert_eq!(approved.lifecycle_events.len(), 2);
        let approval = approved.approval.as_ref().expect("approval view");
        assert!(approval.acknowledged);

        let repeated = approve(&fixture, &registered);
        assert_eq!(repeated, approved);
        let error = approve_profile(
            &fixture.workspace,
            &registered.profile_id,
            &registered.profile_version,
            &registered.profile_hash,
            "practitioner@example.test",
            "Different immutable reason",
            true,
            &RegistryLimits::default(),
        )
        .expect_err("different repeated approval must conflict");
        assert!(matches!(error, KernelError::Conflict(_)));

        let writer = WorkspaceWriter::open(&fixture.workspace).expect("open approved registry");
        let loaded = load_approved_profile(
            &writer,
            &schema_path(),
            &registered.profile_id,
            &registered.profile_version,
            &registered.profile_hash,
        )
        .expect("load exact approved profile");
        assert_runtime_authority(&loaded.profile);
        assert_eq!(loaded.profile_hash, registered.profile_hash);
        assert_eq!(
            loaded.view.lifecycle_status,
            ProfileLifecycleStatus::Approved
        );
    }

    #[test]
    fn generated_and_unapproved_drafts_cannot_gain_runtime_authority() {
        let reviewed_fixture = RegistryFixture::reviewed();
        let reviewed = reviewed_fixture.register();
        let writer =
            WorkspaceWriter::open(&reviewed_fixture.workspace).expect("open draft registry");
        let error = load_approved_profile(
            &writer,
            &schema_path(),
            &reviewed.profile_id,
            &reviewed.profile_version,
            &reviewed.profile_hash,
        )
        .expect_err("reviewed draft is not authority");
        assert!(matches!(error, KernelError::ProfileNotApproved(_)));
        drop(writer);

        let mut generated_document = reviewed_profile();
        generated_document["version"] = json!("0.1.1");
        generated_document["status"] = json!("GENERATED_DRAFT");
        let generated_fixture = RegistryFixture::from_value(&generated_document);
        let generated = generated_fixture.register();
        assert_eq!(
            generated.lifecycle_status,
            ProfileLifecycleStatus::GeneratedDraft
        );
        let error = approve_profile(
            &generated_fixture.workspace,
            &generated.profile_id,
            &generated.profile_version,
            &generated.profile_hash,
            "practitioner@example.test",
            "Generated evidence cannot be approved directly",
            true,
            &RegistryLimits::default(),
        )
        .expect_err("generated draft approval must fail");
        assert!(matches!(error, KernelError::ProfileNotApproved(_)));
    }

    #[test]
    fn caller_declared_authority_and_terminal_labels_are_rejected() {
        for status in ["APPROVED", "DEPRECATED", "RETIRED"] {
            let mut document = reviewed_profile();
            document["version"] = json!(format!("0.2.{}", status.len()));
            document["status"] = json!(status);
            if status == "APPROVED" {
                document["approval"] = json!({
                    "approvedBy": "untrusted-caller",
                    "approvedAt": "2026-07-22T00:00:00Z",
                    "evidence": "caller-shaped approval evidence"
                });
            }
            let fixture = RegistryFixture::from_value(&document);
            let error = register_profile(
                &fixture.workspace,
                &schema_path(),
                &fixture.profile_path,
                &RegistryLimits::default(),
            )
            .expect_err("caller lifecycle label must not grant registry authority");
            assert!(matches!(error, KernelError::ProfileNotApproved(_)));
        }
    }

    #[test]
    fn deprecation_and_retirement_block_new_approved_loads() {
        let fixture = RegistryFixture::reviewed();
        let registered = fixture.register();
        let approved = approve(&fixture, &registered);
        let deprecated = deprecate_profile(
            &fixture.workspace,
            &approved.profile_id,
            &approved.profile_version,
            &approved.profile_hash,
            "governance@example.test",
            "Superseded for new runs",
            &RegistryLimits::default(),
        )
        .expect("deprecate profile");
        assert_eq!(
            deprecated.lifecycle_status,
            ProfileLifecycleStatus::Deprecated
        );
        assert_eq!(deprecated.authority, ProfileAuthority::None);
        assert!(!deprecated.can_start_new_runs);
        {
            let writer =
                WorkspaceWriter::open(&fixture.workspace).expect("open deprecated registry");
            let error = load_approved_profile(
                &writer,
                &schema_path(),
                &deprecated.profile_id,
                &deprecated.profile_version,
                &deprecated.profile_hash,
            )
            .expect_err("deprecated profile cannot start a run");
            assert!(matches!(error, KernelError::ProfileNotApproved(_)));
        }

        let retired = retire_profile(
            &fixture.workspace,
            &deprecated.profile_id,
            &deprecated.profile_version,
            &deprecated.profile_hash,
            "governance@example.test",
            "Superseded profile retired",
            &RegistryLimits::default(),
        )
        .expect("retire profile");
        assert_eq!(retired.lifecycle_status, ProfileLifecycleStatus::Retired);
        assert_eq!(retired.authority, ProfileAuthority::None);
        let repeated = retire_profile(
            &fixture.workspace,
            &retired.profile_id,
            &retired.profile_version,
            &retired.profile_hash,
            "governance@example.test",
            "Superseded profile retired",
            &RegistryLimits::default(),
        )
        .expect("idempotent retirement");
        assert_eq!(repeated, retired);
        let writer = WorkspaceWriter::open(&fixture.workspace).expect("open retired registry");
        let error = load_approved_profile(
            &writer,
            &schema_path(),
            &retired.profile_id,
            &retired.profile_version,
            &retired.profile_hash,
        )
        .expect_err("retired profile cannot start a run");
        assert!(matches!(error, KernelError::ProfileNotApproved(_)));
    }

    #[test]
    fn integrity_replay_detects_lifecycle_tampering() {
        let fixture = RegistryFixture::reviewed();
        let registered = fixture.register();
        let _approved = approve(&fixture, &registered);
        let writer = WorkspaceWriter::open(&fixture.workspace).expect("open registry for tamper");
        with_immutability_guard_disabled(
            &writer.connection,
            "profile_lifecycle_events_no_update",
            PROFILE_LIFECYCLE_UPDATE_TRIGGER,
            |connection| {
                connection
                    .execute(
                        "UPDATE profile_lifecycle_events
                         SET reason = 'forged lifecycle reason'
                         WHERE profile_id = ?1 AND profile_version = ?2 AND to_state = 'APPROVED'",
                        params![registered.profile_id, registered.profile_version],
                    )
                    .expect("inject lifecycle tamper");
            },
        );
        let mut issues = Vec::new();
        let (checked_versions, checked_approvals) =
            verify_integrity(&writer.connection, &mut issues).expect("registry integrity replay");
        assert_eq!(checked_versions, 1);
        assert_eq!(checked_approvals, 1);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("lifecycle event hash is invalid")),
            "unexpected issues: {issues:?}"
        );
        drop(writer);
        let error = inspect_profile(
            &fixture.workspace,
            &registered.profile_id,
            &registered.profile_version,
        )
        .expect_err("tampered lifecycle must not be inspected as trusted evidence");
        assert!(matches!(error, KernelError::IntegrityFailed(_)));
    }

    #[test]
    fn lifecycle_tail_is_delete_guarded_and_missing_guard_fails_closed() {
        let fixture = RegistryFixture::reviewed();
        let registered = fixture.register();
        let approved = approve(&fixture, &registered);
        let deprecated = deprecate_profile(
            &fixture.workspace,
            &approved.profile_id,
            &approved.profile_version,
            &approved.profile_hash,
            "governance@example.test",
            "Superseded for new runs",
            &RegistryLimits::default(),
        )
        .expect("deprecate profile");
        assert_eq!(
            deprecated.lifecycle_status,
            ProfileLifecycleStatus::Deprecated
        );
        assert_eq!(deprecated.authority, ProfileAuthority::None);

        let writer = WorkspaceWriter::open(&fixture.workspace).expect("open registry");
        let deletion = writer.connection.execute(
            "DELETE FROM profile_lifecycle_events
             WHERE profile_id = ?1 AND profile_version = ?2 AND to_state = 'DEPRECATED'",
            params![deprecated.profile_id, deprecated.profile_version],
        );
        let error = deletion.expect_err("lifecycle tail deletion must be rejected");
        assert!(
            error
                .to_string()
                .contains("profile lifecycle history is immutable")
        );
        drop(writer);

        let still_deprecated = inspect_profile(
            &fixture.workspace,
            &deprecated.profile_id,
            &deprecated.profile_version,
        )
        .expect("inspect guarded deprecated profile");
        assert_eq!(
            still_deprecated.lifecycle_status,
            ProfileLifecycleStatus::Deprecated
        );
        assert_eq!(still_deprecated.authority, ProfileAuthority::None);

        let writer = WorkspaceWriter::open(&fixture.workspace).expect("reopen registry");
        writer
            .connection
            .execute_batch("DROP TRIGGER profile_lifecycle_events_no_delete;")
            .expect("remove lifecycle deletion guard for fail-closed proof");
        let mut issues = Vec::new();
        let integrity_error = verify_integrity(&writer.connection, &mut issues)
            .expect_err("global replay must reject a missing lifecycle deletion guard");
        assert!(
            integrity_error
                .to_string()
                .contains("profile_lifecycle_events_no_delete")
        );
        drop(writer);
        let error = inspect_profile(
            &fixture.workspace,
            &deprecated.profile_id,
            &deprecated.profile_version,
        )
        .expect_err("missing lifecycle deletion guard must fail closed");
        assert!(matches!(error, KernelError::IntegrityFailed(_)));
        assert!(
            error
                .to_string()
                .contains("profile_lifecycle_events_no_delete")
        );
    }
}

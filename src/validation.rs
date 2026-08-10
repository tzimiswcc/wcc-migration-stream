//! Bounded deterministic validation over canonical projections.
//!
//! Validation reads only reconciled rows from `SQLite` through an immutable
//! mapping version governed by an exact registry-approved profile. Source
//! values are never modified and are never embedded in issue-list evidence.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use regex::{Regex, RegexBuilder};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json},
    compared_validation::{
        COMPARED_FINDING_AUTHORITY_SCHEMA, COMPARED_FINDING_SET_INPUT_SCHEMA,
        CanonicalFieldValueInput, ComparedFindingAuthority, ComparedFindingSetInput,
        ComparedIssueInput, ComparedIssueSubjectInput, RecordLocatorInput,
        RelationshipKeyValueInput, RelationshipRole,
    },
    decisions::DecisionScope,
    effective_values::{effective_value_hash, original_value_hash},
    engagement::MAPPING_SOURCE_REUSE_SCHEMA,
    mapping::{
        MappingDocument, MappingRelationshipBinding, ProjectedRow, load_stored_mapping,
        project_record, project_rows_batch,
    },
    profile::assert_runtime_authority,
    profile_registry::{load_approved_profile, load_verified_profile_document},
    state::{
        DATABASE_FILENAME, SCHEMA_VERSION, WorkspaceWriter, ensure_package_cleanup_not_conflicted,
        now_rfc3339, sqlite_integer,
    },
};

const MAPPING_SOURCE_REUSE_ALGORITHM: &str = "migration-stream/mapping-source-reuse/v1";
const MAXIMUM_SOURCE_REUSE_JSON_BYTES: usize = 1024 * 1024;
const MAXIMUM_SOURCE_STRUCTURE_JSON_BYTES: usize = 1024 * 1024;

/// Version of validation policy, result, and status contracts.
pub const VALIDATION_SCHEMA_VERSION: u32 = 1;
/// Stable rule-engine identity persisted with every run.
pub const VALIDATION_ENGINE_VERSION: &str = "migration-stream-validation/v1";

const MAXIMUM_POLICY_BYTES: usize = 256 * 1024;
const MAXIMUM_BATCH_SIZE: usize = 10_000;
const MAXIMUM_ISSUES: usize = 100_000;
const MAXIMUM_PATTERN_BYTES: usize = 4_096;
const MAXIMUM_TOTAL_PATTERN_BYTES: usize = 1024 * 1024;
const MAXIMUM_PATTERN_PROGRAM_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_LOOKUP_ENTRIES: usize = 10_000;
const MAXIMUM_LOOKUP_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_UNIQUE_KEYS: usize = 10_000_000;
const MAXIMUM_UNIQUE_KEY_BYTES: usize = 1024 * 1024 * 1024;
const MAXIMUM_PENDING_ISSUES: usize = 1_000;
const MAXIMUM_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_ISSUE_REPLAY_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_EFFECTIVE_CHAIN_REPLAY_BYTES: usize = 128 * 1024 * 1024;
const MAXIMUM_VALIDATION_METADATA_BYTES: usize = 256 * 1024;
const MAXIMUM_VALIDATION_EVENT_BYTES: usize = 64 * 1024;
const MAXIMUM_READ_PAGE_ISSUES: usize = 201;
const MAXIMUM_READ_PAGE_ISSUE_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_READ_PAGE_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_ACTIVE_RELATIONSHIPS: usize = 64;
const MAXIMUM_RELATIONSHIP_KEY_PAIRS: usize = 16;
const RELATIONSHIP_PARENT_KEY_MESSAGE: &str = "A required relationship parent key is missing.";
const RELATIONSHIP_CHILD_KEY_MESSAGE: &str = "A required relationship child key is missing.";
const RELATIONSHIP_MINIMUM_CHILD_MESSAGE: &str =
    "A required relationship parent has no mapped child row.";
const RELATIONSHIP_PARENT_NOT_FOUND_MESSAGE: &str =
    "A relationship child key has no mapped parent row.";

/// Explicit resource bounds for one validation run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationPolicy {
    pub schema_version: u32,
    pub row_batch_size: usize,
    pub maximum_issues: usize,
    pub maximum_pattern_bytes: usize,
    pub maximum_total_pattern_bytes: usize,
    pub maximum_pattern_program_bytes: usize,
    pub maximum_lookup_entries: usize,
    pub maximum_lookup_bytes: usize,
    pub maximum_unique_keys: usize,
    pub maximum_unique_key_bytes: usize,
}

impl ValidationPolicy {
    /// Read and validate one strict policy document.
    ///
    /// # Errors
    ///
    /// Rejects oversized, duplicate-key, malformed, unknown-field, zero, or
    /// above-ceiling policies.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = read_policy_bounded(path)?;
        let value = parse_unique_json(&bytes)
            .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
        let policy: Self = serde_json::from_value(value)
            .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != VALIDATION_SCHEMA_VERSION {
            return Err(KernelError::PolicyInvalid(format!(
                "validation schemaVersion must be {VALIDATION_SCHEMA_VERSION}"
            )));
        }
        for (name, value, maximum) in [
            ("rowBatchSize", self.row_batch_size, MAXIMUM_BATCH_SIZE),
            ("maximumIssues", self.maximum_issues, MAXIMUM_ISSUES),
            (
                "maximumPatternBytes",
                self.maximum_pattern_bytes,
                MAXIMUM_PATTERN_BYTES,
            ),
            (
                "maximumTotalPatternBytes",
                self.maximum_total_pattern_bytes,
                MAXIMUM_TOTAL_PATTERN_BYTES,
            ),
            (
                "maximumPatternProgramBytes",
                self.maximum_pattern_program_bytes,
                MAXIMUM_PATTERN_PROGRAM_BYTES,
            ),
            (
                "maximumLookupEntries",
                self.maximum_lookup_entries,
                MAXIMUM_LOOKUP_ENTRIES,
            ),
            (
                "maximumLookupBytes",
                self.maximum_lookup_bytes,
                MAXIMUM_LOOKUP_BYTES,
            ),
            (
                "maximumUniqueKeys",
                self.maximum_unique_keys,
                MAXIMUM_UNIQUE_KEYS,
            ),
            (
                "maximumUniqueKeyBytes",
                self.maximum_unique_key_bytes,
                MAXIMUM_UNIQUE_KEY_BYTES,
            ),
        ] {
            if value == 0 || value > maximum {
                return Err(KernelError::PolicyInvalid(format!(
                    "{name} must be in 1..={maximum}; found {value}"
                )));
            }
        }
        Ok(())
    }
}

fn read_policy_bounded(path: &Path) -> Result<Vec<u8>> {
    let named_metadata =
        fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    require_regular_policy_file(path, &named_metadata)?;
    require_policy_size(named_metadata.len())?;

    let file = open_policy_nofollow(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    require_regular_policy_file(path, &opened_metadata)?;
    require_policy_size(opened_metadata.len())?;

    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len())
            .unwrap_or(MAXIMUM_POLICY_BYTES)
            .min(MAXIMUM_POLICY_BYTES),
    );
    let maximum_read = u64::try_from(MAXIMUM_POLICY_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(maximum_read)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() > MAXIMUM_POLICY_BYTES {
        return Err(policy_size_error(bytes.len()));
    }
    Ok(bytes)
}

fn require_regular_policy_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let is_safe = metadata.file_type().is_file();
    #[cfg(windows)]
    let is_safe = is_safe && !crate::durability::is_reparse_point(metadata);
    if !is_safe {
        return Err(KernelError::PolicyInvalid(format!(
            "validation policy must be a regular non-link file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_policy_size(size: u64) -> Result<()> {
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size > MAXIMUM_POLICY_BYTES {
        return Err(policy_size_error(size));
    }
    Ok(())
}

fn policy_size_error(size: usize) -> KernelError {
    KernelError::PolicyInvalid(format!(
        "validation policy is {size} bytes; maximum is {MAXIMUM_POLICY_BYTES}"
    ))
}

#[cfg(unix)]
fn open_policy_nofollow(path: &Path) -> Result<File> {
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
fn open_policy_nofollow(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_policy_nofollow(path: &Path) -> Result<File> {
    File::open(path).map_err(|error| KernelError::io(path, error))
}

/// Cooperative interruption controls used by the CLI and recovery proof.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationControl {
    pub cancel_file: Option<PathBuf>,
    pub cancel_after_batches: Option<u64>,
}

/// Durable validation state. Only `COMPLETE` may expose a result hash.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationRunState {
    Running,
    Complete,
    Failed,
    Interrupted,
    BoundExceeded,
}

impl ValidationRunState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Complete => "COMPLETE",
            Self::Failed => "FAILED",
            Self::Interrupted => "INTERRUPTED",
            Self::BoundExceeded => "BOUND_EXCEEDED",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "RUNNING" => Ok(Self::Running),
            "COMPLETE" => Ok(Self::Complete),
            "FAILED" => Ok(Self::Failed),
            "INTERRUPTED" => Ok(Self::Interrupted),
            "BOUND_EXCEEDED" => Ok(Self::BoundExceeded),
            other => Err(KernelError::IntegrityFailed(format!(
                "unknown validation state {other:?}"
            ))),
        }
    }
}

/// Stable completed validation summary. Run/job IDs and timestamps are
/// deliberately excluded so equivalent accepted source content can replay to
/// the same result hash under the same semantic authorities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationResult {
    pub schema_version: u32,
    pub source_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub rule_set_hash: String,
    pub engine_version: String,
    pub policy_hash: String,
    pub rows_processed: u64,
    pub issue_count: u64,
    pub severity_counts: BTreeMap<String, u64>,
    pub rule_counts: BTreeMap<String, u64>,
    pub entity_counts: BTreeMap<String, u64>,
    pub field_counts: BTreeMap<String, u64>,
    pub source_file_counts: BTreeMap<String, u64>,
    pub issue_hashes: Vec<String>,
}

/// Result of one newly completed validation attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub schema_version: u32,
    pub run_id: String,
    pub identity_hash: String,
    pub result_hash: String,
    pub result: ValidationResult,
}

/// Readable lifecycle/status evidence for any attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationStatus {
    pub schema_version: u32,
    pub run_id: String,
    pub state: ValidationRunState,
    pub identity_hash: String,
    pub job_id: String,
    pub source_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub rule_set_hash: String,
    pub engine_version: String,
    pub policy_hash: String,
    pub rows_processed: u64,
    pub issue_count: u64,
    pub result_hash: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    rule_id: String,
    severity: String,
    entity_id: String,
    field_id: String,
    message: String,
    kind: CompiledKind,
}

#[derive(Clone, Debug)]
struct CompiledRelationship {
    relationship_id: String,
    from_entity_id: String,
    to_entity_id: String,
    cardinality: String,
    key_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct RelationshipLineageIdentity {
    relationship_id: String,
    relationship_role: String,
    related_entity_id: String,
    related_field_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct RelationshipKeyState {
    parent_keys: HashSet<String>,
    child_keys: HashSet<String>,
}

#[derive(Clone, Debug)]
enum CompiledKind {
    Required,
    MaximumLength(usize),
    AllowedValues(HashSet<String>),
    Pattern(Regex),
    Unique(Vec<String>),
    ConditionalRequired {
        when_field: String,
        equals: String,
    },
    ReferenceLookup {
        table_id: String,
        table_version: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct IssueEvidence {
    pub(crate) issue_id: String,
    pub(crate) issue_hash: String,
    pub(crate) severity: String,
    pub(crate) rule_id: String,
    pub(crate) issue_code: String,
    pub(crate) display_message: String,
    pub(crate) entity_id: String,
    pub(crate) field_id: String,
    pub(crate) source_file: String,
    pub(crate) source_ordinal: u64,
    pub(crate) line_start: u64,
    pub(crate) line_end: u64,
    pub(crate) lineage_json: String,
}

/// One exact effective-value substitution used only by WP-005 bounded
/// re-evaluation. The original projected row and its lineage are never
/// modified or persisted through this value.
#[derive(Clone, Debug)]
pub(crate) struct EffectiveOverlayInput {
    pub(crate) source_file: String,
    pub(crate) source_ordinal: u64,
    pub(crate) entity_id: String,
    pub(crate) field_id: String,
    pub(crate) value: Option<String>,
}

/// Deterministic, non-persisting output of one conservative full-scope
/// re-evaluation. WP-005 persists a separate immutable envelope around these
/// engine facts; historical validation rows remain untouched.
#[derive(Clone, Debug)]
pub(crate) struct EffectiveValidationEvaluation {
    pub(crate) rows_processed: u64,
    pub(crate) issues: Vec<IssueEvidence>,
    pub(crate) scope_kind: &'static str,
}

#[derive(Clone, Default)]
struct ValidationAccumulator {
    qualify_field_counts: bool,
    rows_processed: u64,
    issue_count: usize,
    issue_hashes: Vec<String>,
    pending_issues: Vec<IssueEvidence>,
    pending_issue_bytes: usize,
    severity_counts: BTreeMap<String, u64>,
    rule_counts: BTreeMap<String, u64>,
    entity_counts: BTreeMap<String, u64>,
    field_counts: BTreeMap<String, u64>,
    source_file_counts: BTreeMap<String, u64>,
    unique_values: HashMap<(String, String), HashSet<String>>,
    unique_key_count: usize,
    unique_key_bytes: usize,
}

impl ValidationAccumulator {
    fn for_mapping(mapping: &MappingDocument) -> Self {
        Self {
            qualify_field_counts: mapping.has_explicit_entity_scope(),
            ..Self::default()
        }
    }

    fn push_issue(&mut self, issue: IssueEvidence, maximum: usize) -> Result<()> {
        if self.issue_count >= maximum {
            return Err(KernelError::ValidationBound(format!(
                "maximum issue count {maximum} reached"
            )));
        }
        let issue_bytes = [
            issue.issue_id.len(),
            issue.issue_hash.len(),
            issue.severity.len(),
            issue.rule_id.len(),
            issue.issue_code.len(),
            issue.display_message.len(),
            issue.entity_id.len(),
            issue.field_id.len(),
            issue.source_file.len(),
            issue.lineage_json.len(),
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
        .ok_or_else(|| KernelError::ValidationBound("pending issue bytes overflow".to_owned()))?;
        let pending_issue_bytes = self
            .pending_issue_bytes
            .checked_add(issue_bytes)
            .ok_or_else(|| {
                KernelError::ValidationBound("pending issue bytes overflow".to_owned())
            })?;
        if pending_issue_bytes > MAXIMUM_ISSUE_REPLAY_BYTES {
            return Err(KernelError::ValidationBound(format!(
                "pending issue evidence exceeds {MAXIMUM_ISSUE_REPLAY_BYTES} bytes"
            )));
        }
        increment(&mut self.severity_counts, &issue.severity)?;
        increment(&mut self.rule_counts, &issue.rule_id)?;
        increment(&mut self.entity_counts, &issue.entity_id)?;
        let field_identity = if self.qualify_field_counts {
            format!("{}.{}", issue.entity_id, issue.field_id)
        } else {
            issue.field_id.clone()
        };
        increment(&mut self.field_counts, &field_identity)?;
        increment(&mut self.source_file_counts, &issue.source_file)?;
        self.issue_hashes.push(issue.issue_hash.clone());
        self.pending_issues.push(issue);
        self.pending_issue_bytes = pending_issue_bytes;
        self.issue_count = self
            .issue_count
            .checked_add(1)
            .ok_or_else(|| KernelError::ValidationBound("issue count overflow".to_owned()))?;
        Ok(())
    }

    fn clear_pending_issues(&mut self) {
        self.pending_issues.clear();
        self.pending_issue_bytes = 0;
    }

    fn reserve_key_state(&mut self, bytes: usize, policy: &ValidationPolicy) -> Result<()> {
        let next_count = self.unique_key_count.saturating_add(1);
        let next_bytes = self.unique_key_bytes.saturating_add(bytes);
        if next_count > policy.maximum_unique_keys || next_bytes > policy.maximum_unique_key_bytes {
            return Err(KernelError::ValidationBound(
                "unique/relationship key state exceeded its configured bound".to_owned(),
            ));
        }
        self.unique_key_count = next_count;
        self.unique_key_bytes = next_bytes;
        Ok(())
    }
}

/// Run bounded validation with default non-cancelling controls.
///
/// # Errors
///
/// Returns typed authority, mapping, policy, bound, interruption, integrity, or
/// persistence errors. Non-complete attempts never receive a result hash.
pub fn run_validation(
    workspace: &Path,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
) -> Result<ValidationReport> {
    run_validation_with_control(
        workspace,
        job_id,
        mapping_id,
        mapping_version,
        profile_schema_path,
        policy,
        &ValidationControl::default(),
    )
}

/// Run bounded validation with cooperative interruption controls.
///
/// # Errors
///
/// Returns typed failures while preserving an immutable lifecycle attempt.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run_validation_with_control(
    workspace: &Path,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
    control: &ValidationControl,
) -> Result<ValidationReport> {
    run_validation_internal(
        workspace,
        None,
        None,
        job_id,
        mapping_id,
        mapping_version,
        profile_schema_path,
        policy,
        control,
    )
}

/// Run ordinary validation under one caller-reserved immutable run identity
/// while retaining the writer authority that created that reservation.
///
/// The engagement authority layer uses this bounded internal variant so a
/// crash between validation persistence and engagement-pointer completion can
/// be recovered by exact identity instead of a best-effort row search. Keeping
/// the same writer also prevents a competing command from occupying the lock
/// between reservation, validation, and result publication.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_validation_with_reserved_writer(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
) -> Result<ValidationReport> {
    if !Uuid::parse_str(run_id).is_ok_and(|parsed| parsed.to_string() == run_id) {
        return Err(KernelError::ValidationInvalid(
            "preallocated validation run ID is not one canonical UUID".to_owned(),
        ));
    }
    writer.verify()?;
    policy.validate()?;
    run_validation_with_writer(
        writer,
        Some(run_id),
        None,
        job_id,
        mapping_id,
        mapping_version,
        profile_schema_path,
        policy,
        &ValidationControl::default(),
    )
}

#[derive(Clone, Copy)]
struct ReusedSourceValidation<'a> {
    run_id: &'a str,
    source_reuse_id: &'a str,
    source_reuse_hash: &'a str,
}

/// Run validation for a revised accepted source under one exact immutable
/// mapping-source reuse authority.
///
/// The ordinary validation entry points intentionally continue to require the
/// intake source embedded in the immutable mapping. This dedicated path is the
/// only exception: it independently replays the exact reuse authority before
/// creating any validation row, binds that authority into the run identity,
/// and uses the caller's preallocated run identity for atomic Tour registration.
///
/// An exact already-COMPLETE run is returned idempotently. Any other existing
/// run state or identity mismatch fails closed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_validation_with_source_reuse_writer(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    source_reuse_id: &str,
    source_reuse_hash: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
) -> Result<ValidationReport> {
    if run_id.is_empty()
        || run_id.len() > 128
        || source_reuse_id.is_empty()
        || source_reuse_id.len() > 256
        || !is_lowercase_sha256(source_reuse_hash)
    {
        return Err(KernelError::ValidationInvalid(
            "reused-source validation identities are outside their bounded contract".to_owned(),
        ));
    }
    writer.verify()?;
    policy.validate()?;
    run_validation_with_writer(
        writer,
        Some(run_id),
        Some(ReusedSourceValidation {
            run_id,
            source_reuse_id,
            source_reuse_hash,
        }),
        job_id,
        mapping_id,
        mapping_version,
        profile_schema_path,
        policy,
        &ValidationControl::default(),
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_validation_internal(
    workspace: &Path,
    preallocated_run_id: Option<&str>,
    reused_source: Option<ReusedSourceValidation<'_>>,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
    control: &ValidationControl,
) -> Result<ValidationReport> {
    policy.validate()?;
    let writer = WorkspaceWriter::open(workspace)?;
    run_validation_with_writer(
        &writer,
        preallocated_run_id,
        reused_source,
        job_id,
        mapping_id,
        mapping_version,
        profile_schema_path,
        policy,
        control,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_validation_with_writer(
    writer: &WorkspaceWriter,
    preallocated_run_id: Option<&str>,
    reused_source: Option<ReusedSourceValidation<'_>>,
    job_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    profile_schema_path: &Path,
    policy: &ValidationPolicy,
    control: &ValidationControl,
) -> Result<ValidationReport> {
    let mapping = load_stored_mapping(&writer.connection, mapping_id, mapping_version)?;
    let (job_state, source_hash): (String, Option<String>) = writer
        .connection
        .query_row(
            "SELECT state, source_hash FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| KernelError::ValidationInvalid(format!("unknown intake job {job_id}")))?;
    if job_state != "COMPLETE" {
        return Err(KernelError::ValidationInvalid(format!(
            "validation requires a COMPLETE intake job; {job_id} is {job_state}"
        )));
    }
    let source_hash = source_hash.ok_or_else(|| {
        KernelError::IntegrityFailed(format!("completed job {job_id} has no source hash"))
    })?;
    match reused_source {
        Some(reuse) => {
            if source_hash == mapping.document.source.package_sha256 {
                return Err(KernelError::ValidationInvalid(
                    "reused-source validation requires an explicitly revised source package"
                        .to_owned(),
                ));
            }
            verify_mapping_source_reuse_authority(
                &writer.connection,
                reuse.source_reuse_id,
                reuse.source_reuse_hash,
                job_id,
                &source_hash,
                &mapping.document.profile.profile_id,
                &mapping.document.profile.version,
                &mapping.document.profile.sha256,
                &mapping.document.mapping_id,
                &mapping.document.version,
                &mapping.mapping_hash,
            )?;
        }
        None if source_hash != mapping.document.source.package_sha256 => {
            return Err(KernelError::ValidationInvalid(
                "validation job source hash differs from immutable mapping source identity"
                    .to_owned(),
            ));
        }
        None => {}
    }
    ensure_package_cleanup_not_conflicted(&writer.connection, job_id)?;
    if mapping.document.profile.profile_id.is_empty() {
        return Err(KernelError::ValidationInvalid(
            "mapping has no profile identity".to_owned(),
        ));
    }
    let approved = load_approved_profile(
        writer,
        profile_schema_path,
        &mapping.document.profile.profile_id,
        &mapping.document.profile.version,
        &mapping.document.profile.sha256,
    )?;
    assert_runtime_authority(&approved.profile);
    if !approved.view.can_start_new_runs {
        return Err(KernelError::ProfileNotApproved(
            "approved profile registry view blocks new validation runs".to_owned(),
        ));
    }
    if approved.profile_hash != mapping.document.profile.sha256 {
        return Err(KernelError::IntegrityFailed(
            "registry authority handle differs from mapping profile hash".to_owned(),
        ));
    }
    let profile_document = approved.profile.document();
    let rules_value = profile_document.get("rules").cloned().ok_or_else(|| {
        KernelError::ValidationInvalid("approved profile has no rules".to_owned())
    })?;
    let rule_set_hash = canonical_hash(&rules_value)?;
    let policy_value = serde_json::to_value(policy)?;
    let policy_json = canonical_json_string(&policy_value)?;
    let policy_hash = canonical_hash(&policy_value)?;
    let identity_hash = validation_identity_hash(
        &source_hash,
        &mapping.document.profile.sha256,
        &mapping.mapping_hash,
        &rule_set_hash,
        &policy_hash,
        reused_source.map(|reuse| reuse.source_reuse_hash),
    )?;
    let generated_run_id;
    let run_id = if let Some(run_id) = preallocated_run_id {
        if reused_source.is_some_and(|reuse| reuse.run_id != run_id) {
            return Err(KernelError::IntegrityFailed(
                "reused-source validation run identity differs from its reservation".to_owned(),
            ));
        }
        run_id
    } else {
        generated_run_id = Uuid::new_v4().to_string();
        &generated_run_id
    };
    if let Some(reuse) = reused_source
        && let Some(report) = load_idempotent_reused_validation(
            &writer.connection,
            run_id,
            &identity_hash,
            job_id,
            &source_hash,
            &mapping.document,
            &mapping.mapping_hash,
            &rule_set_hash,
            &policy_hash,
            reuse.source_reuse_id,
            reuse.source_reuse_hash,
        )?
    {
        return Ok(report);
    }
    let (rules, relationships) =
        match validate_lookup_bounds(&mapping.document, policy).and_then(|()| {
            Ok((
                compile_rules(&rules_value, &mapping.document, policy)?,
                compile_relationships(&mapping.document)?,
            ))
        }) {
            Ok(compiled) => compiled,
            Err(error @ KernelError::ValidationBound(_)) => {
                create_run(
                    writer,
                    run_id,
                    &identity_hash,
                    job_id,
                    &source_hash,
                    &mapping.document,
                    &mapping.mapping_hash,
                    &rule_set_hash,
                    &policy_json,
                    &policy_hash,
                    reused_source,
                )?;
                mark_terminal(
                    writer,
                    run_id,
                    ValidationRunState::BoundExceeded,
                    "VALIDATION_BOUND_EXCEEDED",
                    &error.to_string(),
                )?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
    create_run(
        writer,
        run_id,
        &identity_hash,
        job_id,
        &source_hash,
        &mapping.document,
        &mapping.mapping_hash,
        &rule_set_hash,
        &policy_json,
        &policy_hash,
        reused_source,
    )?;

    let outcome = execute_rules(
        writer,
        run_id,
        job_id,
        &source_hash,
        &mapping.document,
        &mapping.mapping_hash,
        &rules,
        &relationships,
        policy,
        control,
    );
    let accumulator = match outcome {
        Ok(accumulator) => accumulator,
        Err(error) => {
            let (state, code) = match &error {
                KernelError::Cancelled(_) => {
                    (ValidationRunState::Interrupted, "COOPERATIVE_CANCELLED")
                }
                KernelError::ValidationBound(_) => (
                    ValidationRunState::BoundExceeded,
                    "VALIDATION_BOUND_EXCEEDED",
                ),
                _ => (ValidationRunState::Failed, error.code()),
            };
            mark_terminal(writer, run_id, state, code, &error.to_string())?;
            return Err(error);
        }
    };

    let mut issue_hashes = accumulator.issue_hashes.clone();
    issue_hashes.sort_unstable();
    let result = ValidationResult {
        schema_version: VALIDATION_SCHEMA_VERSION,
        source_hash,
        profile_id: mapping.document.profile.profile_id.clone(),
        profile_version: mapping.document.profile.version.clone(),
        profile_hash: mapping.document.profile.sha256.clone(),
        mapping_id: mapping.document.mapping_id.clone(),
        mapping_version: mapping.document.version.clone(),
        mapping_hash: mapping.mapping_hash,
        rule_set_hash,
        engine_version: VALIDATION_ENGINE_VERSION.to_owned(),
        policy_hash,
        rows_processed: accumulator.rows_processed,
        issue_count: u64::try_from(accumulator.issue_count).unwrap_or(u64::MAX),
        severity_counts: accumulator.severity_counts.clone(),
        rule_counts: accumulator.rule_counts.clone(),
        entity_counts: accumulator.entity_counts.clone(),
        field_counts: accumulator.field_counts.clone(),
        source_file_counts: accumulator.source_file_counts.clone(),
        issue_hashes,
    };
    let result_value = serde_json::to_value(&result)?;
    let result_json = canonical_json_string(&result_value)?;
    let result_hash = canonical_hash(&result_value)?;
    complete_run(
        writer,
        run_id,
        &result,
        &result_json,
        &result_hash,
        &accumulator.pending_issues,
    )?;
    Ok(ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        identity_hash,
        result_hash,
        result,
    })
}

fn validation_identity_hash(
    source_hash: &str,
    profile_hash: &str,
    mapping_hash: &str,
    rule_set_hash: &str,
    policy_hash: &str,
    source_reuse_hash: Option<&str>,
) -> Result<String> {
    let mut identity = json!({
        "contract": VALIDATION_ENGINE_VERSION,
        "sourceHash": source_hash,
        "profileHash": profile_hash,
        "mappingHash": mapping_hash,
        "ruleSetHash": rule_set_hash,
        "policyHash": policy_hash,
    });
    if let Some(source_reuse_hash) = source_reuse_hash {
        identity
            .as_object_mut()
            .expect("validation identity is an object")
            .insert(
                "sourceReuseHash".to_owned(),
                Value::String(source_reuse_hash.to_owned()),
            );
    }
    canonical_hash(&identity)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn load_idempotent_reused_validation(
    connection: &Connection,
    run_id: &str,
    identity_hash: &str,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rule_set_hash: &str,
    policy_hash: &str,
    source_reuse_id: &str,
    source_reuse_hash: &str,
) -> Result<Option<ValidationReport>> {
    let row = connection
        .query_row(
            "SELECT identity_hash, job_id, source_hash,
                    profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    rule_set_hash, engine_version, policy_hash,
                    source_reuse_id, source_reuse_hash,
                    state, result_json, result_hash
             FROM validation_runs WHERE id = ?1",
            [run_id],
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
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let exact = row.0 == identity_hash
        && row.1 == job_id
        && row.2 == source_hash
        && row.3 == mapping.profile.profile_id
        && row.4 == mapping.profile.version
        && row.5 == mapping.profile.sha256
        && row.6 == mapping.mapping_id
        && row.7 == mapping.version
        && row.8 == mapping_hash
        && row.9 == rule_set_hash
        && row.10 == VALIDATION_ENGINE_VERSION
        && row.11 == policy_hash
        && row.12.as_deref() == Some(source_reuse_id)
        && row.13.as_deref() == Some(source_reuse_hash);
    if !exact {
        return Err(KernelError::Conflict(format!(
            "preallocated validation run {run_id} already names different immutable evidence"
        )));
    }
    if row.14 != ValidationRunState::Complete.as_str() {
        return Err(KernelError::Conflict(format!(
            "preallocated validation run {run_id} is already terminal or in progress as {}; only an exact COMPLETE retry is idempotent",
            row.14
        )));
    }
    verify_validation_run_integrity(connection, run_id)?;
    let result_json = row.15.ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "complete validation run {run_id} has no result JSON"
        ))
    })?;
    let result_hash = row.16.ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "complete validation run {run_id} has no result hash"
        ))
    })?;
    let value = parse_unique_json(result_json.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "complete validation run {run_id} result JSON is invalid: {error}"
        ))
    })?;
    if canonical_json_string(&value)? != result_json || canonical_hash(&value)? != result_hash {
        return Err(KernelError::IntegrityFailed(format!(
            "complete validation run {run_id} result bytes/hash do not replay"
        )));
    }
    let result: ValidationResult = serde_json::from_value(value)?;
    Ok(Some(ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        identity_hash: identity_hash.to_owned(),
        result_hash,
        result,
    }))
}

#[derive(Debug)]
struct MappingSourceReuseRow {
    source_reuse_id: String,
    engagement_id: String,
    tour_attempt_id: String,
    predecessor_tour_id: String,
    preflight_id: String,
    acceptance_attempt_id: String,
    candidate_job_id: String,
    candidate_profiling_run_id: String,
    candidate_source_hash: String,
    candidate_manifest_hash: String,
    candidate_profiling_result_hash: String,
    baseline_job_id: String,
    mapping_source_hash: String,
    structure_json: String,
    baseline_structure_hash: String,
    candidate_structure_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    profile_approval_id: String,
    profile_approval_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    mapping_approval_id: Option<String>,
    mapping_approval_hash: Option<String>,
    mapping_authority_id: String,
    mapping_authority_hash: String,
    algorithm_version: String,
    canonical_reuse_json: String,
    source_reuse_hash: String,
    verified_at: String,
}

/// Independently replay one exact immutable source-reuse authority.
///
/// This verifier deliberately rebuilds both safe source structures from their
/// accepted CSV headers and deterministic mapping routes. Merely matching the
/// reuse row's candidate job/profile/mapping tuple is not sufficient.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn verify_mapping_source_reuse_authority(
    connection: &Connection,
    source_reuse_id: &str,
    source_reuse_hash: &str,
    job_id: &str,
    source_hash: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
) -> Result<()> {
    let row = connection
        .query_row(
            "SELECT source_reuse_id, engagement_id, tour_attempt_id,
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
                    mapping_authority_id, mapping_authority_hash,
                    algorithm_version, canonical_reuse_json,
                    source_reuse_hash, verified_at
             FROM engagement_mapping_reuse_authorities
             WHERE source_reuse_id = ?1 AND source_reuse_hash = ?2",
            params![source_reuse_id, source_reuse_hash],
            |row| {
                Ok(MappingSourceReuseRow {
                    source_reuse_id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    tour_attempt_id: row.get(2)?,
                    predecessor_tour_id: row.get(3)?,
                    preflight_id: row.get(4)?,
                    acceptance_attempt_id: row.get(5)?,
                    candidate_job_id: row.get(6)?,
                    candidate_profiling_run_id: row.get(7)?,
                    candidate_source_hash: row.get(8)?,
                    candidate_manifest_hash: row.get(9)?,
                    candidate_profiling_result_hash: row.get(10)?,
                    baseline_job_id: row.get(11)?,
                    mapping_source_hash: row.get(12)?,
                    structure_json: row.get(13)?,
                    baseline_structure_hash: row.get(14)?,
                    candidate_structure_hash: row.get(15)?,
                    profile_id: row.get(16)?,
                    profile_version: row.get(17)?,
                    profile_hash: row.get(18)?,
                    profile_approval_id: row.get(19)?,
                    profile_approval_hash: row.get(20)?,
                    mapping_id: row.get(21)?,
                    mapping_version: row.get(22)?,
                    mapping_hash: row.get(23)?,
                    mapping_approval_id: row.get(24)?,
                    mapping_approval_hash: row.get(25)?,
                    mapping_authority_id: row.get(26)?,
                    mapping_authority_hash: row.get(27)?,
                    algorithm_version: row.get(28)?,
                    canonical_reuse_json: row.get(29)?,
                    source_reuse_hash: row.get(30)?,
                    verified_at: row.get(31)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "mapping source-reuse authority {source_reuse_id}/{source_reuse_hash} is absent"
            ))
        })?;
    if row.source_reuse_id != source_reuse_id
        || row.source_reuse_hash != source_reuse_hash
        || row.candidate_job_id != job_id
        || row.candidate_source_hash != source_hash
        || row.profile_id != profile_id
        || row.profile_version != profile_version
        || row.profile_hash != profile_hash
        || row.mapping_id != mapping_id
        || row.mapping_version != mapping_version
        || row.mapping_hash != mapping_hash
        || row.algorithm_version != MAPPING_SOURCE_REUSE_ALGORITHM
        || !is_lowercase_sha256(&row.source_reuse_hash)
        || !is_lowercase_sha256(&row.candidate_source_hash)
        || !is_lowercase_sha256(&row.candidate_manifest_hash)
        || !is_lowercase_sha256(&row.candidate_profiling_result_hash)
        || !is_lowercase_sha256(&row.mapping_source_hash)
        || !is_lowercase_sha256(&row.baseline_structure_hash)
        || !is_lowercase_sha256(&row.candidate_structure_hash)
        || !is_lowercase_sha256(&row.profile_hash)
        || !is_lowercase_sha256(&row.profile_approval_hash)
        || !is_lowercase_sha256(&row.mapping_hash)
        || row.mapping_approval_id.is_some() != row.mapping_approval_hash.is_some()
        || row
            .mapping_approval_hash
            .as_deref()
            .is_some_and(|hash| !is_lowercase_sha256(hash))
        || !is_lowercase_sha256(&row.mapping_authority_hash)
        || chrono::DateTime::parse_from_rfc3339(&row.verified_at).is_err()
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "row identity, authority, bounds, or exact requested anchors differ",
        ));
    }
    if row.canonical_reuse_json.len() > MAXIMUM_SOURCE_REUSE_JSON_BYTES
        || row.structure_json.len() > MAXIMUM_SOURCE_STRUCTURE_JSON_BYTES
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "canonical reuse or structure evidence exceeds its absolute bound",
        ));
    }

    let parsed_reuse = parse_unique_json(row.canonical_reuse_json.as_bytes()).map_err(|error| {
        source_reuse_integrity(
            source_reuse_id,
            format!("canonical reuse JSON is invalid: {error}"),
        )
    })?;
    let stored_mapping_evidence = parsed_reuse
        .get("mapping")
        .and_then(Value::as_object)
        .ok_or_else(|| source_reuse_integrity(source_reuse_id, "mapping evidence is absent"))?;
    let mut mapping_evidence = json!({
        "mappingId": row.mapping_id,
        "mappingVersion": row.mapping_version,
        "mappingHash": row.mapping_hash,
    });
    if stored_mapping_evidence.contains_key("authorityId")
        || stored_mapping_evidence.contains_key("authorityHash")
    {
        let object = mapping_evidence.as_object_mut().ok_or_else(|| {
            source_reuse_integrity(source_reuse_id, "mapping evidence is not an object")
        })?;
        object.insert("authorityId".to_owned(), json!(row.mapping_authority_id));
        object.insert(
            "authorityHash".to_owned(),
            json!(row.mapping_authority_hash),
        );
    }
    if let (Some(approval_id), Some(approval_hash)) = (
        row.mapping_approval_id.as_ref(),
        row.mapping_approval_hash.as_ref(),
    ) {
        let object = mapping_evidence.as_object_mut().ok_or_else(|| {
            source_reuse_integrity(source_reuse_id, "mapping evidence is not an object")
        })?;
        object.insert("approvalId".to_owned(), json!(approval_id));
        object.insert("approvalHash".to_owned(), json!(approval_hash));
    }
    let mut baseline_evidence = json!({
        "jobId": row.baseline_job_id,
        "mappingSourceHash": row.mapping_source_hash,
        "structureHash": row.baseline_structure_hash,
    });
    let stored_baseline = parsed_reuse
        .get("baseline")
        .and_then(Value::as_object)
        .ok_or_else(|| source_reuse_integrity(source_reuse_id, "baseline evidence is absent"))?;
    if stored_baseline.contains_key("predecessorSourceHash") {
        let predecessor_source_hash: String = connection.query_row(
            "SELECT source_hash FROM engagement_tours
             WHERE tour_id = ?1 AND engagement_id = ?2 AND job_id = ?3",
            params![
                row.predecessor_tour_id,
                row.engagement_id,
                row.baseline_job_id,
            ],
            |row| row.get(0),
        )?;
        baseline_evidence
            .as_object_mut()
            .ok_or_else(|| {
                source_reuse_integrity(source_reuse_id, "baseline evidence is not an object")
            })?
            .insert(
                "predecessorSourceHash".to_owned(),
                json!(predecessor_source_hash),
            );
    }
    let expected_reuse = json!({
        "schemaVersion": MAPPING_SOURCE_REUSE_SCHEMA,
        "sourceReuseId": row.source_reuse_id,
        "engagementId": row.engagement_id,
        "tourAttemptId": row.tour_attempt_id,
        "predecessorTourId": row.predecessor_tour_id,
        "candidate": {
            "preflightId": row.preflight_id,
            "acceptanceAttemptId": row.acceptance_attempt_id,
            "jobId": row.candidate_job_id,
            "profilingRunId": row.candidate_profiling_run_id,
            "sourceHash": row.candidate_source_hash,
            "manifestHash": row.candidate_manifest_hash,
            "profilingResultHash": row.candidate_profiling_result_hash,
            "structureHash": row.candidate_structure_hash,
        },
        "baseline": baseline_evidence,
        "profile": {
            "profileId": row.profile_id,
            "profileVersion": row.profile_version,
            "profileHash": row.profile_hash,
            "approvalId": row.profile_approval_id,
            "approvalHash": row.profile_approval_hash,
        },
        "mapping": mapping_evidence,
        "verifiedAt": row.verified_at,
    });
    if parsed_reuse != expected_reuse
        || canonical_json_string(&expected_reuse)? != row.canonical_reuse_json
        || canonical_hash(&expected_reuse)? != row.source_reuse_hash
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "canonical reuse bytes/hash differ from independently rebuilt evidence",
        ));
    }

    let mapping = load_stored_mapping(connection, &row.mapping_id, &row.mapping_version)?;
    if mapping.mapping_hash != row.mapping_hash
        || mapping.document.profile.profile_id != row.profile_id
        || mapping.document.profile.version != row.profile_version
        || mapping.document.profile.sha256 != row.profile_hash
        || mapping.document.source.package_sha256 != row.mapping_source_hash
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "verified mapping bytes differ from reuse authority",
        ));
    }
    let mapping_registry_anchor: Option<(String, String)> = connection
        .query_row(
            "SELECT validated_job_id, source_hash FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2 AND mapping_hash = ?3",
            params![row.mapping_id, row.mapping_version, row.mapping_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let mapping_origin_ok =
        if let Some((validated_job_id, registry_source_hash)) = mapping_registry_anchor.as_ref() {
            registry_source_hash == &row.mapping_source_hash
                && connection.query_row(
                    "SELECT EXISTS(
                     SELECT 1 FROM jobs
                     WHERE id = ?1 AND state = 'COMPLETE' AND source_hash = ?2
                 )",
                    params![validated_job_id, registry_source_hash],
                    |record| record.get::<_, bool>(0),
                )?
        } else {
            false
        };
    if !mapping_origin_ok {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "mapping registry origin job/source differs from reuse authority",
        ));
    }

    let baseline_structure =
        rebuild_source_structure(connection, &row.baseline_job_id, &mapping.document)?;
    let candidate_structure =
        rebuild_source_structure(connection, &row.candidate_job_id, &mapping.document)?;
    let stored_structure = parse_unique_json(row.structure_json.as_bytes()).map_err(|error| {
        source_reuse_integrity(
            source_reuse_id,
            format!("stored source structure is invalid: {error}"),
        )
    })?;
    let baseline_value = parse_unique_json(baseline_structure.as_bytes())?;
    let candidate_value = parse_unique_json(candidate_structure.as_bytes())?;
    let structure_hash = canonical_hash(&stored_structure)?;
    if canonical_json_string(&stored_structure)? != row.structure_json
        || baseline_value != stored_structure
        || candidate_value != stored_structure
        || baseline_structure != row.structure_json
        || candidate_structure != row.structure_json
        || structure_hash != row.baseline_structure_hash
        || structure_hash != row.candidate_structure_hash
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "baseline/candidate file-header-entity structure does not replay exactly",
        ));
    }

    verify_source_reuse_links(connection, &row)?;
    verify_source_reuse_approvals(connection, &row)?;
    Ok(())
}

fn source_reuse_integrity(source_reuse_id: &str, detail: impl std::fmt::Display) -> KernelError {
    KernelError::IntegrityFailed(format!(
        "mapping source-reuse authority {source_reuse_id} replay failed: {detail}"
    ))
}

fn rebuild_source_structure(
    connection: &Connection,
    job_id: &str,
    mapping: &MappingDocument,
) -> Result<String> {
    let mut statement = connection.prepare(
        "SELECT path, header_json FROM csv_files
         WHERE job_id = ?1 ORDER BY path",
    )?;
    let rows = statement
        .query_map([job_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() || rows.len() > 1_000 {
        return Err(KernelError::IntegrityFailed(format!(
            "accepted job {job_id} has no bounded CSV structure"
        )));
    }
    let mut files = Vec::with_capacity(rows.len());
    for (path, header_json) in rows {
        let header_json = header_json.ok_or_else(|| {
            KernelError::IntegrityFailed(format!(
                "accepted job {job_id} CSV {path:?} has no header evidence"
            ))
        })?;
        if header_json.len() > MAXIMUM_SOURCE_STRUCTURE_JSON_BYTES {
            return Err(KernelError::IntegrityFailed(format!(
                "accepted job {job_id} CSV {path:?} header evidence exceeds its bound"
            )));
        }
        let headers: Vec<String> = serde_json::from_value(
            parse_unique_json(header_json.as_bytes()).map_err(|error| {
                KernelError::IntegrityFailed(format!(
                    "accepted job {job_id} CSV {path:?} header JSON is invalid: {error}"
                ))
            })?,
        )?;
        if headers.is_empty()
            || headers.len() > 10_000
            || headers
                .iter()
                .any(|header| header.is_empty() || header.len() > 64 * 1024)
        {
            return Err(KernelError::IntegrityFailed(format!(
                "accepted job {job_id} CSV {path:?} headers are outside bounds"
            )));
        }
        let entity_id = mapping.entity_for_source_file(&path)?;
        files.push(json!({
            "path": path,
            "entityId": entity_id,
            "headers": headers,
        }));
    }
    let value = json!({
        "schemaVersion": "migration-stream/source-structure/v1",
        "files": files,
    });
    let canonical = canonical_json_string(&value)?;
    if canonical.len() > MAXIMUM_SOURCE_STRUCTURE_JSON_BYTES {
        return Err(KernelError::IntegrityFailed(format!(
            "accepted job {job_id} source structure exceeds its bound"
        )));
    }
    Ok(canonical)
}

#[allow(clippy::too_many_lines)]
fn verify_source_reuse_links(connection: &Connection, row: &MappingSourceReuseRow) -> Result<()> {
    let source_reuse_id = &row.source_reuse_id;
    let attempt_ok: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM engagement_tour_attempts
             WHERE tour_attempt_id = ?1 AND engagement_id = ?2
               AND predecessor_tour_id = ?3 AND tour_number > 1
         )",
        params![
            row.tour_attempt_id,
            row.engagement_id,
            row.predecessor_tour_id
        ],
        |row| row.get(0),
    )?;
    let predecessor_ok: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM engagement_tours AS tour
             JOIN engagement_mapping_authorities AS authority
               ON authority.authority_id = tour.mapping_authority_id
              AND authority.authority_hash = tour.mapping_authority_hash
              AND authority.engagement_id = tour.engagement_id
              AND authority.job_id = tour.job_id
              AND authority.source_hash = tour.source_hash
              AND authority.mapping_id = tour.mapping_id
              AND authority.mapping_version = tour.mapping_version
              AND authority.mapping_hash = tour.mapping_hash
             WHERE tour.tour_id = ?1 AND tour.engagement_id = ?2 AND tour.job_id = ?3
               AND tour.profile_id = ?4 AND tour.profile_version = ?5
               AND tour.profile_hash = ?6
               AND tour.profile_approval_id = ?7 AND tour.profile_approval_hash = ?8
               AND tour.mapping_id = ?9 AND tour.mapping_version = ?10
               AND tour.mapping_hash = ?11
         )",
        params![
            row.predecessor_tour_id,
            row.engagement_id,
            row.baseline_job_id,
            row.profile_id,
            row.profile_version,
            row.profile_hash,
            row.profile_approval_id,
            row.profile_approval_hash,
            row.mapping_id,
            row.mapping_version,
            row.mapping_hash,
        ],
        |row| row.get(0),
    )?;
    let preflight_manifest: Option<String> = connection
        .query_row(
            "SELECT manifest_json FROM engagement_source_preflights
             WHERE preflight_id = ?1 AND engagement_id = ?2 AND state = 'READY'
               AND source_hash = ?3 AND manifest_hash = ?4",
            params![
                row.preflight_id,
                row.engagement_id,
                row.candidate_source_hash,
                row.candidate_manifest_hash
            ],
            |row| row.get(0),
        )
        .optional()?;
    let preflight_ok = if let Some(manifest_json) = preflight_manifest {
        manifest_json.len() <= MAXIMUM_SOURCE_REUSE_JSON_BYTES
            && canonical_hash(
                &parse_unique_json(manifest_json.as_bytes()).map_err(|error| {
                    source_reuse_integrity(
                        source_reuse_id,
                        format!("candidate manifest JSON is invalid: {error}"),
                    )
                })?,
            )? == row.candidate_manifest_hash
    } else {
        false
    };
    let acceptance_ok: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM engagement_acceptance_attempts
             WHERE attempt_id = ?1 AND engagement_id = ?2 AND preflight_id = ?3
               AND accepted_source_hash = ?4 AND accepted_manifest_hash = ?5
               AND job_id = ?6 AND profiling_run_id = ?7
         )",
        params![
            row.acceptance_attempt_id,
            row.engagement_id,
            row.preflight_id,
            row.candidate_source_hash,
            row.candidate_manifest_hash,
            row.candidate_job_id,
            row.candidate_profiling_run_id
        ],
        |row| row.get(0),
    )?;
    let job_ok: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM jobs
             WHERE id = ?1 AND state = 'COMPLETE' AND source_hash = ?2
         )",
        params![row.candidate_job_id, row.candidate_source_hash],
        |row| row.get(0),
    )?;
    let profiling: Option<(String, String)> = connection
        .query_row(
            "SELECT result_json, result_hash FROM profiling_runs
             WHERE id = ?1 AND job_id = ?2 AND source_hash = ?3
               AND state = 'COMPLETE' AND result_hash = ?4",
            params![
                row.candidate_profiling_run_id,
                row.candidate_job_id,
                row.candidate_source_hash,
                row.candidate_profiling_result_hash
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let profiling_ok = if let Some((result_json, result_hash)) = profiling {
        if result_json.len() > MAXIMUM_RESULT_BYTES {
            false
        } else {
            let result = parse_unique_json(result_json.as_bytes()).map_err(|error| {
                source_reuse_integrity(
                    source_reuse_id,
                    format!("profiling result JSON is invalid: {error}"),
                )
            })?;
            let typed: crate::profiling::ProfilingResult = serde_json::from_value(result.clone())
                .map_err(|error| {
                source_reuse_integrity(
                    source_reuse_id,
                    format!("profiling result contract is invalid: {error}"),
                )
            })?;
            canonical_json_string(&result)? == result_json
                && crate::profiling::canonical_result_hash_value(&result)? == result_hash
                && typed.job_id == row.candidate_job_id
                && typed.source_hash == row.candidate_source_hash
                && typed
                    .files
                    .iter()
                    .all(|file| file.columns.iter().all(|column| column.samples.is_empty()))
        }
    } else {
        false
    };
    let retained_sample_columns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM profiling_columns
         WHERE run_id = ?1 AND samples_json <> '[]'",
        [&row.candidate_profiling_run_id],
        |row| row.get(0),
    )?;
    if !attempt_ok
        || !predecessor_ok
        || !preflight_ok
        || !acceptance_ok
        || !job_ok
        || !profiling_ok
        || retained_sample_columns != 0
    {
        return Err(source_reuse_integrity(
            source_reuse_id,
            "tour/source/preflight/acceptance/job/profiling linkage is absent, altered, or retains samples",
        ));
    }
    Ok(())
}

fn verify_source_reuse_approvals(
    connection: &Connection,
    row: &MappingSourceReuseRow,
) -> Result<()> {
    let mut profile_issues = Vec::new();
    crate::profile_registry::verify_integrity(connection, &mut profile_issues)?;
    if !profile_issues.is_empty() {
        return Err(source_reuse_integrity(
            &row.source_reuse_id,
            format!(
                "profile registry authority replay failed: {}",
                profile_issues.join("; ")
            ),
        ));
    }
    load_verified_profile_document(
        connection,
        &row.profile_id,
        &row.profile_version,
        &row.profile_hash,
    )?;
    let profile_approval_ok: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM profile_approvals
             WHERE approval_id = ?1 AND profile_id = ?2
               AND profile_version = ?3 AND profile_hash = ?4
               AND approval_hash = ?5 AND acknowledged = 1
         )",
        params![
            row.profile_approval_id,
            row.profile_id,
            row.profile_version,
            row.profile_hash,
            row.profile_approval_hash
        ],
        |row| row.get(0),
    )?;
    let mut mapping_issues = Vec::new();
    crate::mapping::verify_integrity(connection, &mut mapping_issues)?;
    crate::engagement::verify_mapping_authority_integrity(connection, &mut mapping_issues)?;
    if !mapping_issues.is_empty() {
        return Err(source_reuse_integrity(
            &row.source_reuse_id,
            format!(
                "mapping/operator approval replay failed: {}",
                mapping_issues.join("; ")
            ),
        ));
    }
    let mapping_authority = crate::engagement::verify_exact_mapping_authority(
        connection,
        &row.mapping_authority_id,
        &row.mapping_authority_hash,
        &row.engagement_id,
        &row.candidate_job_id,
        &row.candidate_source_hash,
        &row.mapping_id,
        &row.mapping_version,
        &row.mapping_hash,
    )?;
    let mapping_authority_ok = match mapping_authority.authority_kind.as_str() {
        "OPERATOR_APPROVAL" => {
            mapping_authority.mapping_approval_id == row.mapping_approval_id
                && mapping_authority.mapping_approval_hash == row.mapping_approval_hash
                && row.mapping_approval_id.is_some()
                && row.mapping_approval_hash.is_some()
        }
        "CONTROLLED_PREPARATION" => {
            mapping_authority.mapping_approval_id.is_none()
                && mapping_authority.mapping_approval_hash.is_none()
                && row.mapping_approval_id.is_none()
                && row.mapping_approval_hash.is_none()
        }
        _ => false,
    };
    if !profile_approval_ok || !mapping_authority_ok {
        return Err(source_reuse_integrity(
            &row.source_reuse_id,
            "exact independently replayed profile approval or normalized mapping authority is absent",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_run(
    writer: &WorkspaceWriter,
    run_id: &str,
    identity_hash: &str,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rule_set_hash: &str,
    policy_json: &str,
    policy_hash: &str,
    reused_source: Option<ReusedSourceValidation<'_>>,
) -> Result<()> {
    let now = now_rfc3339();
    let transaction = writer.connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO validation_runs (
             id, identity_hash, job_id, source_hash,
             profile_id, profile_version, profile_hash,
             mapping_id, mapping_version, mapping_hash,
             rule_set_hash, engine_version, policy_json, policy_hash,
             state, created_at, updated_at, source_reuse_id, source_reuse_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, 'RUNNING', ?15, ?15, ?16, ?17)",
        params![
            run_id,
            identity_hash,
            job_id,
            source_hash,
            mapping.profile.profile_id,
            mapping.profile.version,
            mapping.profile.sha256,
            mapping.mapping_id,
            mapping.version,
            mapping_hash,
            rule_set_hash,
            VALIDATION_ENGINE_VERSION,
            policy_json,
            policy_hash,
            now,
            reused_source.map(|reuse| reuse.source_reuse_id),
            reused_source.map(|reuse| reuse.source_reuse_hash),
        ],
    )?;
    insert_event(
        &transaction,
        run_id,
        ValidationRunState::Running,
        None,
        None,
        &now,
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_rules(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rules: &[CompiledRule],
    relationships: &[CompiledRelationship],
    policy: &ValidationPolicy,
    control: &ValidationControl,
) -> Result<ValidationAccumulator> {
    let mut accumulator = ValidationAccumulator::for_mapping(mapping);
    let mut relationship_keys = vec![RelationshipKeyState::default(); relationships.len()];
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;
    let mut batches = 0_u64;
    loop {
        if should_cancel(control, batches) {
            return Err(KernelError::Cancelled(
                "validation cancelled at a committed batch boundary".to_owned(),
            ));
        }
        let rows = project_rows_batch(
            &writer.connection,
            job_id,
            mapping,
            &after_file,
            after_ordinal,
            policy.row_batch_size,
        )?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            accumulator.rows_processed =
                accumulator.rows_processed.checked_add(1).ok_or_else(|| {
                    KernelError::ValidationBound("processed-row count overflow".to_owned())
                })?;
            for rule in rules {
                if let Some(code) = evaluate_rule(rule, row, mapping, &mut accumulator, policy)? {
                    let issue = make_issue(
                        source_hash,
                        &mapping.profile.sha256,
                        mapping_hash,
                        rule,
                        row,
                        code,
                    )?;
                    accumulator.push_issue(issue, policy.maximum_issues)?;
                    if accumulator.pending_issues.len() >= MAXIMUM_PENDING_ISSUES
                        || accumulator.issue_count == policy.maximum_issues
                    {
                        persist_pending(writer, run_id, &mut accumulator)?;
                    }
                }
            }
            index_relationship_keys(
                row,
                relationships,
                &mut relationship_keys,
                &mut accumulator,
                policy,
            )?;
        }
        persist_pending(writer, run_id, &mut accumulator)?;
        let last = rows.last().expect("nonempty projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
        batches = batches
            .checked_add(1)
            .ok_or_else(|| KernelError::ValidationBound("batch count overflow".to_owned()))?;
    }
    execute_relationship_issues(
        writer,
        job_id,
        source_hash,
        mapping,
        mapping_hash,
        relationships,
        &relationship_keys,
        accumulator,
        policy,
        control,
        &mut batches,
    )
}

fn index_relationship_keys(
    row: &ProjectedRow,
    relationships: &[CompiledRelationship],
    states: &mut [RelationshipKeyState],
    accumulator: &mut ValidationAccumulator,
    policy: &ValidationPolicy,
) -> Result<()> {
    for (relationship, state) in relationships.iter().zip(states) {
        if row.entity_id == relationship.from_entity_id
            && let Some(key) = relationship_key(row, relationship, true)?
            && !state.parent_keys.contains(&key)
        {
            accumulator.reserve_key_state(key.len(), policy)?;
            state.parent_keys.insert(key);
        }
        if row.entity_id == relationship.to_entity_id
            && let Some(key) = relationship_key(row, relationship, false)?
            && !state.child_keys.contains(&key)
        {
            accumulator.reserve_key_state(key.len(), policy)?;
            state.child_keys.insert(key);
        }
    }
    Ok(())
}

fn relationship_key(
    row: &ProjectedRow,
    relationship: &CompiledRelationship,
    parent: bool,
) -> Result<Option<String>> {
    let mut values = Vec::with_capacity(relationship.key_pairs.len());
    for (from_field, to_field) in &relationship.key_pairs {
        let field = if parent { from_field } else { to_field };
        let Some(value) = row
            .canonical_values
            .get(field)
            .and_then(Option::as_deref)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        values.push(Value::String(value.to_owned()));
    }
    canonical_hash(&Value::Array(values)).map(Some)
}

#[allow(clippy::too_many_arguments)]
fn execute_relationship_issues(
    writer: &WorkspaceWriter,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    relationships: &[CompiledRelationship],
    states: &[RelationshipKeyState],
    mut accumulator: ValidationAccumulator,
    policy: &ValidationPolicy,
    control: &ValidationControl,
    batches: &mut u64,
) -> Result<ValidationAccumulator> {
    if relationships.is_empty() {
        return Ok(accumulator);
    }
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;
    loop {
        if should_cancel(control, *batches) {
            return Err(KernelError::Cancelled(
                "validation cancelled at a committed batch boundary".to_owned(),
            ));
        }
        let rows = project_rows_batch(
            &writer.connection,
            job_id,
            mapping,
            &after_file,
            after_ordinal,
            policy.row_batch_size,
        )?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            evaluate_relationship_issues_for_row(
                source_hash,
                mapping_hash,
                mapping,
                row,
                relationships,
                states,
                &mut accumulator,
                policy,
            )?;
        }
        let last = rows.last().expect("nonempty relationship projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
        *batches = batches
            .checked_add(1)
            .ok_or_else(|| KernelError::ValidationBound("batch count overflow".to_owned()))?;
    }
    Ok(accumulator)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_relationship_issues_for_row(
    source_hash: &str,
    mapping_hash: &str,
    mapping: &MappingDocument,
    row: &ProjectedRow,
    relationships: &[CompiledRelationship],
    states: &[RelationshipKeyState],
    accumulator: &mut ValidationAccumulator,
    policy: &ValidationPolicy,
) -> Result<()> {
    for (relationship, state) in relationships.iter().zip(states) {
        if row.entity_id == relationship.from_entity_id {
            match relationship_key(row, relationship, true)? {
                None => accumulator.push_issue(
                    make_relationship_issue(
                        source_hash,
                        &mapping.profile.sha256,
                        mapping_hash,
                        relationship,
                        row,
                        true,
                        "RELATIONSHIP_KEY_MISSING",
                        "parent-key",
                        RELATIONSHIP_PARENT_KEY_MESSAGE,
                    )?,
                    policy.maximum_issues,
                )?,
                Some(key)
                    if relationship.cardinality == "1..*" && !state.child_keys.contains(&key) =>
                {
                    accumulator.push_issue(
                        make_relationship_issue(
                            source_hash,
                            &mapping.profile.sha256,
                            mapping_hash,
                            relationship,
                            row,
                            true,
                            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
                            "minimum-child",
                            RELATIONSHIP_MINIMUM_CHILD_MESSAGE,
                        )?,
                        policy.maximum_issues,
                    )?;
                }
                Some(_) => {}
            }
        }
        if row.entity_id == relationship.to_entity_id {
            match relationship_key(row, relationship, false)? {
                None => accumulator.push_issue(
                    make_relationship_issue(
                        source_hash,
                        &mapping.profile.sha256,
                        mapping_hash,
                        relationship,
                        row,
                        false,
                        "RELATIONSHIP_KEY_MISSING",
                        "child-key",
                        RELATIONSHIP_CHILD_KEY_MESSAGE,
                    )?,
                    policy.maximum_issues,
                )?,
                Some(key) if !state.parent_keys.contains(&key) => {
                    accumulator.push_issue(
                        make_relationship_issue(
                            source_hash,
                            &mapping.profile.sha256,
                            mapping_hash,
                            relationship,
                            row,
                            false,
                            "RELATIONSHIP_PARENT_NOT_FOUND",
                            "parent-exists",
                            RELATIONSHIP_PARENT_NOT_FOUND_MESSAGE,
                        )?,
                        policy.maximum_issues,
                    )?;
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

fn relationship_anchor_field(relationship: &CompiledRelationship, parent: bool) -> Result<&str> {
    relationship
        .key_pairs
        .first()
        .map(|pair| {
            if parent {
                pair.0.as_str()
            } else {
                pair.1.as_str()
            }
        })
        .ok_or_else(|| {
            KernelError::ValidationInvalid("compiled relationship has no key field".to_owned())
        })
}

fn relationship_lineage_identity(
    relationship: &CompiledRelationship,
    parent: bool,
) -> RelationshipLineageIdentity {
    RelationshipLineageIdentity {
        relationship_id: relationship.relationship_id.clone(),
        relationship_role: if parent { "parent" } else { "child" }.to_owned(),
        related_entity_id: if parent {
            relationship.to_entity_id.clone()
        } else {
            relationship.from_entity_id.clone()
        },
        related_field_ids: relationship
            .key_pairs
            .iter()
            .map(|pair| {
                if parent {
                    pair.1.clone()
                } else {
                    pair.0.clone()
                }
            })
            .collect(),
    }
}

fn augment_relationship_lineage(
    lineage: &mut Value,
    identity: &RelationshipLineageIdentity,
) -> Result<()> {
    let evidence = lineage.as_object_mut().ok_or_else(|| {
        KernelError::IntegrityFailed("relationship key lineage is not an object".to_owned())
    })?;
    evidence.insert(
        "relationshipId".to_owned(),
        Value::String(identity.relationship_id.clone()),
    );
    evidence.insert(
        "relationshipRole".to_owned(),
        Value::String(identity.relationship_role.clone()),
    );
    evidence.insert(
        "relatedEntityId".to_owned(),
        Value::String(identity.related_entity_id.clone()),
    );
    evidence.insert(
        "relatedFieldIds".to_owned(),
        Value::Array(
            identity
                .related_field_ids
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    evidence.insert("sourceValueIncluded".to_owned(), Value::Bool(false));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn make_relationship_issue(
    source_hash: &str,
    profile_hash: &str,
    mapping_hash: &str,
    relationship: &CompiledRelationship,
    row: &ProjectedRow,
    parent: bool,
    issue_code: &str,
    rule_suffix: &str,
    display_message: &str,
) -> Result<IssueEvidence> {
    let field_id = relationship_anchor_field(relationship, parent)?;
    let mut lineage = row.lineage.get(field_id).cloned().ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "relationship key field {:?}.{:?} has no mapping lineage",
            row.entity_id, field_id
        ))
    })?;
    augment_relationship_lineage(
        &mut lineage,
        &relationship_lineage_identity(relationship, parent),
    )?;
    let rule_id = format!(
        "relationship.{}.{}",
        relationship.relationship_id, rule_suffix
    );
    let material = issue_material(
        source_hash,
        profile_hash,
        mapping_hash,
        "ERROR",
        &rule_id,
        issue_code,
        display_message,
        &row.entity_id,
        field_id,
        &row.source_file,
        row.source_ordinal,
        row.line_start,
        row.line_end,
        &lineage,
    );
    let issue_hash = canonical_hash(&material)?;
    Ok(IssueEvidence {
        issue_id: format!("issue-{issue_hash}"),
        issue_hash,
        severity: "ERROR".to_owned(),
        rule_id,
        issue_code: issue_code.to_owned(),
        display_message: display_message.to_owned(),
        entity_id: row.entity_id.clone(),
        field_id: field_id.to_owned(),
        source_file: row.source_file.clone(),
        source_ordinal: row.source_ordinal,
        line_start: row.line_start,
        line_end: row.line_end,
        lineage_json: canonical_json_string(&lineage)?,
    })
}

/// Evaluate the exact inherited validation policy after applying one bounded
/// field overlay to one projected row in memory.
///
/// The implementation deliberately evaluates the complete inherited mapping
/// scope. That conservative scope is bounded by the accepted validation policy
/// and is safe for uniqueness, conditional, and reference dependencies; it
/// never silently narrows a dependency. No source, projection, validation run,
/// or issue row is written by this function.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn evaluate_effective_overlay(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rules_value: &Value,
    policy: &ValidationPolicy,
    overlay: &EffectiveOverlayInput,
    control: &ValidationControl,
) -> Result<EffectiveValidationEvaluation> {
    evaluate_effective_overlay_before_commit(
        connection,
        job_id,
        validation_run_id,
        source_hash,
        mapping,
        mapping_hash,
        rules_value,
        policy,
        overlay,
        control,
        None,
    )
}

/// Replay a correction against exactly the effectivations that preceded one
/// immutable decision commit sequence. `None` evaluates a prospective new
/// command against every currently effective overlay.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn evaluate_effective_overlay_before_commit(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rules_value: &Value,
    policy: &ValidationPolicy,
    overlay: &EffectiveOverlayInput,
    control: &ValidationControl,
    before_commit_sequence: Option<u64>,
) -> Result<EffectiveValidationEvaluation> {
    policy.validate()?;
    if mapped_entity_ids(mapping).len() > 1 {
        return Err(KernelError::DecisionInvalid(
            "governed correction/re-evaluation is unavailable for multi-entity mappings".to_owned(),
        ));
    }
    validate_lookup_bounds(mapping, policy)?;
    let rules = compile_rules(rules_value, mapping, policy)?;
    if overlay.entity_id != mapping.root_entity_id {
        return Err(KernelError::DecisionInvalid(
            "correction overlay targets a non-root canonical entity".to_owned(),
        ));
    }
    let mut active_effective_chains = load_active_effective_chains(
        connection,
        job_id,
        validation_run_id,
        mapping,
        mapping_hash,
        before_commit_sequence,
    )?;

    let mut accumulator = ValidationAccumulator::for_mapping(mapping);
    let mut retained_issues = Vec::new();
    let mut retained_issue_bytes = 0_usize;
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;
    let mut batches = 0_u64;
    let mut matched_rows = 0_u8;

    loop {
        if should_cancel(control, batches) {
            return Err(KernelError::Cancelled(
                "re-evaluation cancelled at a completed batch boundary".to_owned(),
            ));
        }
        let rows = project_rows_batch(
            connection,
            job_id,
            mapping,
            &after_file,
            after_ordinal,
            policy.row_batch_size,
        )?;
        if rows.is_empty() {
            break;
        }

        for mut row in rows.iter().cloned() {
            apply_active_effective_values(
                &mut row,
                job_id,
                validation_run_id,
                mapping,
                mapping_hash,
                &mut active_effective_chains,
            )?;
            if row.source_file == overlay.source_file
                && row.source_ordinal == overlay.source_ordinal
            {
                matched_rows = matched_rows.saturating_add(1);
                if matched_rows != 1 || !row.canonical_values.contains_key(&overlay.field_id) {
                    return Err(KernelError::DecisionIntegrity(
                        "correction overlay does not bind one exact projected field".to_owned(),
                    ));
                }
                row.canonical_values
                    .insert(overlay.field_id.clone(), overlay.value.clone());
            }

            accumulator.rows_processed =
                accumulator.rows_processed.checked_add(1).ok_or_else(|| {
                    KernelError::DecisionBound("re-evaluated row count overflow".to_owned())
                })?;
            for rule in &rules {
                if let Some(code) = evaluate_rule(rule, &row, mapping, &mut accumulator, policy)? {
                    let issue = make_issue(
                        source_hash,
                        &mapping.profile.sha256,
                        mapping_hash,
                        rule,
                        &row,
                        code,
                    )?;
                    retained_issue_bytes = retained_issue_bytes
                        .checked_add(issue.lineage_json.len())
                        .and_then(|value| value.checked_add(issue.display_message.len()))
                        .and_then(|value| value.checked_add(issue.issue_hash.len()))
                        .ok_or_else(|| {
                            KernelError::DecisionBound(
                                "re-evaluation retained issue bytes overflow".to_owned(),
                            )
                        })?;
                    if retained_issue_bytes > MAXIMUM_ISSUE_REPLAY_BYTES {
                        return Err(KernelError::DecisionBound(format!(
                            "re-evaluation issue evidence exceeds {MAXIMUM_ISSUE_REPLAY_BYTES} bytes"
                        )));
                    }
                    accumulator.push_issue(issue, policy.maximum_issues)?;
                }
            }
            retained_issues.append(&mut accumulator.pending_issues);
        }

        let last = rows.last().expect("nonempty effective projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
        batches = batches.checked_add(1).ok_or_else(|| {
            KernelError::DecisionBound("re-evaluation batch count overflow".to_owned())
        })?;
    }

    if matched_rows != 1 {
        return Err(KernelError::DecisionIntegrity(
            "correction overlay did not bind one accepted projected row".to_owned(),
        ));
    }
    if !active_effective_chains.is_empty() {
        return Err(KernelError::DecisionIntegrity(
            "an effective correction chain does not bind an exact projected field".to_owned(),
        ));
    }
    Ok(EffectiveValidationEvaluation {
        rows_processed: accumulator.rows_processed,
        issues: retained_issues,
        scope_kind: "FULL_INHERITED_VALIDATION_SCOPE",
    })
}

type EffectiveChainKey = (String, u64, String, String);

#[derive(Clone, Debug)]
struct EffectiveChainEdge {
    original_value_hash: String,
    effective_value_hash: String,
    value: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn load_active_effective_chains(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    before_commit_sequence: Option<u64>,
) -> Result<HashMap<EffectiveChainKey, HashMap<String, EffectiveChainEdge>>> {
    let mut statement = connection.prepare(
        "SELECT a.accepted_source_file, a.source_ordinal,
                a.canonical_entity_id, a.canonical_field_id,
                ce.original_value_hash, ce.prior_effective_value_hash,
                ce.effective_value_hash, ce.effective_value_text
         FROM correction_effectivations ce
         JOIN issue_decisions d ON d.decision_id = ce.decision_id
         JOIN decision_attempts a ON a.attempt_id = d.decision_attempt_id
         WHERE a.job_id = ?1 AND a.validation_run_id = ?2
           AND a.profile_id = ?3 AND a.profile_version = ?4
           AND a.profile_hash = ?5 AND a.mapping_id = ?6
           AND a.mapping_version = ?7 AND a.mapping_hash = ?8
           AND (?9 IS NULL OR d.commit_sequence < ?9)
         ORDER BY ce.effectivation_id
         LIMIT 1000001",
    )?;
    let rows = statement.query_map(
        params![
            job_id,
            validation_run_id,
            mapping.profile.profile_id,
            mapping.profile.version,
            mapping.profile.sha256,
            mapping.mapping_id,
            mapping.version,
            mapping_hash,
            before_commit_sequence
                .map(i64::try_from)
                .transpose()
                .map_err(|_| KernelError::DecisionBound(
                    "decision commit sequence exceeds SQLite integer range".to_owned(),
                ))?,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        },
    )?;
    let mut count = 0_usize;
    let mut retained_bytes = 0_usize;
    let mut chains = HashMap::<EffectiveChainKey, HashMap<String, EffectiveChainEdge>>::new();
    for row in rows {
        let (source_file, ordinal, entity_id, field_id, original, prior, effective, value) = row?;
        count = count.checked_add(1).ok_or_else(|| {
            KernelError::DecisionBound("effective correction count overflow".to_owned())
        })?;
        if count > 1_000_000 {
            return Err(KernelError::DecisionBound(
                "effective correction replay exceeds 1000000 rows".to_owned(),
            ));
        }
        let row_bytes = source_file
            .len()
            .checked_add(entity_id.len())
            .and_then(|bytes| bytes.checked_add(field_id.len()))
            .and_then(|bytes| bytes.checked_add(original.len()))
            .and_then(|bytes| bytes.checked_add(prior.len()))
            .and_then(|bytes| bytes.checked_add(effective.len()))
            .and_then(|bytes| bytes.checked_add(value.as_deref().map_or(0, str::len)))
            .ok_or_else(|| {
                KernelError::DecisionBound(
                    "effective correction replay byte count overflowed".to_owned(),
                )
            })?;
        retained_bytes = retained_bytes.checked_add(row_bytes).ok_or_else(|| {
            KernelError::DecisionBound(
                "effective correction replay byte count overflowed".to_owned(),
            )
        })?;
        if retained_bytes > MAXIMUM_EFFECTIVE_CHAIN_REPLAY_BYTES {
            return Err(KernelError::DecisionBound(format!(
                "effective correction replay exceeds {MAXIMUM_EFFECTIVE_CHAIN_REPLAY_BYTES} bytes"
            )));
        }
        let key = (
            source_file,
            nonnegative(ordinal, "effective correction source ordinal")?,
            entity_id,
            field_id,
        );
        let prior_edges = chains.entry(key).or_default();
        if prior_edges
            .insert(
                prior,
                EffectiveChainEdge {
                    original_value_hash: original,
                    effective_value_hash: effective,
                    value,
                },
            )
            .is_some()
        {
            return Err(KernelError::DecisionIntegrity(
                "effective correction history contains a field-level fork".to_owned(),
            ));
        }
    }
    Ok(chains)
}

fn apply_active_effective_values(
    row: &mut ProjectedRow,
    job_id: &str,
    validation_run_id: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    chains: &mut HashMap<EffectiveChainKey, HashMap<String, EffectiveChainEdge>>,
) -> Result<()> {
    let field_ids = row.canonical_values.keys().cloned().collect::<Vec<_>>();
    for field_id in field_ids {
        let key = (
            row.source_file.clone(),
            row.source_ordinal,
            row.entity_id.clone(),
            field_id.clone(),
        );
        let Some(mut chain) = chains.remove(&key) else {
            continue;
        };
        let original_value = row
            .canonical_values
            .get(&field_id)
            .cloned()
            .ok_or_else(|| {
                KernelError::DecisionIntegrity(
                    "effective correction field disappeared from projection".to_owned(),
                )
            })?;
        let original_lineage = row.lineage.get(&field_id).ok_or_else(|| {
            KernelError::DecisionIntegrity(
                "effective correction field has no original lineage".to_owned(),
            )
        })?;
        let scope = DecisionScope {
            job_id: job_id.to_owned(),
            validation_run_id: validation_run_id.to_owned(),
            issue_id: "effective-field-replay".to_owned(),
            issue_hash: "0".repeat(64),
            accepted_source_file: row.source_file.clone(),
            source_ordinal: row.source_ordinal,
            line_start: row.line_start,
            line_end: row.line_end,
            canonical_entity_id: row.entity_id.clone(),
            canonical_field_id: field_id.clone(),
            profile_id: mapping.profile.profile_id.clone(),
            profile_version: mapping.profile.version.clone(),
            profile_hash: mapping.profile.sha256.clone(),
            mapping_id: mapping.mapping_id.clone(),
            mapping_version: mapping.version.clone(),
            mapping_hash: mapping_hash.to_owned(),
        };
        let original_hash = original_value_hash(&scope, &original_value, original_lineage)?;
        let mut current_hash =
            effective_value_hash(&scope, &original_hash, None, None, 0, &original_value)?;
        let mut effective_value = original_value;
        let mut visited = HashSet::from([current_hash.clone()]);
        while let Some(edge) = chain.remove(&current_hash) {
            if edge.original_value_hash != original_hash
                || !visited.insert(edge.effective_value_hash.clone())
            {
                return Err(KernelError::DecisionIntegrity(
                    "effective correction history has a mismatched original or cycle".to_owned(),
                ));
            }
            current_hash = edge.effective_value_hash;
            effective_value = edge.value;
        }
        if !chain.is_empty() {
            return Err(KernelError::DecisionIntegrity(
                "effective correction history is disconnected".to_owned(),
            ));
        }
        row.canonical_values.insert(field_id, effective_value);
    }
    Ok(())
}

/// Replay the exact effective canonical projection used by validation and retain
/// only the snapshot rows selected for a readiness publication.
pub(crate) fn project_effective_rows_for_publication(
    connection: &Connection,
    job_id: &str,
    validation_run_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    included_rows: &BTreeSet<(String, u64)>,
) -> Result<Vec<ProjectedRow>> {
    let mapping = load_stored_mapping(connection, mapping_id, mapping_version)?;
    if mapping.mapping_hash != mapping_hash {
        return Err(KernelError::DecisionIntegrity(
            "publication mapping hash differs from immutable mapping evidence".to_owned(),
        ));
    }
    if mapped_entity_ids(&mapping.document).len() > 1 {
        return Err(KernelError::DecisionIntegrity(
            "readiness publication is unavailable for multi-entity mappings".to_owned(),
        ));
    }
    let mut active_effective_chains = load_active_effective_chains(
        connection,
        job_id,
        validation_run_id,
        &mapping.document,
        mapping_hash,
        None,
    )?;
    let mut retained = Vec::with_capacity(included_rows.len());
    let mut retained_identities = BTreeSet::new();
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;

    loop {
        let rows = project_rows_batch(
            connection,
            job_id,
            &mapping.document,
            &after_file,
            after_ordinal,
            MAXIMUM_BATCH_SIZE,
        )?;
        if rows.is_empty() {
            break;
        }
        for mut row in rows.iter().cloned() {
            apply_active_effective_values(
                &mut row,
                job_id,
                validation_run_id,
                &mapping.document,
                mapping_hash,
                &mut active_effective_chains,
            )?;
            let identity = (row.source_file.clone(), row.source_ordinal);
            if included_rows.contains(&identity) {
                if !retained_identities.insert(identity) {
                    return Err(KernelError::DecisionIntegrity(
                        "publication projection contains a duplicate accepted row".to_owned(),
                    ));
                }
                row.source_values.clear();
                retained.push(row);
            }
        }
        let last = rows.last().expect("nonempty publication projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
    }

    if !active_effective_chains.is_empty() {
        return Err(KernelError::DecisionIntegrity(
            "publication projection left an effective correction unbound".to_owned(),
        ));
    }
    if retained_identities != *included_rows {
        return Err(KernelError::DecisionIntegrity(
            "publication snapshot rows differ from immutable accepted source evidence".to_owned(),
        ));
    }
    Ok(retained)
}

struct DeterministicRuleReplay {
    accumulator: ValidationAccumulator,
    source_exhausted: bool,
    bound_exceeded: bool,
}

/// Re-evaluate the implemented rule engine without writing state. This is the
/// integrity counterpart of `execute_rules`: it uses the same projection,
/// compiled rules, ordering, identity material, and configured bounds while
/// discarding pending issue bodies after their hashes and aggregates are kept.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn replay_rules_deterministically(
    connection: &Connection,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    rules: &[CompiledRule],
    relationships: &[CompiledRelationship],
    policy: &ValidationPolicy,
    target_rows: Option<u64>,
) -> Result<DeterministicRuleReplay> {
    let mut accumulator = ValidationAccumulator::for_mapping(mapping);
    let mut relationship_keys = vec![RelationshipKeyState::default(); relationships.len()];
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;
    loop {
        let batch_size = match target_rows {
            Some(target) => {
                let remaining = target.saturating_sub(accumulator.rows_processed);
                if remaining == 0 {
                    return Ok(DeterministicRuleReplay {
                        accumulator,
                        source_exhausted: false,
                        bound_exceeded: false,
                    });
                }
                policy
                    .row_batch_size
                    .min(usize::try_from(remaining).unwrap_or(usize::MAX))
            }
            None => policy.row_batch_size,
        };
        let rows = project_rows_batch(
            connection,
            job_id,
            mapping,
            &after_file,
            after_ordinal,
            batch_size,
        )?;
        if rows.is_empty() {
            let relationship_bound = match replay_relationship_issues_deterministically(
                connection,
                job_id,
                source_hash,
                mapping,
                mapping_hash,
                relationships,
                &relationship_keys,
                &mut accumulator,
                policy,
            ) {
                Ok(()) => false,
                Err(KernelError::ValidationBound(_)) => true,
                Err(error) => return Err(error),
            };
            accumulator.clear_pending_issues();
            return Ok(DeterministicRuleReplay {
                accumulator,
                source_exhausted: true,
                bound_exceeded: relationship_bound,
            });
        }
        for row in &rows {
            accumulator.rows_processed =
                accumulator.rows_processed.checked_add(1).ok_or_else(|| {
                    KernelError::ValidationBound("replayed row count overflow".to_owned())
                })?;
            for rule in rules {
                let outcome = evaluate_rule(rule, row, mapping, &mut accumulator, policy);
                let code = match outcome {
                    Ok(code) => code,
                    Err(KernelError::ValidationBound(_)) => {
                        accumulator.clear_pending_issues();
                        return Ok(DeterministicRuleReplay {
                            accumulator,
                            source_exhausted: false,
                            bound_exceeded: true,
                        });
                    }
                    Err(error) => return Err(error),
                };
                if let Some(code) = code {
                    let issue = make_issue(
                        source_hash,
                        &mapping.profile.sha256,
                        mapping_hash,
                        rule,
                        row,
                        code,
                    )?;
                    match accumulator.push_issue(issue, policy.maximum_issues) {
                        Ok(()) => {}
                        Err(KernelError::ValidationBound(_)) => {
                            accumulator.clear_pending_issues();
                            return Ok(DeterministicRuleReplay {
                                accumulator,
                                source_exhausted: false,
                                bound_exceeded: true,
                            });
                        }
                        Err(error) => return Err(error),
                    }
                    if accumulator.pending_issues.len() >= MAXIMUM_PENDING_ISSUES
                        || accumulator.issue_count == policy.maximum_issues
                    {
                        accumulator.clear_pending_issues();
                    }
                }
            }
            if let Err(error) = index_relationship_keys(
                row,
                relationships,
                &mut relationship_keys,
                &mut accumulator,
                policy,
            ) {
                if matches!(error, KernelError::ValidationBound(_)) {
                    accumulator.clear_pending_issues();
                    return Ok(DeterministicRuleReplay {
                        accumulator,
                        source_exhausted: false,
                        bound_exceeded: true,
                    });
                }
                return Err(error);
            }
        }
        let last = rows
            .last()
            .expect("nonempty deterministic projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
        accumulator.clear_pending_issues();
    }
}

#[allow(clippy::too_many_arguments)]
fn replay_relationship_issues_deterministically(
    connection: &Connection,
    job_id: &str,
    source_hash: &str,
    mapping: &MappingDocument,
    mapping_hash: &str,
    relationships: &[CompiledRelationship],
    states: &[RelationshipKeyState],
    accumulator: &mut ValidationAccumulator,
    policy: &ValidationPolicy,
) -> Result<()> {
    if relationships.is_empty() {
        return Ok(());
    }
    let mut after_file = String::new();
    let mut after_ordinal = 0_u64;
    loop {
        let rows = project_rows_batch(
            connection,
            job_id,
            mapping,
            &after_file,
            after_ordinal,
            policy.row_batch_size,
        )?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            evaluate_relationship_issues_for_row(
                source_hash,
                mapping_hash,
                mapping,
                row,
                relationships,
                states,
                accumulator,
                policy,
            )?;
            if accumulator.pending_issues.len() >= MAXIMUM_PENDING_ISSUES
                || accumulator.issue_count == policy.maximum_issues
            {
                accumulator.clear_pending_issues();
            }
        }
        let last = rows
            .last()
            .expect("nonempty deterministic relationship projection batch");
        after_file.clone_from(&last.source_file);
        after_ordinal = last.source_ordinal;
        accumulator.clear_pending_issues();
    }
}

fn persist_pending(
    writer: &WorkspaceWriter,
    run_id: &str,
    accumulator: &mut ValidationAccumulator,
) -> Result<()> {
    persist_batch(
        writer,
        run_id,
        &accumulator.pending_issues,
        accumulator.rows_processed,
        accumulator.issue_count,
    )?;
    accumulator.clear_pending_issues();
    Ok(())
}

fn should_cancel(control: &ValidationControl, completed_batches: u64) -> bool {
    control
        .cancel_after_batches
        .is_some_and(|maximum| completed_batches >= maximum)
        || control.cancel_file.as_deref().is_some_and(Path::exists)
}

fn evaluate_rule(
    rule: &CompiledRule,
    row: &ProjectedRow,
    mapping: &MappingDocument,
    accumulator: &mut ValidationAccumulator,
    policy: &ValidationPolicy,
) -> Result<Option<&'static str>> {
    if row.entity_id != rule.entity_id {
        return Ok(None);
    }
    let value = row
        .canonical_values
        .get(&rule.field_id)
        .and_then(Option::as_deref);
    let missing = value.is_none_or(str::is_empty);
    match &rule.kind {
        CompiledKind::Required => Ok(missing.then_some("REQUIRED_MISSING")),
        CompiledKind::MaximumLength(maximum) => Ok(value
            .is_some_and(|value| value.chars().count() > *maximum)
            .then_some("MAX_LENGTH_EXCEEDED")),
        CompiledKind::AllowedValues(values) => Ok(value
            .is_some_and(|value| !value.is_empty() && !values.contains(value))
            .then_some("ALLOWED_VALUE_MISMATCH")),
        CompiledKind::Pattern(pattern) => Ok(value
            .is_some_and(|value| !value.is_empty() && !pattern.is_match(value))
            .then_some("PATTERN_MISMATCH")),
        CompiledKind::Unique(scope_fields) => {
            let mut values = Vec::with_capacity(scope_fields.len());
            for field in scope_fields {
                let Some(value) = row
                    .canonical_values
                    .get(field)
                    .and_then(Option::as_deref)
                    .filter(|value| !value.is_empty())
                else {
                    return Ok(None);
                };
                values.push(Value::String(value.to_owned()));
            }
            let key = canonical_json_string(&Value::Array(values))?;
            let scope = (rule.entity_id.clone(), rule.rule_id.clone());
            if accumulator
                .unique_values
                .get(&scope)
                .is_some_and(|seen| seen.contains(&key))
            {
                return Ok(Some("UNIQUE_DUPLICATE"));
            }
            accumulator.reserve_key_state(key.len(), policy)?;
            accumulator
                .unique_values
                .entry(scope)
                .or_default()
                .insert(key);
            Ok(None)
        }
        CompiledKind::ConditionalRequired { when_field, equals } => {
            let condition = row
                .canonical_values
                .get(when_field)
                .and_then(Option::as_deref)
                == Some(equals.as_str());
            Ok((condition && missing).then_some("CONDITIONAL_REQUIRED_MISSING"))
        }
        CompiledKind::ReferenceLookup {
            table_id,
            table_version,
        } => {
            let table = mapping
                .reference_tables
                .iter()
                .find(|table| {
                    table.table_id == *table_id
                        && table.version == *table_version
                        && mapping.effective_table_entity(table) == rule.entity_id
                })
                .ok_or_else(|| {
                    KernelError::ValidationInvalid(format!(
                        "reference table {table_id}/{table_version} disappeared from mapping"
                    ))
                })?;
            Ok(value
                .is_some_and(|value| !value.is_empty() && !table.values.contains_key(value))
                .then_some("REFERENCE_LOOKUP_MISSING"))
        }
    }
}

fn make_issue(
    source_hash: &str,
    profile_hash: &str,
    mapping_hash: &str,
    rule: &CompiledRule,
    row: &ProjectedRow,
    issue_code: &str,
) -> Result<IssueEvidence> {
    let lineage = row
        .lineage
        .get(&rule.field_id)
        .cloned()
        .unwrap_or_else(|| json!({ "kind": "unmapped" }));
    let material = issue_material(
        source_hash,
        profile_hash,
        mapping_hash,
        &rule.severity,
        &rule.rule_id,
        issue_code,
        &rule.message,
        &rule.entity_id,
        &rule.field_id,
        &row.source_file,
        row.source_ordinal,
        row.line_start,
        row.line_end,
        &lineage,
    );
    let issue_hash = canonical_hash(&material)?;
    Ok(IssueEvidence {
        issue_id: format!("issue-{issue_hash}"),
        issue_hash,
        severity: rule.severity.clone(),
        rule_id: rule.rule_id.clone(),
        issue_code: issue_code.to_owned(),
        display_message: rule.message.clone(),
        entity_id: rule.entity_id.clone(),
        field_id: rule.field_id.clone(),
        source_file: row.source_file.clone(),
        source_ordinal: row.source_ordinal,
        line_start: row.line_start,
        line_end: row.line_end,
        lineage_json: canonical_json_string(&lineage)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn issue_material(
    source_hash: &str,
    profile_hash: &str,
    mapping_hash: &str,
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
) -> Value {
    json!({
        "contract": "migration-stream-validation-issue/v1",
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
        "sourceOrdinal": source_ordinal,
        "lineStart": line_start,
        "lineEnd": line_end,
        "lineage": lineage
    })
}

fn persist_batch(
    writer: &WorkspaceWriter,
    run_id: &str,
    issues: &[IssueEvidence],
    rows_processed: u64,
    issue_count: usize,
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    insert_issues(&transaction, run_id, issues)?;
    let changed = transaction.execute(
        "UPDATE validation_runs
         SET rows_processed = ?1, issue_count = ?2, updated_at = ?3
         WHERE id = ?4 AND state = 'RUNNING'",
        params![
            sqlite_integer(rows_processed, "validation rows processed")?,
            sqlite_integer(
                u64::try_from(issue_count).unwrap_or(u64::MAX),
                "validation issue count"
            )?,
            now_rfc3339(),
            run_id,
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "validation run {run_id} is no longer RUNNING during batch persistence"
        )));
    }
    transaction.commit()?;
    Ok(())
}

fn insert_issues(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
    issues: &[IssueEvidence],
) -> Result<()> {
    for issue in issues {
        transaction.execute(
            "INSERT INTO validation_issues (
                 run_id, issue_id, severity, rule_id, issue_code, display_message,
                 entity_id, field_id, source_file, source_ordinal, line_start,
                 line_end, lineage_json, issue_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                run_id,
                issue.issue_id,
                issue.severity,
                issue.rule_id,
                issue.issue_code,
                issue.display_message,
                issue.entity_id,
                issue.field_id,
                issue.source_file,
                sqlite_integer(issue.source_ordinal, "issue source ordinal")?,
                sqlite_integer(issue.line_start, "issue line start")?,
                sqlite_integer(issue.line_end, "issue line end")?,
                issue.lineage_json,
                issue.issue_hash,
            ],
        )?;
    }
    Ok(())
}

fn complete_run(
    writer: &WorkspaceWriter,
    run_id: &str,
    result: &ValidationResult,
    result_json: &str,
    result_hash: &str,
    pending_issues: &[IssueEvidence],
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    insert_issues(&transaction, run_id, pending_issues)?;
    let changed = transaction.execute(
        "UPDATE validation_runs
         SET issue_count = ?1
         WHERE id = ?2 AND state = 'RUNNING'",
        params![
            sqlite_integer(result.issue_count, "validation issue count")?,
            run_id
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "could not reconcile pending relationship issues for run {run_id}"
        )));
    }
    let (actual_issues, durable_rows, durable_issue_count): (i64, i64, i64) = transaction
        .query_row(
            "SELECT (SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1),
                rows_processed, issue_count
         FROM validation_runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if u64::try_from(actual_issues).ok() != Some(result.issue_count)
        || u64::try_from(durable_issue_count).ok() != Some(result.issue_count)
        || u64::try_from(durable_rows).ok() != Some(result.rows_processed)
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation run {run_id} rows/issues do not reconcile before completion"
        )));
    }
    let now = now_rfc3339();
    let terminal_hash = terminal_checkpoint_hash(
        &transaction,
        run_id,
        ValidationRunState::Complete.as_str(),
        None,
        None,
        &now,
        Some(result_hash),
    )?;
    let changed = transaction.execute(
        "UPDATE validation_runs
         SET state = 'COMPLETE', result_json = ?1, result_hash = ?2,
             terminal_hash = ?3, issue_count = ?4, rows_processed = ?5,
             updated_at = ?6, completed_at = ?6
         WHERE id = ?7 AND state = 'RUNNING'",
        params![
            result_json,
            result_hash,
            terminal_hash,
            sqlite_integer(result.issue_count, "validation issue count")?,
            sqlite_integer(result.rows_processed, "validation row count")?,
            now,
            run_id,
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "could not complete validation run {run_id}; changed {changed} rows"
        )));
    }
    insert_event(
        &transaction,
        run_id,
        ValidationRunState::Complete,
        None,
        None,
        &now,
    )?;
    transaction.commit()?;
    Ok(())
}

fn mark_terminal(
    writer: &WorkspaceWriter,
    run_id: &str,
    state: ValidationRunState,
    code: &str,
    detail: &str,
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let terminal_hash = terminal_checkpoint_hash(
        &transaction,
        run_id,
        state.as_str(),
        Some(code),
        Some(detail),
        &now,
        None,
    )?;
    let changed = transaction.execute(
        "UPDATE validation_runs
         SET state = ?1, error_code = ?2, error_message = ?3,
             terminal_hash = ?4, updated_at = ?5, completed_at = ?5
         WHERE id = ?6 AND state = 'RUNNING'",
        params![state.as_str(), code, detail, terminal_hash, now, run_id],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(format!(
            "could not make validation run {run_id} terminal; changed {changed} rows"
        )));
    }
    insert_event(&transaction, run_id, state, Some(code), Some(detail), &now)?;
    transaction.commit()?;
    Ok(())
}

fn insert_event(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
    state: ValidationRunState,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO validation_events (run_id, state, error_code, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, state.as_str(), error_code, detail, occurred_at],
    )?;
    Ok(())
}

fn mapped_entity_ids(mapping: &MappingDocument) -> HashSet<&str> {
    if mapping.has_explicit_entity_scope() {
        mapping
            .source_entity_assignments
            .iter()
            .map(|assignment| assignment.entity_id.as_str())
            .collect()
    } else {
        std::iter::once(mapping.root_entity_id.as_str()).collect()
    }
}

fn compile_relationships(mapping: &MappingDocument) -> Result<Vec<CompiledRelationship>> {
    if mapping.relationship_bindings.len() > MAXIMUM_ACTIVE_RELATIONSHIPS {
        return Err(KernelError::ValidationBound(format!(
            "mapping declares {} active relationships; maximum is {MAXIMUM_ACTIVE_RELATIONSHIPS}",
            mapping.relationship_bindings.len()
        )));
    }
    let mapped_entities = mapped_entity_ids(mapping);
    let mut relationship_ids = HashSet::with_capacity(mapping.relationship_bindings.len());
    mapping
        .relationship_bindings
        .iter()
        .map(|relationship: &MappingRelationshipBinding| {
            if relationship.relationship_id.trim().is_empty()
                || relationship.relationship_id.chars().count() > 128
                || !relationship_ids.insert(relationship.relationship_id.as_str())
            {
                return Err(KernelError::ValidationInvalid(
                    "mapping relationship IDs must be unique bounded nonempty values".to_owned(),
                ));
            }
            if !mapped_entities.contains(relationship.from_entity_id.as_str())
                || !mapped_entities.contains(relationship.to_entity_id.as_str())
            {
                return Err(KernelError::ValidationInvalid(format!(
                    "relationship {:?} endpoints are not both mapped entities",
                    relationship.relationship_id
                )));
            }
            if !matches!(relationship.cardinality.as_str(), "0..*" | "1..*") {
                return Err(KernelError::ValidationInvalid(format!(
                    "relationship {:?} has unsupported bounded cardinality {:?}",
                    relationship.relationship_id, relationship.cardinality
                )));
            }
            if relationship.key_pairs.is_empty()
                || relationship.key_pairs.len() > MAXIMUM_RELATIONSHIP_KEY_PAIRS
            {
                return Err(KernelError::ValidationInvalid(format!(
                    "relationship {:?} must contain 1..={MAXIMUM_RELATIONSHIP_KEY_PAIRS} key pairs",
                    relationship.relationship_id
                )));
            }
            let mut from_fields = HashSet::with_capacity(relationship.key_pairs.len());
            let mut to_fields = HashSet::with_capacity(relationship.key_pairs.len());
            let key_pairs = relationship
                .key_pairs
                .iter()
                .map(|pair| {
                    if pair.from_field_id.trim().is_empty()
                        || pair.to_field_id.trim().is_empty()
                        || pair.from_field_id.chars().count() > 128
                        || pair.to_field_id.chars().count() > 128
                        || !from_fields.insert(pair.from_field_id.as_str())
                        || !to_fields.insert(pair.to_field_id.as_str())
                    {
                        return Err(KernelError::ValidationInvalid(format!(
                            "relationship {:?} has blank, overlong, or repeated key fields",
                            relationship.relationship_id
                        )));
                    }
                    Ok((pair.from_field_id.clone(), pair.to_field_id.clone()))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(CompiledRelationship {
                relationship_id: relationship.relationship_id.clone(),
                from_entity_id: relationship.from_entity_id.clone(),
                to_entity_id: relationship.to_entity_id.clone(),
                cardinality: relationship.cardinality.clone(),
                key_pairs,
            })
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn compile_rules(
    value: &Value,
    mapping: &MappingDocument,
    policy: &ValidationPolicy,
) -> Result<Vec<CompiledRule>> {
    let rules = value.as_array().ok_or_else(|| {
        KernelError::ValidationInvalid("approved profile rules must be an array".to_owned())
    })?;
    let mapped_entities = mapped_entity_ids(mapping);
    let mut active_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let target = rule
            .get("target")
            .and_then(Value::as_str)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| {
                KernelError::ValidationInvalid(
                    "approved profile rule target must be a nonempty string".to_owned(),
                )
            })?;
        let (entity_id, field_id) = target.split_once('.').ok_or_else(|| {
            KernelError::ValidationInvalid(
                "approved profile rule target must have Entity.field structure".to_owned(),
            )
        })?;
        if entity_id.is_empty() || field_id.is_empty() {
            return Err(KernelError::ValidationInvalid(
                "approved profile rule target must have nonempty Entity.field parts".to_owned(),
            ));
        }
        if mapped_entities.contains(entity_id) {
            active_rules.push(rule);
        }
    }
    let pattern_sources = active_rules
        .iter()
        .filter(|rule| rule.get("kind").and_then(Value::as_str) == Some("pattern"))
        .map(|rule| {
            rule.pointer("/parameters/pattern")
                .and_then(Value::as_str)
                .map(str::len)
                .ok_or_else(|| {
                    KernelError::ValidationInvalid(
                        "pattern rule has no string pattern parameter".to_owned(),
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let total_pattern_bytes = pattern_sources
        .iter()
        .try_fold(0_usize, |total, size| total.checked_add(*size))
        .ok_or_else(|| KernelError::ValidationBound("total pattern bytes overflow".to_owned()))?;
    if total_pattern_bytes > policy.maximum_total_pattern_bytes {
        return Err(KernelError::ValidationBound(format!(
            "approved rule set requires {total_pattern_bytes} pattern bytes; configured aggregate bound is {}",
            policy.maximum_total_pattern_bytes
        )));
    }
    let per_pattern_program_bytes = if pattern_sources.is_empty() {
        policy.maximum_pattern_program_bytes
    } else {
        policy.maximum_pattern_program_bytes / pattern_sources.len()
    };
    if !pattern_sources.is_empty() && per_pattern_program_bytes == 0 {
        return Err(KernelError::ValidationBound(
            "aggregate pattern program budget is smaller than the pattern rule count".to_owned(),
        ));
    }
    let mut result = Vec::with_capacity(active_rules.len());
    for rule in active_rules {
        let rule_id = required_string(rule, "ruleId")?;
        let severity = match required_string(rule, "severity")?.as_str() {
            "error" => "ERROR",
            "warning" => "WARNING",
            "information" => "INFORMATION",
            other => {
                return Err(KernelError::ValidationInvalid(format!(
                    "rule {rule_id:?} has unsupported severity {other:?}"
                )));
            }
        }
        .to_owned();
        let target = required_string(rule, "target")?;
        let (entity_id, field_id) = target.split_once('.').ok_or_else(|| {
            KernelError::ValidationInvalid(format!(
                "rule {rule_id:?} target must have Entity.field structure"
            ))
        })?;
        let rule_kind = required_string(rule, "kind")?;
        let empty_parameters = serde_json::Map::new();
        let parameters = match rule.get("parameters") {
            Some(parameters) => parameters.as_object().ok_or_else(|| {
                KernelError::ValidationInvalid(format!(
                    "rule {rule_id:?} parameters must be an object"
                ))
            })?,
            None if rule_kind == "required" => &empty_parameters,
            None => {
                return Err(KernelError::ValidationInvalid(format!(
                    "rule {rule_id:?} parameters must be an object"
                )));
            }
        };
        let kind = match rule_kind.as_str() {
            "required" => CompiledKind::Required,
            "max_length" => CompiledKind::MaximumLength(
                usize::try_from(required_u64(parameters, "maximum", &rule_id)?).map_err(|_| {
                    KernelError::ValidationInvalid(format!(
                        "rule {rule_id:?} maximum does not fit this platform"
                    ))
                })?,
            ),
            "allowed_values" => {
                let values = parameters
                    .get("values")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} values must be an array"
                        ))
                    })?
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            KernelError::ValidationInvalid(format!(
                                "rule {rule_id:?} allowed values must be strings"
                            ))
                        })
                    })
                    .collect::<Result<HashSet<_>>>()?;
                CompiledKind::AllowedValues(values)
            }
            "pattern" => {
                let pattern = parameters
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} pattern must be a string"
                        ))
                    })?;
                if pattern.len() > policy.maximum_pattern_bytes {
                    return Err(KernelError::ValidationBound(format!(
                        "rule {rule_id:?} pattern exceeds configured byte bound"
                    )));
                }
                let compiled = RegexBuilder::new(pattern)
                    .size_limit(per_pattern_program_bytes)
                    .build()
                    .map_err(|error| match error {
                        regex::Error::CompiledTooBig(_) => KernelError::ValidationBound(format!(
                            "rule {rule_id:?} pattern exceeds its share of the configured aggregate program bound"
                        )),
                        _ => KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} pattern is invalid: {error}"
                        )),
                    })?;
                CompiledKind::Pattern(compiled)
            }
            "unique" => {
                let fields = parameters
                    .get("scopeFields")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} scopeFields must be an array"
                        ))
                    })?
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            KernelError::ValidationInvalid(format!(
                                "rule {rule_id:?} scope fields must be strings"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                CompiledKind::Unique(fields)
            }
            "conditional_required" => {
                let when = parameters
                    .get("whenField")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} whenField must be a string"
                        ))
                    })?;
                let (when_entity, when_field) = when.split_once('.').ok_or_else(|| {
                    KernelError::ValidationInvalid(format!(
                        "rule {rule_id:?} whenField must have Entity.field structure"
                    ))
                })?;
                if when_entity != entity_id {
                    return Err(KernelError::ValidationInvalid(format!(
                        "rule {rule_id:?} condition must target the same mapped entity"
                    )));
                }
                let equals = parameters
                    .get("equals")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::ValidationInvalid(format!(
                            "rule {rule_id:?} equals must be a string"
                        ))
                    })?;
                CompiledKind::ConditionalRequired {
                    when_field: when_field.to_owned(),
                    equals: equals.to_owned(),
                }
            }
            "reference_lookup" => {
                let table_id = required_parameter_string(parameters, "tableId", &rule_id)?;
                let table_version =
                    required_parameter_string(parameters, "tableVersion", &rule_id)?;
                if !mapping.reference_tables.iter().any(|table| {
                    table.table_id == table_id
                        && table.version == table_version
                        && mapping.effective_table_entity(table) == entity_id
                }) {
                    return Err(KernelError::ValidationInvalid(format!(
                        "rule {rule_id:?} references absent table {table_id}/{table_version}"
                    )));
                }
                CompiledKind::ReferenceLookup {
                    table_id,
                    table_version,
                }
            }
            other => {
                return Err(KernelError::ValidationInvalid(format!(
                    "rule {rule_id:?} has unsupported kind {other:?}"
                )));
            }
        };
        result.push(CompiledRule {
            rule_id,
            severity,
            entity_id: entity_id.to_owned(),
            field_id: field_id.to_owned(),
            message: required_string(rule, "message")?,
            kind,
        });
    }
    Ok(result)
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| KernelError::ValidationInvalid(format!("{key} must be a nonempty string")))
}

fn required_parameter_string(
    parameters: &serde_json::Map<String, Value>,
    key: &str,
    rule_id: &str,
) -> Result<String> {
    parameters
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            KernelError::ValidationInvalid(format!(
                "rule {rule_id:?} parameter {key} must be a nonempty string"
            ))
        })
}

fn required_u64(
    parameters: &serde_json::Map<String, Value>,
    key: &str,
    rule_id: &str,
) -> Result<u64> {
    parameters
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            KernelError::ValidationInvalid(format!(
                "rule {rule_id:?} parameter {key} must be a positive integer"
            ))
        })
}

fn validate_lookup_bounds(mapping: &MappingDocument, policy: &ValidationPolicy) -> Result<()> {
    let mut entries = 0_usize;
    let mut bytes = 0_usize;
    for table in &mapping.reference_tables {
        entries = entries.saturating_add(table.values.len());
        for (key, value) in &table.values {
            bytes = bytes.saturating_add(key.len()).saturating_add(value.len());
        }
    }
    if entries > policy.maximum_lookup_entries || bytes > policy.maximum_lookup_bytes {
        return Err(KernelError::ValidationBound(format!(
            "mapping lookup tables require {entries} entries/{bytes} bytes; configured bounds are {}/{}",
            policy.maximum_lookup_entries, policy.maximum_lookup_bytes
        )));
    }
    Ok(())
}

fn increment(counts: &mut BTreeMap<String, u64>, key: &str) -> Result<()> {
    let value = counts.entry(key.to_owned()).or_default();
    *value = value.checked_add(1).ok_or_else(|| {
        KernelError::ValidationBound(format!("validation count overflow for {key:?}"))
    })?;
    Ok(())
}

/// Return persisted status for any validation attempt without mutation.
///
/// # Errors
///
/// Rejects an unknown run or malformed authoritative counters/state.
pub fn validation_status(workspace: &Path, run_id: &str) -> Result<ValidationStatus> {
    let database_path = workspace.join(DATABASE_FILENAME);
    let connection = crate::state::open_read_only(&database_path)?;
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != SCHEMA_VERSION {
        return Err(KernelError::Conflict(format!(
            "validation status requires schema version {SCHEMA_VERSION}; found {schema_version}"
        )));
    }
    verify_validation_run_integrity(&connection, run_id)?;
    connection
        .query_row(
            "SELECT state, identity_hash, job_id, source_hash,
                    profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    rule_set_hash, engine_version, policy_hash,
                    rows_processed, issue_count, result_hash,
                    error_code, error_message, created_at, updated_at, completed_at
             FROM validation_runs WHERE id = ?1",
            [run_id],
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
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, Option<String>>(20)?,
                ))
            },
        )
        .optional()?
        .map(|row| -> Result<ValidationStatus> {
            Ok(ValidationStatus {
                schema_version: VALIDATION_SCHEMA_VERSION,
                run_id: run_id.to_owned(),
                state: ValidationRunState::parse(&row.0)?,
                identity_hash: row.1,
                job_id: row.2,
                source_hash: row.3,
                profile_id: row.4,
                profile_version: row.5,
                profile_hash: row.6,
                mapping_id: row.7,
                mapping_version: row.8,
                mapping_hash: row.9,
                rule_set_hash: row.10,
                engine_version: row.11,
                policy_hash: row.12,
                rows_processed: nonnegative(row.13, "validation rows processed")?,
                issue_count: nonnegative(row.14, "validation issue count")?,
                result_hash: row.15,
                error_code: row.16,
                error_message: row.17,
                created_at: row.18,
                updated_at: row.19,
                completed_at: row.20,
            })
        })
        .transpose()?
        .ok_or_else(|| KernelError::NotFound(format!("validation run {run_id}")))
}

fn nonnegative(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        KernelError::IntegrityFailed(format!("{label} is negative in authoritative state"))
    })
}

#[derive(Default)]
struct IssueReplay {
    count: u64,
    hashes: Vec<String>,
    severity_counts: BTreeMap<String, u64>,
    rule_counts: BTreeMap<String, u64>,
    entity_counts: BTreeMap<String, u64>,
    field_counts: BTreeMap<String, u64>,
    source_file_counts: BTreeMap<String, u64>,
}

struct ValidationIntegrityRecord {
    run_id: String,
    identity_hash: String,
    job_id: String,
    source_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    source_reuse_id: Option<String>,
    source_reuse_hash: Option<String>,
    rule_set_hash: String,
    engine_version: String,
    policy_json: String,
    policy_hash: String,
    state: String,
    result_json: Option<String>,
    result_hash: Option<String>,
    terminal_hash: Option<String>,
    issue_count: i64,
    rows_processed: i64,
    error_code: Option<String>,
    error_message: Option<String>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

const TERMINAL_HASH_UPDATE_TRIGGER: &str = "CREATE TRIGGER validation_terminal_hash_no_update BEFORE UPDATE OF terminal_hash ON validation_runs WHEN OLD.terminal_hash IS NOT NULL BEGIN SELECT RAISE(ABORT, 'validation terminal checkpoint is immutable'); END";
const TERMINAL_RUN_DELETE_TRIGGER: &str = "CREATE TRIGGER validation_terminal_run_no_delete BEFORE DELETE ON validation_runs WHEN OLD.terminal_hash IS NOT NULL BEGIN SELECT RAISE(ABORT, 'validation terminal history is immutable'); END";

fn normalized_schema_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_terminal_checkpoint_guards(connection: &Connection) -> Result<()> {
    for (name, expected) in [
        (
            "validation_terminal_hash_no_update",
            TERMINAL_HASH_UPDATE_TRIGGER,
        ),
        (
            "validation_terminal_run_no_delete",
            TERMINAL_RUN_DELETE_TRIGGER,
        ),
    ] {
        let expected = normalized_schema_sql(expected);
        let sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1 AND tbl_name = 'validation_runs'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if sql.as_deref().map(normalized_schema_sql).as_deref() != Some(expected.as_str()) {
            return Err(KernelError::IntegrityFailed(format!(
                "validation terminal checkpoint guard {name} is missing or altered"
            )));
        }
    }
    Ok(())
}

pub(crate) fn terminal_checkpoint_hash(
    connection: &Connection,
    run_id: &str,
    state: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
    completed_at: &str,
    result_hash: Option<&str>,
) -> Result<String> {
    if !matches!(
        state,
        "COMPLETE" | "FAILED" | "INTERRUPTED" | "BOUND_EXCEEDED"
    ) {
        return Err(KernelError::IntegrityFailed(format!(
            "validation run {run_id} cannot seal nonterminal state {state:?}"
        )));
    }
    let (identity_hash, rows_processed, issue_count, created_at): (String, i64, i64, String) =
        connection.query_row(
            "SELECT identity_hash, rows_processed, issue_count, created_at
             FROM validation_runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let (actual_count, issue_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
             length(CAST(issue_id AS BLOB)) + length(CAST(issue_hash AS BLOB))
         ), 0) FROM validation_issues WHERE run_id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if actual_count != issue_count
        || usize::try_from(actual_count).unwrap_or(usize::MAX) > MAXIMUM_ISSUES
        || usize::try_from(issue_bytes).unwrap_or(usize::MAX) > MAXIMUM_ISSUE_REPLAY_BYTES
    {
        return Err(KernelError::IntegrityFailed(format!(
            "validation run {run_id} cannot seal unreconciled or oversized issue evidence"
        )));
    }
    let mut statement = connection.prepare(
        "SELECT issue_id, issue_hash FROM validation_issues
         WHERE run_id = ?1 ORDER BY issue_id",
    )?;
    let issues = statement
        .query_map([run_id], |row| {
            Ok(json!({
                "issueId": row.get::<_, String>(0)?,
                "issueHash": row.get::<_, String>(1)?,
            }))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    canonical_hash(&json!({
        "contract": "migration-stream-validation-terminal/v1",
        "runId": run_id,
        "identityHash": identity_hash,
        "state": state,
        "rowsProcessed": nonnegative(rows_processed, "validation rows processed")?,
        "issueCount": nonnegative(issue_count, "validation issue count")?,
        "errorCode": error_code,
        "errorMessage": error_message,
        "createdAt": created_at,
        "completedAt": completed_at,
        "resultHash": result_hash,
        "issues": issues,
    }))
}

struct RuleIssueIdentity {
    severity: String,
    entity_id: String,
    field_id: String,
    message: String,
    issue_code: String,
    relationship_lineage: Option<RelationshipLineageIdentity>,
}

/// Build the private, value-bearing input used to derive one Tour's durable
/// HMAC-suppressed finding multiset.
///
/// The caller must immediately pass this value to `build_compared_finding_set`
/// and persist only that function's opaque outputs. Canonical values are never
/// serialized into engagement comparison tables or read contracts.
#[allow(clippy::too_many_lines, clippy::type_complexity)]
pub(crate) fn compared_finding_set_input(
    connection: &Connection,
    run_id: &str,
    tour_id: &str,
) -> Result<ComparedFindingSetInput> {
    verify_validation_run_integrity(connection, run_id)?;
    let tour: Option<(
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
    )> = connection
        .query_row(
            "SELECT t.engagement_id, t.source_hash, t.manifest_hash,
                    t.profile_id, t.profile_version, t.profile_hash,
                    t.mapping_id, t.mapping_version, t.mapping_hash,
                    t.validation_result_hash,
                    v.job_id, v.rule_set_hash, v.policy_hash, v.engine_version
             FROM engagement_tours t
             JOIN validation_runs v ON v.id = t.validation_run_id
             WHERE t.tour_id = ?1 AND t.validation_run_id = ?2
               AND v.state = 'COMPLETE'",
            params![tour_id, run_id],
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
        )
        .optional()?;
    let Some(tour) = tour else {
        return Err(KernelError::IntegrityFailed(format!(
            "Tour {tour_id} is not bound to COMPLETE validation run {run_id}"
        )));
    };
    let run_anchor: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT source_hash, profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash, result_hash
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
            ))
        },
    )?;
    if run_anchor.0 != tour.1
        || run_anchor.1 != tour.3
        || run_anchor.2 != tour.4
        || run_anchor.3 != tour.5
        || run_anchor.4 != tour.6
        || run_anchor.5 != tour.7
        || run_anchor.6 != tour.8
        || run_anchor.7 != tour.9
    {
        return Err(KernelError::IntegrityFailed(format!(
            "Tour {tour_id} authority differs from validation run {run_id}"
        )));
    }

    let profile = load_verified_profile_document(connection, &tour.3, &tour.4, &tour.5)?;
    let mapping = load_stored_mapping(connection, &tour.6, &tour.7)?;
    if mapping.mapping_hash != tour.8 {
        return Err(KernelError::IntegrityFailed(format!(
            "Tour {tour_id} mapping hash differs from immutable mapping bytes"
        )));
    }
    let policy_json: String = connection.query_row(
        "SELECT policy_json FROM validation_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    let policy: ValidationPolicy =
        serde_json::from_value(parse_unique_json(policy_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "Tour {tour_id} validation policy is invalid: {error}"
            ))
        })?)?;
    policy.validate()?;
    let rules_value = profile
        .get("rules")
        .cloned()
        .ok_or_else(|| KernelError::IntegrityFailed("exact profile has no rules".to_owned()))?;
    if canonical_hash(&rules_value)? != tour.11 {
        return Err(KernelError::IntegrityFailed(format!(
            "Tour {tour_id} rule-set hash differs from exact profile"
        )));
    }
    let compiled_rules = compile_rules(&rules_value, &mapping.document, &policy)?;
    let unique_rules = compiled_rules
        .iter()
        .filter_map(|rule| match &rule.kind {
            CompiledKind::Unique(fields) => Some((
                rule.rule_id.as_str(),
                (rule.entity_id.as_str(), fields.as_slice()),
            )),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let relationships = compile_relationships(&mapping.document)?;
    let relationship_rules = relationship_comparison_rules(&relationships)?;
    let entity_keys = profile_entity_keys(&profile)?;

    let issue_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    if usize::try_from(issue_count).unwrap_or(usize::MAX) > MAXIMUM_ISSUES {
        return Err(KernelError::ValidationBound(
            "compared finding input exceeds maximum validation issues".to_owned(),
        ));
    }
    let mut statement = connection.prepare(
        "SELECT issue_id, issue_hash, severity, rule_id, issue_code,
                display_message, entity_id, field_id, source_file,
                source_ordinal, line_start, line_end
         FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id",
    )?;
    let raw_issues = statement
        .query_map([run_id], |row| {
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
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    let mut projection_cache = HashMap::<(String, u64), ProjectedRow>::new();
    let mut compared_issues = Vec::with_capacity(raw_issues.len());
    for raw in raw_issues {
        let source_ordinal = nonnegative(raw.9, "comparison source ordinal")?;
        let line_start = nonnegative(raw.10, "comparison line start")?;
        let line_end = nonnegative(raw.11, "comparison line end")?;
        let source_key = (raw.8.clone(), source_ordinal);
        if !projection_cache.contains_key(&source_key) {
            let source_row: Option<(i64, i64, String)> = connection
                .query_row(
                    "SELECT line_start, line_end, fields_json
                     FROM source_records
                     WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
                    params![tour.10, raw.8, raw.9],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some(source_row) = source_row else {
                return Err(KernelError::IntegrityFailed(format!(
                    "Tour {tour_id} issue {} has no exact accepted source row",
                    raw.0
                )));
            };
            let projected = project_record(
                &mapping.document,
                &raw.8,
                source_ordinal,
                nonnegative(source_row.0, "accepted source line start")?,
                nonnegative(source_row.1, "accepted source line end")?,
                serde_json::from_str(&source_row.2)?,
            )?;
            projection_cache.insert(source_key.clone(), projected);
        }
        let projected = projection_cache.get(&source_key).ok_or_else(|| {
            KernelError::IntegrityFailed("comparison projection cache is inconsistent".to_owned())
        })?;
        if projected.entity_id != raw.6
            || projected.line_start != line_start
            || projected.line_end != line_end
        {
            return Err(KernelError::IntegrityFailed(format!(
                "Tour {tour_id} issue {} row/entity evidence differs from projection",
                raw.0
            )));
        }
        let record = comparison_record_locator(projected, &entity_keys)?;
        let subject = if raw.4 == "UNIQUE_DUPLICATE" {
            let Some((entity_id, fields)) = unique_rules.get(raw.3.as_str()).copied() else {
                return Err(KernelError::IntegrityFailed(format!(
                    "Tour {tour_id} unique issue {} names no exact unique rule",
                    raw.0
                )));
            };
            if entity_id != raw.6 {
                return Err(KernelError::IntegrityFailed(format!(
                    "Tour {tour_id} unique issue {} crosses entity scope",
                    raw.0
                )));
            }
            ComparedIssueSubjectInput::UniqueGroup {
                scope_values: fields
                    .iter()
                    .map(|field_id| CanonicalFieldValueInput {
                        field_id: qualify_field(entity_id, field_id),
                        value: projected.canonical_values.get(field_id).cloned().flatten(),
                    })
                    .collect(),
            }
        } else if raw.4.starts_with("RELATIONSHIP_") {
            let relationship = relationship_rules.get(raw.3.as_str()).ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "Tour {tour_id} relationship issue {} names no declared relationship rule",
                    raw.0
                ))
            })?;
            if relationship.entity_id != raw.6 {
                return Err(KernelError::IntegrityFailed(format!(
                    "Tour {tour_id} relationship issue {} crosses entity scope",
                    raw.0
                )));
            }
            ComparedIssueSubjectInput::RelationshipRecord {
                record,
                relationship_id: relationship.relationship.relationship_id.clone(),
                role: relationship.role,
                cardinality: relationship.relationship.cardinality.clone(),
                key_pairs: relationship
                    .relationship
                    .key_pairs
                    .iter()
                    .map(|(from_field_id, to_field_id)| {
                        let role_field = if relationship.role == RelationshipRole::Parent {
                            from_field_id
                        } else {
                            to_field_id
                        };
                        RelationshipKeyValueInput {
                            from_field_id: qualify_field(
                                &relationship.relationship.from_entity_id,
                                from_field_id,
                            ),
                            to_field_id: qualify_field(
                                &relationship.relationship.to_entity_id,
                                to_field_id,
                            ),
                            value: projected
                                .canonical_values
                                .get(role_field)
                                .cloned()
                                .flatten(),
                        }
                    })
                    .collect(),
            }
        } else {
            ComparedIssueSubjectInput::RuleRecord { record }
        };
        compared_issues.push(ComparedIssueInput {
            issue_id: raw.0,
            issue_hash: raw.1,
            severity: raw.2,
            rule_id: raw.3,
            issue_code: raw.4,
            display_message: raw.5,
            entity_id: raw.6,
            field_id: raw.7,
            source_file: raw.8,
            source_ordinal,
            line_start,
            line_end,
            subject,
        });
    }
    Ok(ComparedFindingSetInput {
        schema_version: COMPARED_FINDING_SET_INPUT_SCHEMA.to_owned(),
        authority: ComparedFindingAuthority {
            schema_version: COMPARED_FINDING_AUTHORITY_SCHEMA.to_owned(),
            engagement_id: tour.0,
            profile_id: tour.3,
            profile_version: tour.4,
            profile_hash: tour.5,
            mapping_id: tour.6,
            mapping_version: tour.7,
            mapping_hash: tour.8,
            rule_set_hash: tour.11,
            validation_policy_hash: tour.12,
            validation_engine_version: tour.13,
        },
        tour_id: tour_id.to_owned(),
        validation_run_id: run_id.to_owned(),
        validation_result_hash: tour.9,
        source_hash: tour.1,
        manifest_hash: tour.2,
        issues: compared_issues,
    })
}

struct RelationshipComparisonRule<'a> {
    relationship: &'a CompiledRelationship,
    role: RelationshipRole,
    entity_id: &'a str,
}

fn relationship_comparison_rules(
    relationships: &[CompiledRelationship],
) -> Result<HashMap<String, RelationshipComparisonRule<'_>>> {
    let mut rules = HashMap::new();
    for relationship in relationships {
        for (suffix, role, entity_id) in [
            (
                "parent-key",
                RelationshipRole::Parent,
                relationship.from_entity_id.as_str(),
            ),
            (
                "minimum-child",
                RelationshipRole::Parent,
                relationship.from_entity_id.as_str(),
            ),
            (
                "child-key",
                RelationshipRole::Child,
                relationship.to_entity_id.as_str(),
            ),
            (
                "parent-exists",
                RelationshipRole::Child,
                relationship.to_entity_id.as_str(),
            ),
        ] {
            if suffix == "minimum-child" && relationship.cardinality != "1..*" {
                continue;
            }
            let rule_id = format!("relationship.{}.{}", relationship.relationship_id, suffix);
            if rules
                .insert(
                    rule_id.clone(),
                    RelationshipComparisonRule {
                        relationship,
                        role,
                        entity_id,
                    },
                )
                .is_some()
            {
                return Err(KernelError::IntegrityFailed(format!(
                    "relationship comparison rule identity {rule_id:?} is ambiguous"
                )));
            }
        }
    }
    Ok(rules)
}

fn profile_entity_keys(profile: &Value) -> Result<BTreeMap<String, Vec<String>>> {
    let entities = profile
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| KernelError::IntegrityFailed("exact profile has no entities".to_owned()))?;
    if entities.is_empty() || entities.len() > 1_000 {
        return Err(KernelError::IntegrityFailed(
            "exact profile entity keys are outside bounds".to_owned(),
        ));
    }
    let mut result = BTreeMap::new();
    for entity in entities {
        let entity_id = entity
            .get("entityId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or_else(|| {
                KernelError::IntegrityFailed("profile entity ID is invalid".to_owned())
            })?;
        let key = entity
            .get("key")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "profile entity {entity_id:?} has no declared key"
                ))
            })?
            .iter()
            .map(|field| {
                field
                    .as_str()
                    .filter(|value| !value.is_empty() && value.len() <= 256)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        KernelError::IntegrityFailed(format!(
                            "profile entity {entity_id:?} key field is invalid"
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        if key.is_empty()
            || key.len() > 64
            || key.iter().collect::<HashSet<_>>().len() != key.len()
            || result.insert(entity_id.to_owned(), key).is_some()
        {
            return Err(KernelError::IntegrityFailed(format!(
                "profile entity {entity_id:?} key identity is empty, repeated, or ambiguous"
            )));
        }
    }
    Ok(result)
}

fn comparison_record_locator(
    row: &ProjectedRow,
    entity_keys: &BTreeMap<String, Vec<String>>,
) -> Result<RecordLocatorInput> {
    let key = entity_keys.get(&row.entity_id).ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "projected entity {:?} has no profile-declared key",
            row.entity_id
        ))
    })?;
    let declared_key = key
        .iter()
        .map(|field_id| CanonicalFieldValueInput {
            field_id: qualify_field(&row.entity_id, field_id),
            value: row.canonical_values.get(field_id).cloned().flatten(),
        })
        .collect();
    let projected_row = row
        .canonical_values
        .iter()
        .map(|(field_id, value)| CanonicalFieldValueInput {
            field_id: qualify_field(&row.entity_id, field_id),
            value: value.clone(),
        })
        .collect();
    Ok(RecordLocatorInput {
        declared_key,
        projected_row,
    })
}

fn qualify_field(entity_id: &str, field_id: &str) -> String {
    format!("{entity_id}.{field_id}")
}

/// Verify the immutable run/result/authority checkpoint needed before a
/// practitioner-facing bounded page is presented. Page issue bodies receive a
/// separate bounded membership/hash/lineage replay.
pub(crate) fn verify_validation_run_for_read(connection: &Connection, run_id: &str) -> Result<()> {
    verify_validation_run(connection, run_id, false)
}

/// Verify only the issues selected for one bounded practitioner page/row.
/// The immutable result checkpoint supplies membership, while each selected
/// issue is re-hashed and its accepted-row projection lineage is rebuilt once
/// per unique source row under cumulative byte ceilings.
#[allow(clippy::too_many_lines)]
pub(crate) fn verify_validation_issue_page_for_read(
    connection: &Connection,
    run_id: &str,
    issue_ids: &[String],
) -> Result<()> {
    if issue_ids.len() > MAXIMUM_READ_PAGE_ISSUES {
        return Err(validation_read_integrity(
            run_id,
            "selected issue page exceeds its absolute item bound",
        ));
    }
    let unique_ids = issue_ids.iter().collect::<HashSet<_>>();
    if unique_ids.len() != issue_ids.len()
        || issue_ids
            .iter()
            .any(|issue_id| issue_id.is_empty() || issue_id.len() > 134)
    {
        return Err(validation_read_integrity(
            run_id,
            "selected issue page contains duplicate or invalid identities",
        ));
    }
    if issue_ids.is_empty() {
        return Ok(());
    }

    let (
        job_id,
        source_hash,
        profile_id,
        profile_version,
        profile_hash,
        mapping_id,
        mapping_version,
        mapping_hash,
        policy_json,
        result_json,
    ): (
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
    ) = connection.query_row(
        "SELECT job_id, source_hash, profile_id, profile_version, profile_hash,
                mapping_id, mapping_version, mapping_hash, policy_json, result_json
         FROM validation_runs WHERE id = ?1 AND state = 'COMPLETE'",
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
            ))
        },
    )?;
    let result: ValidationResult = serde_json::from_str(&result_json).map_err(|error| {
        validation_read_integrity(run_id, format!("result membership is invalid: {error}"))
    })?;
    let result_hashes = result.issue_hashes.into_iter().collect::<HashSet<_>>();
    let profile =
        load_verified_profile_document(connection, &profile_id, &profile_version, &profile_hash)?;
    let mapping = load_stored_mapping(connection, &mapping_id, &mapping_version)?;
    if mapping.mapping_hash != mapping_hash {
        return Err(validation_read_integrity(
            run_id,
            "selected issue page mapping hash differs from exact mapping",
        ));
    }
    let policy: ValidationPolicy = serde_json::from_str(&policy_json).map_err(|error| {
        validation_read_integrity(run_id, format!("validation policy is invalid: {error}"))
    })?;
    policy.validate()?;
    let rules_value = profile
        .get("rules")
        .cloned()
        .ok_or_else(|| validation_read_integrity(run_id, "exact profile has no rule array"))?;
    let mut rules = HashMap::new();
    for rule in compile_rules(&rules_value, &mapping.document, &policy)? {
        rules.insert(
            rule.rule_id,
            RuleIssueIdentity {
                severity: rule.severity,
                entity_id: rule.entity_id,
                field_id: rule.field_id,
                message: rule.message,
                issue_code: compiled_issue_code(&rule.kind).to_owned(),
                relationship_lineage: None,
            },
        );
    }
    for relationship in compile_relationships(&mapping.document)? {
        insert_relationship_issue_identities(&mut rules, &relationship)?;
    }

    let mut issue_bytes = 0_usize;
    let mut source_bytes = 0_usize;
    let mut projected_bytes = 0_usize;
    let mut projections: HashMap<(String, u64), ProjectedRow> = HashMap::new();
    for issue_id in issue_ids {
        let selected_bytes: Option<i64> = connection
            .query_row(
                "SELECT length(CAST(issue_id AS BLOB))
                      + length(CAST(severity AS BLOB))
                      + length(CAST(rule_id AS BLOB))
                      + length(CAST(issue_code AS BLOB))
                      + length(CAST(display_message AS BLOB))
                      + length(CAST(entity_id AS BLOB))
                      + length(CAST(field_id AS BLOB))
                      + length(CAST(source_file AS BLOB))
                      + length(CAST(lineage_json AS BLOB))
                      + length(CAST(issue_hash AS BLOB))
                 FROM validation_issues WHERE run_id = ?1 AND issue_id = ?2",
                params![run_id, issue_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(selected_bytes) = selected_bytes else {
            return Err(validation_read_integrity(
                run_id,
                format!("selected issue {issue_id} disappeared from the snapshot"),
            ));
        };
        issue_bytes =
            issue_bytes.saturating_add(usize::try_from(selected_bytes).unwrap_or(usize::MAX));
        if issue_bytes > MAXIMUM_READ_PAGE_ISSUE_BYTES {
            return Err(validation_read_integrity(
                run_id,
                "selected issue page exceeds its cumulative evidence-byte bound",
            ));
        }
        let raw: (
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
        ) = connection.query_row(
            "SELECT severity, rule_id, issue_code, display_message, entity_id, field_id,
                        source_file, source_ordinal, line_start, line_end, lineage_json, issue_hash
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
        )?;
        let source_ordinal = nonnegative(raw.7, "issue source ordinal")?;
        let line_start = nonnegative(raw.8, "issue line start")?;
        let line_end = nonnegative(raw.9, "issue line end")?;
        let source_key = (raw.6.clone(), source_ordinal);
        if !projections.contains_key(&source_key) {
            let source_row_size: Option<i64> = connection
                .query_row(
                    "SELECT length(CAST(fields_json AS BLOB)) FROM source_records
                     WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
                    params![job_id, raw.6, raw.7],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(source_row_size) = source_row_size else {
                return Err(validation_read_integrity(
                    run_id,
                    format!("selected issue {issue_id} has no accepted source row"),
                ));
            };
            source_bytes =
                source_bytes.saturating_add(usize::try_from(source_row_size).unwrap_or(usize::MAX));
            if source_bytes > MAXIMUM_READ_PAGE_SOURCE_BYTES {
                return Err(validation_read_integrity(
                    run_id,
                    "selected issue page exceeds its cumulative source-byte bound",
                ));
            }
            let source_row: (i64, i64, String) = connection.query_row(
                "SELECT line_start, line_end, fields_json FROM source_records
                 WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
                params![job_id, raw.6, raw.7],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let mut projected = project_record(
                &mapping.document,
                &raw.6,
                source_ordinal,
                nonnegative(source_row.0, "accepted line start")?,
                nonnegative(source_row.1, "accepted line end")?,
                serde_json::from_str(&source_row.2)?,
            )?;
            projected.source_values.clear();
            projected_bytes = projected_bytes.saturating_add(
                canonical_json_string(&json!({
                    "canonicalValues": &projected.canonical_values,
                    "lineage": &projected.lineage,
                }))?
                .len(),
            );
            if projected_bytes > MAXIMUM_READ_PAGE_SOURCE_BYTES {
                return Err(validation_read_integrity(
                    run_id,
                    "selected issue page exceeds its cumulative projection-byte bound",
                ));
            }
            projections.insert(source_key.clone(), projected);
        }
        let projected = projections.get(&source_key).ok_or_else(|| {
            validation_read_integrity(run_id, "selected issue projection cache is inconsistent")
        })?;
        let lineage = parse_unique_json(raw.10.as_bytes()).map_err(|error| {
            validation_read_integrity(
                run_id,
                format!("selected issue {issue_id} lineage is invalid: {error}"),
            )
        })?;
        let rule = rules.get(&raw.1).ok_or_else(|| {
            validation_read_integrity(
                run_id,
                format!("selected issue names unknown rule {:?}", raw.1),
            )
        })?;
        let mut expected_lineage = projected
            .lineage
            .get(&raw.5)
            .cloned()
            .unwrap_or_else(|| json!({ "kind": "unmapped" }));
        if let Some(identity) = &rule.relationship_lineage {
            augment_relationship_lineage(&mut expected_lineage, identity)?;
        }
        let material = issue_material(
            &source_hash,
            &profile_hash,
            &mapping_hash,
            &raw.0,
            &raw.1,
            &raw.2,
            &raw.3,
            &raw.4,
            &raw.5,
            &raw.6,
            source_ordinal,
            line_start,
            line_end,
            &lineage,
        );
        let expected_hash = canonical_hash(&material)?;
        if raw.0 != rule.severity
            || raw.2 != rule.issue_code
            || raw.3 != rule.message
            || raw.4 != rule.entity_id
            || raw.5 != rule.field_id
            || projected.entity_id != raw.4
            || projected.line_start != line_start
            || projected.line_end != line_end
            || expected_lineage != lineage
            || issue_id != &format!("issue-{expected_hash}")
            || raw.11 != expected_hash
            || !result_hashes.contains(&raw.11)
        {
            return Err(validation_read_integrity(
                run_id,
                format!(
                    "selected issue {issue_id} differs from immutable rule/projection/result evidence"
                ),
            ));
        }
    }
    Ok(())
}

fn compiled_issue_code(kind: &CompiledKind) -> &'static str {
    match kind {
        CompiledKind::Required => "REQUIRED_MISSING",
        CompiledKind::MaximumLength(_) => "MAX_LENGTH_EXCEEDED",
        CompiledKind::AllowedValues(_) => "ALLOWED_VALUE_MISMATCH",
        CompiledKind::Pattern(_) => "PATTERN_MISMATCH",
        CompiledKind::Unique(_) => "UNIQUE_DUPLICATE",
        CompiledKind::ConditionalRequired { .. } => "CONDITIONAL_REQUIRED_MISSING",
        CompiledKind::ReferenceLookup { .. } => "REFERENCE_LOOKUP_MISSING",
    }
}

fn insert_relationship_issue_identities(
    rules: &mut HashMap<String, RuleIssueIdentity>,
    relationship: &CompiledRelationship,
) -> Result<()> {
    let parent_field = relationship_anchor_field(relationship, true)?.to_owned();
    let child_field = relationship_anchor_field(relationship, false)?.to_owned();
    let parent_lineage = relationship_lineage_identity(relationship, true);
    let child_lineage = relationship_lineage_identity(relationship, false);
    let mut insert = |suffix: &str,
                      issue_code: &str,
                      message: &str,
                      entity_id: &str,
                      field_id: &str,
                      lineage: &RelationshipLineageIdentity| {
        let rule_id = format!("relationship.{}.{}", relationship.relationship_id, suffix);
        if rules.contains_key(&rule_id) {
            return Err(KernelError::ValidationInvalid(format!(
                "generated relationship rule ID {rule_id:?} is not unique"
            )));
        }
        rules.insert(
            rule_id,
            RuleIssueIdentity {
                severity: "ERROR".to_owned(),
                entity_id: entity_id.to_owned(),
                field_id: field_id.to_owned(),
                message: message.to_owned(),
                issue_code: issue_code.to_owned(),
                relationship_lineage: Some(lineage.clone()),
            },
        );
        Ok(())
    };
    insert(
        "parent-key",
        "RELATIONSHIP_KEY_MISSING",
        RELATIONSHIP_PARENT_KEY_MESSAGE,
        &relationship.from_entity_id,
        &parent_field,
        &parent_lineage,
    )?;
    insert(
        "child-key",
        "RELATIONSHIP_KEY_MISSING",
        RELATIONSHIP_CHILD_KEY_MESSAGE,
        &relationship.to_entity_id,
        &child_field,
        &child_lineage,
    )?;
    insert(
        "parent-exists",
        "RELATIONSHIP_PARENT_NOT_FOUND",
        RELATIONSHIP_PARENT_NOT_FOUND_MESSAGE,
        &relationship.to_entity_id,
        &child_field,
        &child_lineage,
    )?;
    if relationship.cardinality == "1..*" {
        insert(
            "minimum-child",
            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
            RELATIONSHIP_MINIMUM_CHILD_MESSAGE,
            &relationship.from_entity_id,
            &parent_field,
            &parent_lineage,
        )?;
    }
    Ok(())
}

fn verify_validation_run_integrity(connection: &Connection, run_id: &str) -> Result<()> {
    verify_validation_run(connection, run_id, true)
}

#[allow(clippy::too_many_lines)] // Shared checkpoint and optional full replay avoid divergent verifiers.
fn verify_validation_run(
    connection: &Connection,
    run_id: &str,
    deterministic_replay: bool,
) -> Result<()> {
    verify_terminal_checkpoint_guards(connection)?;
    let sizes: Option<(i64, i64, i64)> = connection
        .query_row(
            "SELECT length(CAST(policy_json AS BLOB)),
                    COALESCE(length(CAST(result_json AS BLOB)), 0),
                    length(CAST(id AS BLOB))
                  + length(CAST(identity_hash AS BLOB))
                  + length(CAST(job_id AS BLOB))
                  + length(CAST(source_hash AS BLOB))
                  + length(CAST(profile_id AS BLOB))
                  + length(CAST(profile_version AS BLOB))
                  + length(CAST(profile_hash AS BLOB))
                  + length(CAST(mapping_id AS BLOB))
                  + length(CAST(mapping_version AS BLOB))
                  + length(CAST(mapping_hash AS BLOB))
                  + COALESCE(length(CAST(source_reuse_id AS BLOB)), 0)
                  + COALESCE(length(CAST(source_reuse_hash AS BLOB)), 0)
                  + length(CAST(rule_set_hash AS BLOB))
                  + length(CAST(engine_version AS BLOB))
                  + length(CAST(policy_hash AS BLOB))
                  + length(CAST(state AS BLOB))
                  + COALESCE(length(CAST(result_hash AS BLOB)), 0)
                  + COALESCE(length(CAST(terminal_hash AS BLOB)), 0)
                  + COALESCE(length(CAST(error_code AS BLOB)), 0)
                  + COALESCE(length(CAST(error_message AS BLOB)), 0)
                  + length(CAST(created_at AS BLOB))
                  + length(CAST(updated_at AS BLOB))
                  + COALESCE(length(CAST(completed_at AS BLOB)), 0)
             FROM validation_runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((policy_bytes, result_bytes, metadata_bytes)) = sizes else {
        return Err(KernelError::NotFound(format!("validation run {run_id}")));
    };
    if usize::try_from(policy_bytes).unwrap_or(usize::MAX) > MAXIMUM_POLICY_BYTES
        || usize::try_from(result_bytes).unwrap_or(usize::MAX) > MAXIMUM_RESULT_BYTES
        || usize::try_from(metadata_bytes).unwrap_or(usize::MAX) > MAXIMUM_VALIDATION_METADATA_BYTES
    {
        return Err(validation_read_integrity(
            run_id,
            "policy/result/metadata evidence exceeds immutable replay bounds",
        ));
    }
    let record = connection.query_row(
        "SELECT id, identity_hash, job_id, source_hash,
                profile_id, profile_version, profile_hash,
                mapping_id, mapping_version, mapping_hash,
                source_reuse_id, source_reuse_hash,
                rule_set_hash, engine_version, policy_json, policy_hash,
                state, result_json, result_hash, terminal_hash,
                issue_count, rows_processed, error_code, error_message,
                created_at, updated_at, completed_at
         FROM validation_runs WHERE id = ?1",
        [run_id],
        |row| {
            Ok(ValidationIntegrityRecord {
                run_id: row.get(0)?,
                identity_hash: row.get(1)?,
                job_id: row.get(2)?,
                source_hash: row.get(3)?,
                profile_id: row.get(4)?,
                profile_version: row.get(5)?,
                profile_hash: row.get(6)?,
                mapping_id: row.get(7)?,
                mapping_version: row.get(8)?,
                mapping_hash: row.get(9)?,
                source_reuse_id: row.get(10)?,
                source_reuse_hash: row.get(11)?,
                rule_set_hash: row.get(12)?,
                engine_version: row.get(13)?,
                policy_json: row.get(14)?,
                policy_hash: row.get(15)?,
                state: row.get(16)?,
                result_json: row.get(17)?,
                result_hash: row.get(18)?,
                terminal_hash: row.get(19)?,
                issue_count: row.get(20)?,
                rows_processed: row.get(21)?,
                error_code: row.get(22)?,
                error_message: row.get(23)?,
                created_at: row.get(24)?,
                updated_at: row.get(25)?,
                completed_at: row.get(26)?,
            })
        },
    )?;
    let mut issues = Vec::new();
    let policy_value = parse_unique_json(record.policy_json.as_bytes()).map_err(|error| {
        validation_read_integrity(run_id, format!("policy JSON is invalid: {error}"))
    })?;
    if canonical_json_string(&policy_value)? != record.policy_json
        || canonical_hash(&policy_value)? != record.policy_hash
    {
        issues.push(format!(
            "validation run {run_id} policy hash/canonical bytes mismatch"
        ));
    }
    let replay_policy = match serde_json::from_value::<ValidationPolicy>(policy_value) {
        Ok(policy) => {
            if let Err(error) = policy.validate() {
                issues.push(format!(
                    "validation run {run_id} policy violates bounds: {error}"
                ));
                None
            } else {
                Some(policy)
            }
        }
        Err(error) => {
            issues.push(format!(
                "validation run {run_id} policy contract is invalid: {error}"
            ));
            None
        }
    };
    if record.engine_version != VALIDATION_ENGINE_VERSION {
        issues.push(format!(
            "validation run {run_id} engine version is not the implemented engine"
        ));
    }
    let expected_identity = validation_identity_hash(
        &record.source_hash,
        &record.profile_hash,
        &record.mapping_hash,
        &record.rule_set_hash,
        &record.policy_hash,
        record.source_reuse_hash.as_deref(),
    )?;
    if expected_identity != record.identity_hash {
        issues.push(format!("validation run {run_id} identity hash mismatch"));
    }
    let profile_document = verify_run_anchors(
        connection,
        &record.run_id,
        &record.job_id,
        &record.source_hash,
        &record.profile_id,
        &record.profile_version,
        &record.profile_hash,
        &record.mapping_id,
        &record.mapping_version,
        &record.mapping_hash,
        record.source_reuse_id.as_deref(),
        record.source_reuse_hash.as_deref(),
        &record.rule_set_hash,
        &mut issues,
    )?;
    let replay_mapping =
        match load_stored_mapping(connection, &record.mapping_id, &record.mapping_version) {
            Ok(mapping) if mapping.mapping_hash == record.mapping_hash => Some(mapping),
            Ok(_) => {
                issues.push(format!(
                    "validation run {run_id} mapping hash differs from verified mapping evidence"
                ));
                None
            }
            Err(error) => {
                issues.push(format!(
                    "validation run {run_id} mapping evidence replay failed: {error}"
                ));
                None
            }
        };
    let actual_issue_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
        [&record.run_id],
        |row| row.get(0),
    )?;
    if record.issue_count != actual_issue_count {
        issues.push(format!(
            "validation run {run_id} issue count differs from durable rows"
        ));
    }
    let mut replay = if deterministic_replay {
        Some(replay_issues(
            connection,
            &record.run_id,
            &record.source_hash,
            &record.profile_hash,
            &record.mapping_hash,
            replay_mapping
                .as_ref()
                .is_some_and(|mapping| mapping.document.has_explicit_entity_scope()),
            &mut issues,
        )?)
    } else {
        None
    };
    let parsed_state = ValidationRunState::parse(&record.state).map_err(|error| {
        validation_read_integrity(run_id, format!("run state is invalid: {error}"))
    })?;
    let mut deterministic = None;
    if deterministic_replay
        && let (Some(profile), Some(mapping), Some(policy)) = (
            profile_document.as_ref(),
            replay_mapping.as_ref(),
            replay_policy.as_ref(),
        )
    {
        let rules_value = profile.get("rules").cloned().ok_or_else(|| {
            validation_read_integrity(run_id, "exact profile has no replayable rule set")
        })?;
        match validate_lookup_bounds(&mapping.document, policy).and_then(|()| {
            Ok((
                compile_rules(&rules_value, &mapping.document, policy)?,
                compile_relationships(&mapping.document)?,
            ))
        }) {
            Ok((rules, relationships)) => {
                let target_rows = (parsed_state != ValidationRunState::Complete)
                    .then(|| nonnegative(record.rows_processed, "validation rows processed"))
                    .transpose()?;
                match replay_rules_deterministically(
                    connection,
                    &record.job_id,
                    &record.source_hash,
                    &mapping.document,
                    &record.mapping_hash,
                    &rules,
                    &relationships,
                    policy,
                    target_rows,
                ) {
                    Ok(replayed) => {
                        if parsed_state == ValidationRunState::Complete
                            && (!replayed.source_exhausted || replayed.bound_exceeded)
                        {
                            issues.push(format!(
                                "validation run {run_id} cannot be COMPLETE because deterministic replay did not exhaust its bounded source"
                            ));
                        }
                        if replayed.bound_exceeded
                            && parsed_state != ValidationRunState::BoundExceeded
                        {
                            issues.push(format!(
                                "validation run {run_id} deterministic replay reached a bound outside BOUND_EXCEEDED state"
                            ));
                        }
                        deterministic = Some(replayed.accumulator);
                    }
                    Err(error) => issues.push(format!(
                        "validation run {run_id} deterministic rule replay failed: {error}"
                    )),
                }
            }
            Err(error @ KernelError::ValidationBound(_)) => {
                if parsed_state == ValidationRunState::BoundExceeded
                    && record.rows_processed == 0
                    && record.issue_count == 0
                    && record.error_code.as_deref() == Some("VALIDATION_BOUND_EXCEEDED")
                {
                    deterministic = Some(ValidationAccumulator::default());
                } else {
                    issues.push(format!(
                        "validation run {run_id} rule compilation replay reached an inconsistent bound: {error}"
                    ));
                }
            }
            Err(error) => issues.push(format!(
                "validation run {run_id} exact rule compilation failed: {error}"
            )),
        }
    }
    if let (Some(expected), Some(replay)) = (deterministic.as_mut(), replay.as_mut()) {
        expected.issue_hashes.sort_unstable();
        replay.hashes.sort_unstable();
        if expected.rows_processed
            != nonnegative(record.rows_processed, "validation rows processed")?
            || u64::try_from(expected.issue_count).unwrap_or(u64::MAX) != replay.count
            || expected.issue_hashes != replay.hashes
            || expected.severity_counts != replay.severity_counts
            || expected.rule_counts != replay.rule_counts
            || expected.entity_counts != replay.entity_counts
            || expected.field_counts != replay.field_counts
            || expected.source_file_counts != replay.source_file_counts
        {
            issues.push(format!(
                "validation run {run_id} persisted rows/issues differ from deterministic profile-to-rule replay"
            ));
        }
    }
    verify_validation_events(
        connection,
        &record.run_id,
        &ValidationLifecycleAnchor {
            state: parsed_state,
            error_code: record.error_code.as_deref(),
            error_message: record.error_message.as_deref(),
            created_at: &record.created_at,
            completed_at: record.completed_at.as_deref(),
        },
        &mut issues,
    )?;
    let created = chrono::DateTime::parse_from_rfc3339(&record.created_at).ok();
    let updated = chrono::DateTime::parse_from_rfc3339(&record.updated_at).ok();
    if created.is_none()
        || updated.is_none()
        || created
            .zip(updated)
            .is_some_and(|(created, updated)| updated < created)
        || (parsed_state != ValidationRunState::Running
            && record.completed_at.as_deref() != Some(record.updated_at.as_str()))
    {
        issues.push(format!(
            "validation run {run_id} updated/completed timestamps are invalid"
        ));
    }
    if parsed_state == ValidationRunState::Running {
        if record.terminal_hash.is_some() {
            issues.push(format!(
                "running validation run {run_id} exposes a terminal checkpoint"
            ));
        }
    } else if let Some(completed_at) = record.completed_at.as_deref() {
        let expected_terminal_hash = terminal_checkpoint_hash(
            connection,
            &record.run_id,
            &record.state,
            record.error_code.as_deref(),
            record.error_message.as_deref(),
            completed_at,
            record.result_hash.as_deref(),
        )?;
        if record.terminal_hash.as_deref() != Some(expected_terminal_hash.as_str()) {
            issues.push(format!(
                "validation run {run_id} terminal checkpoint differs from immutable history"
            ));
        }
    } else {
        issues.push(format!(
            "terminal validation run {run_id} has no completion timestamp/checkpoint basis"
        ));
    }
    if parsed_state == ValidationRunState::Complete {
        let (Some(result_json), Some(result_hash)) =
            (record.result_json.as_deref(), record.result_hash.as_deref())
        else {
            return Err(validation_read_integrity(
                run_id,
                "complete run has no result evidence",
            ));
        };
        let result_value = parse_unique_json(result_json.as_bytes()).map_err(|error| {
            validation_read_integrity(run_id, format!("result JSON is invalid: {error}"))
        })?;
        if canonical_json_string(&result_value)? != result_json
            || canonical_hash(&result_value)? != result_hash
        {
            issues.push(format!(
                "validation run {run_id} result hash/canonical bytes mismatch"
            ));
        }
        let result: ValidationResult = serde_json::from_value(result_value).map_err(|error| {
            validation_read_integrity(run_id, format!("result contract is invalid: {error}"))
        })?;
        if deterministic_replay {
            let expected = deterministic.take().map(|replay| ValidationResult {
                schema_version: VALIDATION_SCHEMA_VERSION,
                source_hash: record.source_hash,
                profile_id: record.profile_id,
                profile_version: record.profile_version,
                profile_hash: record.profile_hash,
                mapping_id: record.mapping_id,
                mapping_version: record.mapping_version,
                mapping_hash: record.mapping_hash,
                rule_set_hash: record.rule_set_hash,
                engine_version: record.engine_version,
                policy_hash: record.policy_hash,
                rows_processed: replay.rows_processed,
                issue_count: u64::try_from(replay.issue_count).unwrap_or(u64::MAX),
                severity_counts: replay.severity_counts,
                rule_counts: replay.rule_counts,
                entity_counts: replay.entity_counts,
                field_counts: replay.field_counts,
                source_file_counts: replay.source_file_counts,
                issue_hashes: replay.issue_hashes,
            });
            if expected.as_ref() != Some(&result) {
                issues.push(format!(
                    "validation run {run_id} result differs from deterministic source/profile/mapping/rule replay"
                ));
            }
        } else if !validation_result_matches_checkpoint(&result, &record, actual_issue_count)? {
            issues.push(format!(
                "validation run {run_id} result differs from its immutable run checkpoint"
            ));
        }
    } else if record.result_json.is_some() || record.result_hash.is_some() {
        issues.push(format!(
            "non-complete validation run {run_id} exposes result evidence"
        ));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(issues.join("; ")))
    }
}

fn validation_result_matches_checkpoint(
    result: &ValidationResult,
    record: &ValidationIntegrityRecord,
    actual_issue_count: i64,
) -> Result<bool> {
    let issue_count = nonnegative(actual_issue_count, "validation issue count")?;
    let rows_processed = nonnegative(record.rows_processed, "validation rows processed")?;
    let hashes = result.issue_hashes.iter().collect::<HashSet<_>>();
    let valid_hashes = hashes.len() == result.issue_hashes.len()
        && result
            .issue_hashes
            .iter()
            .all(|hash| is_lowercase_sha256(hash));
    let aggregate_totals_match = [
        &result.severity_counts,
        &result.rule_counts,
        &result.entity_counts,
        &result.field_counts,
        &result.source_file_counts,
    ]
    .into_iter()
    .all(|counts| counts.values().copied().sum::<u64>() == issue_count);
    Ok(result.schema_version == VALIDATION_SCHEMA_VERSION
        && result.source_hash == record.source_hash
        && result.profile_id == record.profile_id
        && result.profile_version == record.profile_version
        && result.profile_hash == record.profile_hash
        && result.mapping_id == record.mapping_id
        && result.mapping_version == record.mapping_version
        && result.mapping_hash == record.mapping_hash
        && result.rule_set_hash == record.rule_set_hash
        && result.engine_version == record.engine_version
        && result.policy_hash == record.policy_hash
        && result.rows_processed == rows_processed
        && result.issue_count == issue_count
        && result.issue_hashes.len() == usize::try_from(issue_count).unwrap_or(usize::MAX)
        && valid_hashes
        && aggregate_totals_match)
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validation_read_integrity(run_id: &str, detail: impl std::fmt::Display) -> KernelError {
    KernelError::IntegrityFailed(format!(
        "validation run {run_id} practitioner replay failed: {detail}"
    ))
}

/// Replay every validation run through the same bounded deterministic verifier
/// used by exact run inspection. Oversized rows are diagnosed from `SQLite`
/// lengths before any durable text is materialized.
pub(crate) fn verify_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<(usize, usize)> {
    let mut statement = connection.prepare(
        "SELECT rowid, length(CAST(id AS BLOB)),
                length(CAST(policy_json AS BLOB)),
                COALESCE(length(CAST(result_json AS BLOB)), 0),
                length(CAST(id AS BLOB))
              + length(CAST(identity_hash AS BLOB))
              + length(CAST(job_id AS BLOB))
              + length(CAST(source_hash AS BLOB))
              + length(CAST(profile_id AS BLOB))
              + length(CAST(profile_version AS BLOB))
              + length(CAST(profile_hash AS BLOB))
              + length(CAST(mapping_id AS BLOB))
              + length(CAST(mapping_version AS BLOB))
              + length(CAST(mapping_hash AS BLOB))
              + COALESCE(length(CAST(source_reuse_id AS BLOB)), 0)
              + COALESCE(length(CAST(source_reuse_hash AS BLOB)), 0)
              + length(CAST(rule_set_hash AS BLOB))
              + length(CAST(engine_version AS BLOB))
              + length(CAST(policy_hash AS BLOB))
              + length(CAST(state AS BLOB))
              + COALESCE(length(CAST(result_hash AS BLOB)), 0)
              + COALESCE(length(CAST(terminal_hash AS BLOB)), 0)
              + COALESCE(length(CAST(error_code AS BLOB)), 0)
              + COALESCE(length(CAST(error_message AS BLOB)), 0)
              + length(CAST(created_at AS BLOB))
              + length(CAST(updated_at AS BLOB))
              + COALESCE(length(CAST(completed_at AS BLOB)), 0),
                (SELECT COUNT(*) FROM validation_issues
                 WHERE run_id = validation_runs.id)
         FROM validation_runs ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut checked_runs = 0_usize;
    let mut checked_issues = 0_usize;
    for row in rows {
        let (rowid, run_id_bytes, policy_bytes, result_bytes, metadata_bytes, issue_count) = row?;
        checked_runs = checked_runs.saturating_add(1);
        checked_issues =
            checked_issues.saturating_add(usize::try_from(issue_count).unwrap_or(usize::MAX));
        if usize::try_from(run_id_bytes).unwrap_or(usize::MAX) > 128
            || usize::try_from(policy_bytes).unwrap_or(usize::MAX) > MAXIMUM_POLICY_BYTES
            || usize::try_from(result_bytes).unwrap_or(usize::MAX) > MAXIMUM_RESULT_BYTES
            || usize::try_from(metadata_bytes).unwrap_or(usize::MAX)
                > MAXIMUM_VALIDATION_METADATA_BYTES
        {
            issues.push(format!(
                "validation run row {rowid} exceeds immutable policy/result/metadata bounds"
            ));
            continue;
        }
        let run_id: String = connection.query_row(
            "SELECT id FROM validation_runs WHERE rowid = ?1",
            [rowid],
            |row| row.get(0),
        )?;
        if let Err(error) = verify_validation_run_integrity(connection, &run_id) {
            issues.push(error.to_string());
        }
    }
    Ok((checked_runs, checked_issues))
}
fn replay_issues(
    connection: &Connection,
    run_id: &str,
    source_hash: &str,
    profile_hash: &str,
    mapping_hash: &str,
    qualify_field_counts: bool,
    issues: &mut Vec<String>,
) -> Result<IssueReplay> {
    let (issue_count, issue_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
             length(CAST(issue_id AS BLOB)) + length(CAST(severity AS BLOB))
           + length(CAST(rule_id AS BLOB)) + length(CAST(issue_code AS BLOB))
           + length(CAST(display_message AS BLOB)) + length(CAST(entity_id AS BLOB))
           + length(CAST(field_id AS BLOB)) + length(CAST(source_file AS BLOB))
           + length(CAST(lineage_json AS BLOB)) + length(CAST(issue_hash AS BLOB))
         ), 0)
         FROM validation_issues WHERE run_id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if usize::try_from(issue_count).unwrap_or(usize::MAX) > MAXIMUM_ISSUES
        || usize::try_from(issue_bytes).unwrap_or(usize::MAX) > MAXIMUM_ISSUE_REPLAY_BYTES
    {
        issues.push(format!(
            "validation run {run_id} issue replay exceeds count/byte bounds {MAXIMUM_ISSUES}/{MAXIMUM_ISSUE_REPLAY_BYTES}"
        ));
        return Ok(IssueReplay {
            count: u64::try_from(issue_count).unwrap_or(u64::MAX),
            ..IssueReplay::default()
        });
    }
    let mut statement = connection.prepare(
        "SELECT issue_id, severity, rule_id, issue_code, display_message,
                entity_id, field_id, source_file, source_ordinal,
                line_start, line_end, lineage_json, issue_hash
         FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id",
    )?;
    let rows = statement.query_map([run_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
        ))
    })?;
    let mut replay = IssueReplay::default();
    for issue in rows {
        let issue = issue?;
        replay.count = replay
            .count
            .checked_add(1)
            .ok_or_else(|| KernelError::IntegrityFailed("issue count overflow".to_owned()))?;
        increment(&mut replay.severity_counts, &issue.1)?;
        increment(&mut replay.rule_counts, &issue.2)?;
        increment(&mut replay.entity_counts, &issue.5)?;
        let field_identity = if qualify_field_counts {
            format!("{}.{}", issue.5, issue.6)
        } else {
            issue.6.clone()
        };
        increment(&mut replay.field_counts, &field_identity)?;
        increment(&mut replay.source_file_counts, &issue.7)?;
        replay.hashes.push(issue.12.clone());
        let lineage = match parse_unique_json(issue.11.as_bytes()) {
            Ok(lineage) => lineage,
            Err(error) => {
                issues.push(format!(
                    "validation issue {} lineage JSON is invalid: {error}",
                    issue.0
                ));
                continue;
            }
        };
        let material = issue_material(
            source_hash,
            profile_hash,
            mapping_hash,
            &issue.1,
            &issue.2,
            &issue.3,
            &issue.4,
            &issue.5,
            &issue.6,
            &issue.7,
            nonnegative(issue.8, "issue ordinal")?,
            nonnegative(issue.9, "issue line start")?,
            nonnegative(issue.10, "issue line end")?,
            &lineage,
        );
        let expected_hash = canonical_hash(&material)?;
        if issue.0 != format!("issue-{expected_hash}") || issue.12 != expected_hash {
            issues.push(format!("validation issue {} hash mismatch", issue.0));
        }
    }
    Ok(replay)
}

#[allow(clippy::too_many_arguments)]
fn verify_run_anchors(
    connection: &Connection,
    run_id: &str,
    job_id: &str,
    source_hash: &str,
    profile_id: &str,
    profile_version: &str,
    profile_hash: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    source_reuse_id: Option<&str>,
    source_reuse_hash: Option<&str>,
    rule_set_hash: &str,
    issues: &mut Vec<String>,
) -> Result<Option<Value>> {
    let source_ok: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs
                       WHERE id = ?1 AND state = 'COMPLETE' AND source_hash = ?2)",
        params![job_id, source_hash],
        |row| row.get(0),
    )?;
    if !source_ok {
        issues.push(format!(
            "validation run {run_id} has no matching complete accepted source"
        ));
    }
    let mapping_source: Option<String> = connection
        .query_row(
            "SELECT source_hash FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2
               AND mapping_hash = ?3 AND profile_id = ?4
               AND profile_version = ?5 AND profile_hash = ?6",
            params![
                mapping_id,
                mapping_version,
                mapping_hash,
                profile_id,
                profile_version,
                profile_hash,
            ],
            |row| row.get(0),
        )
        .optional()?;
    let mapping_ok = match (
        mapping_source.as_deref(),
        source_reuse_id,
        source_reuse_hash,
    ) {
        (Some(mapping_source), None, None) => mapping_source == source_hash,
        (Some(mapping_source), Some(reuse_id), Some(reuse_hash))
            if mapping_source != source_hash =>
        {
            match verify_mapping_source_reuse_authority(
                connection,
                reuse_id,
                reuse_hash,
                job_id,
                source_hash,
                profile_id,
                profile_version,
                profile_hash,
                mapping_id,
                mapping_version,
                mapping_hash,
            ) {
                Ok(()) => true,
                Err(error) => {
                    issues.push(format!(
                        "validation run {run_id} source-reuse authority replay failed: {error}"
                    ));
                    false
                }
            }
        }
        _ => false,
    };
    if !mapping_ok {
        issues.push(format!(
            "validation run {run_id} has no matching immutable mapping authority"
        ));
    }
    match load_verified_profile_document(connection, profile_id, profile_version, profile_hash) {
        Ok(profile) => {
            match profile.get("rules") {
                Some(rules) if canonical_hash(rules)? == rule_set_hash => {}
                Some(_) => issues.push(format!(
                    "validation run {run_id} rule-set hash differs from its exact profile"
                )),
                None => issues.push(format!(
                    "validation run {run_id} exact profile has no rule set"
                )),
            }
            Ok(Some(profile))
        }
        Err(error) => {
            issues.push(format!(
                "validation run {run_id} exact profile authority replay failed: {error}"
            ));
            Ok(None)
        }
    }
}

struct ValidationLifecycleAnchor<'a> {
    state: ValidationRunState,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    created_at: &'a str,
    completed_at: Option<&'a str>,
}

struct ValidationEventEvidence {
    state: String,
    error_code: Option<String>,
    detail: Option<String>,
    occurred_at: String,
}

fn load_validation_events(
    connection: &Connection,
    run_id: &str,
) -> Result<Vec<ValidationEventEvidence>> {
    let (event_count, event_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
               length(CAST(state AS BLOB))
             + COALESCE(length(CAST(error_code AS BLOB)), 0)
             + COALESCE(length(CAST(detail AS BLOB)), 0)
             + length(CAST(occurred_at AS BLOB))
         ), 0)
         FROM validation_events WHERE run_id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if !(0..=2).contains(&event_count)
        || usize::try_from(event_bytes).unwrap_or(usize::MAX) > MAXIMUM_VALIDATION_EVENT_BYTES
    {
        return Err(validation_read_integrity(
            run_id,
            "validation lifecycle count/bytes exceed immutable bounds",
        ));
    }
    let mut statement = connection.prepare(
        "SELECT state, error_code, detail, occurred_at
         FROM validation_events WHERE run_id = ?1 ORDER BY sequence",
    )?;
    statement
        .query_map([run_id], |row| {
            Ok(ValidationEventEvidence {
                state: row.get(0)?,
                error_code: row.get(1)?,
                detail: row.get(2)?,
                occurred_at: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn verify_validation_events(
    connection: &Connection,
    run_id: &str,
    anchor: &ValidationLifecycleAnchor<'_>,
    issues: &mut Vec<String>,
) -> Result<()> {
    let events = load_validation_events(connection, run_id)?;
    let expected_count = if anchor.state == ValidationRunState::Running {
        1
    } else {
        2
    };
    if events.len() != expected_count
        || events.first().map(|event| event.state.as_str()) != Some("RUNNING")
        || events.last().map(|event| event.state.as_str()) != Some(anchor.state.as_str())
    {
        issues.push(format!(
            "validation run {run_id} lifecycle events do not match its durable state"
        ));
    }
    if events.first().is_some_and(|event| {
        event.error_code.is_some()
            || event.detail.is_some()
            || chrono::DateTime::parse_from_rfc3339(&event.occurred_at).is_err()
    }) {
        issues.push(format!(
            "validation run {run_id} RUNNING event has invalid error or timestamp evidence"
        ));
    }
    let created_timestamp = chrono::DateTime::parse_from_rfc3339(anchor.created_at).ok();
    if created_timestamp.is_none()
        || events.first().map(|event| event.occurred_at.as_str()) != Some(anchor.created_at)
    {
        issues.push(format!(
            "validation run {run_id} RUNNING event is not bound to its created_at"
        ));
    }
    let run_error_shape_is_valid = if anchor.state == ValidationRunState::Running
        || anchor.state == ValidationRunState::Complete
    {
        anchor.error_code.is_none() && anchor.error_message.is_none()
    } else {
        anchor.error_code.is_some_and(|value| !value.is_empty())
            && anchor.error_message.is_some_and(|value| !value.is_empty())
    };
    if !run_error_shape_is_valid {
        issues.push(format!(
            "validation run {run_id} durable error evidence does not match its state"
        ));
    }
    if anchor.state == ValidationRunState::Running {
        if anchor.completed_at.is_some() {
            issues.push(format!(
                "running validation run {run_id} has a completed_at timestamp"
            ));
        }
        return Ok(());
    }
    let completed_timestamp = anchor
        .completed_at
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
    if completed_timestamp.is_none() {
        issues.push(format!(
            "terminal validation run {run_id} has no valid completed_at timestamp"
        ));
    }
    if completed_timestamp
        .zip(created_timestamp)
        .is_some_and(|(completed, created)| completed < created)
    {
        issues.push(format!(
            "terminal validation run {run_id} completed before it was created"
        ));
    }
    if let Some(terminal) = events.get(1) {
        verify_terminal_validation_event(run_id, anchor, terminal, issues);
    }
    Ok(())
}

fn verify_terminal_validation_event(
    run_id: &str,
    anchor: &ValidationLifecycleAnchor<'_>,
    terminal: &ValidationEventEvidence,
    issues: &mut Vec<String>,
) {
    let error_shape_is_valid = if anchor.state == ValidationRunState::Complete {
        terminal.error_code.is_none() && terminal.detail.is_none()
    } else {
        terminal
            .error_code
            .as_ref()
            .is_some_and(|value| !value.is_empty())
            && terminal
                .detail
                .as_ref()
                .is_some_and(|value| !value.is_empty())
    };
    if !error_shape_is_valid || chrono::DateTime::parse_from_rfc3339(&terminal.occurred_at).is_err()
    {
        issues.push(format!(
            "validation run {run_id} terminal event has invalid error or timestamp evidence"
        ));
    }
    if terminal.error_code.as_deref() != anchor.error_code
        || terminal.detail.as_deref() != anchor.error_message
    {
        issues.push(format!(
            "validation run {run_id} terminal event error evidence differs from its durable run"
        ));
    }
    if Some(terminal.occurred_at.as_str()) != anchor.completed_at {
        issues.push(format!(
            "validation run {run_id} terminal event is not bound to its completed_at"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{
        MappingProfileIdentity, MappingSourceIdentity, RelationshipFieldPair,
        SourceEntityAssignment,
    };

    fn policy(maximum_keys: usize) -> ValidationPolicy {
        ValidationPolicy {
            schema_version: VALIDATION_SCHEMA_VERSION,
            row_batch_size: 10,
            maximum_issues: 100,
            maximum_pattern_bytes: 512,
            maximum_total_pattern_bytes: 4096,
            maximum_pattern_program_bytes: 8 * 1024 * 1024,
            maximum_lookup_entries: 100,
            maximum_lookup_bytes: 1024 * 1024,
            maximum_unique_keys: maximum_keys,
            maximum_unique_key_bytes: 1024 * 1024,
        }
    }

    fn mapping(cardinality: &str) -> MappingDocument {
        MappingDocument {
            schema_version: 1,
            mapping_id: "synthetic-sales-document".to_owned(),
            version: "1.0.0".to_owned(),
            profile: MappingProfileIdentity {
                profile_id: "synthetic.sales-document.readiness".to_owned(),
                version: "1.0.0".to_owned(),
                sha256: "a".repeat(64),
            },
            source: MappingSourceIdentity {
                package_sha256: "b".repeat(64),
                source_files: vec!["deal_book.csv".to_owned(), "deal_detail.csv".to_owned()],
            },
            root_entity_id: "SalesDocument".to_owned(),
            source_entity_assignments: vec![
                SourceEntityAssignment {
                    source_file: "deal_book.csv".to_owned(),
                    entity_id: "SalesDocument".to_owned(),
                },
                SourceEntityAssignment {
                    source_file: "deal_detail.csv".to_owned(),
                    entity_id: "SalesDocumentItem".to_owned(),
                },
            ],
            bindings: Vec::new(),
            reference_tables: Vec::new(),
            relationship_bindings: vec![MappingRelationshipBinding {
                relationship_id: "sales-document.items".to_owned(),
                from_entity_id: "SalesDocument".to_owned(),
                to_entity_id: "SalesDocumentItem".to_owned(),
                cardinality: cardinality.to_owned(),
                key_pairs: vec![RelationshipFieldPair {
                    from_field_id: "sourceDocumentId".to_owned(),
                    to_field_id: "parentDocumentId".to_owned(),
                }],
            }],
            author: None,
            reviewer: None,
        }
    }

    fn row(
        entity_id: &str,
        source_file: &str,
        source_ordinal: u64,
        field_id: &str,
        value: Option<&str>,
    ) -> ProjectedRow {
        ProjectedRow {
            entity_id: entity_id.to_owned(),
            source_file: source_file.to_owned(),
            source_ordinal,
            line_start: source_ordinal + 1,
            line_end: source_ordinal + 1,
            source_values: Vec::new(),
            canonical_values: BTreeMap::from([(field_id.to_owned(), value.map(str::to_owned))]),
            lineage: BTreeMap::from([(
                field_id.to_owned(),
                json!({
                    "bindingId": format!("binding-{entity_id}-{field_id}"),
                    "kind": "direct",
                    "sourceFile": source_file,
                    "sourceColumn": field_id,
                    "sourceColumnOrdinal": 0,
                    "sourceValueIncluded": false
                }),
            )]),
        }
    }

    #[test]
    fn rules_are_entity_filtered_even_when_field_names_collide() {
        let mapping = mapping("1..*");
        let rules = json!([
            {
                "ruleId": "root-required",
                "severity": "error",
                "kind": "required",
                "target": "SalesDocument.sharedId",
                "parameters": {},
                "message": "Root value is required."
            },
            {
                "ruleId": "child-required",
                "severity": "error",
                "kind": "required",
                "target": "SalesDocumentItem.sharedId",
                "parameters": {},
                "message": "Child value is required."
            }
        ]);
        let compiled = compile_rules(&rules, &mapping, &policy(100)).expect("compile rules");
        let root = row("SalesDocument", "deal_book.csv", 1, "sharedId", None);
        let child = row("SalesDocumentItem", "deal_detail.csv", 1, "sharedId", None);
        let mut root_accumulator = ValidationAccumulator::default();
        let root_codes = compiled
            .iter()
            .map(|rule| {
                evaluate_rule(rule, &root, &mapping, &mut root_accumulator, &policy(100))
                    .expect("evaluate root")
            })
            .collect::<Vec<_>>();
        assert_eq!(root_codes, vec![Some("REQUIRED_MISSING"), None]);

        let mut child_accumulator = ValidationAccumulator::default();
        let child_codes = compiled
            .iter()
            .map(|rule| {
                evaluate_rule(rule, &child, &mapping, &mut child_accumulator, &policy(100))
                    .expect("evaluate child")
            })
            .collect::<Vec<_>>();
        assert_eq!(child_codes, vec![None, Some("REQUIRED_MISSING")]);
    }

    #[test]
    fn explicit_entity_field_aggregates_do_not_collide() {
        let mapping = mapping("1..*");
        let mut accumulator = ValidationAccumulator::for_mapping(&mapping);
        for (ordinal, entity_id) in ["SalesDocument", "SalesDocumentItem"]
            .into_iter()
            .enumerate()
        {
            accumulator
                .push_issue(
                    IssueEvidence {
                        issue_id: format!("issue-{ordinal}"),
                        issue_hash: format!("{ordinal:064x}"),
                        severity: "ERROR".to_owned(),
                        rule_id: format!("rule-{ordinal}"),
                        issue_code: "REQUIRED_MISSING".to_owned(),
                        display_message: "Value is required.".to_owned(),
                        entity_id: entity_id.to_owned(),
                        field_id: "sharedId".to_owned(),
                        source_file: format!("source-{ordinal}.csv"),
                        source_ordinal: u64::try_from(ordinal).expect("bounded ordinal"),
                        line_start: 1,
                        line_end: 1,
                        lineage_json: "{}".to_owned(),
                    },
                    10,
                )
                .expect("bounded issue");
        }
        assert_eq!(
            accumulator.field_counts,
            BTreeMap::from([
                ("SalesDocument.sharedId".to_owned(), 1),
                ("SalesDocumentItem.sharedId".to_owned(), 1),
            ])
        );

        let mut legacy = mapping;
        legacy.source_entity_assignments.clear();
        legacy.relationship_bindings.clear();
        let mut legacy_accumulator = ValidationAccumulator::for_mapping(&legacy);
        legacy_accumulator
            .push_issue(accumulator.pending_issues[0].clone(), 10)
            .expect("legacy issue");
        assert_eq!(
            legacy_accumulator.field_counts,
            BTreeMap::from([("sharedId".to_owned(), 1)])
        );
    }

    #[test]
    fn unique_rules_keep_second_and_later_duplicate_emission() {
        let mapping = mapping("1..*");
        let rule = CompiledRule {
            rule_id: "root-unique".to_owned(),
            severity: "ERROR".to_owned(),
            entity_id: "SalesDocument".to_owned(),
            field_id: "sourceDocumentId".to_owned(),
            message: "Header key must be unique.".to_owned(),
            kind: CompiledKind::Unique(vec!["sourceDocumentId".to_owned()]),
        };
        let repeated = row(
            "SalesDocument",
            "deal_book.csv",
            1,
            "sourceDocumentId",
            Some("RECEIVED-DUPLICATE-KEY"),
        );
        let mut accumulator = ValidationAccumulator::default();
        assert_eq!(
            evaluate_rule(&rule, &repeated, &mapping, &mut accumulator, &policy(100))
                .expect("first key"),
            None
        );
        assert_eq!(
            evaluate_rule(&rule, &repeated, &mapping, &mut accumulator, &policy(100))
                .expect("second key"),
            Some("UNIQUE_DUPLICATE")
        );
        assert_eq!(
            evaluate_rule(&rule, &repeated, &mapping, &mut accumulator, &policy(100))
                .expect("third key"),
            Some("UNIQUE_DUPLICATE")
        );

        let whitespace = row(
            "SalesDocument",
            "deal_book.csv",
            2,
            "sourceDocumentId",
            Some("   "),
        );
        assert_eq!(
            evaluate_rule(&rule, &whitespace, &mapping, &mut accumulator, &policy(100))
                .expect("first exact whitespace key"),
            None
        );
        assert_eq!(
            evaluate_rule(&rule, &whitespace, &mapping, &mut accumulator, &policy(100))
                .expect("second exact whitespace key"),
            Some("UNIQUE_DUPLICATE"),
            "exact nonempty whitespace preserves established UNIQUE semantics"
        );
    }

    #[test]
    fn one_to_many_relationships_emit_bounded_orphan_and_minimum_child_evidence() {
        let mapping = mapping("1..*");
        let relationships = compile_relationships(&mapping).expect("compile relationship");
        let rows = vec![
            row(
                "SalesDocument",
                "deal_book.csv",
                1,
                "sourceDocumentId",
                Some("RECEIVED-PARENT-WITH-CHILD"),
            ),
            row(
                "SalesDocument",
                "deal_book.csv",
                2,
                "sourceDocumentId",
                Some("RECEIVED-PARENT-WITHOUT-CHILD"),
            ),
            row(
                "SalesDocumentItem",
                "deal_detail.csv",
                1,
                "parentDocumentId",
                Some("RECEIVED-PARENT-WITH-CHILD"),
            ),
            row(
                "SalesDocumentItem",
                "deal_detail.csv",
                2,
                "parentDocumentId",
                Some("RECEIVED-ORPHAN-PARENT"),
            ),
        ];
        let mut states = vec![RelationshipKeyState::default(); relationships.len()];
        let mut accumulator = ValidationAccumulator::default();
        for projected in &rows {
            index_relationship_keys(
                projected,
                &relationships,
                &mut states,
                &mut accumulator,
                &policy(100),
            )
            .expect("index relationship key");
        }
        for projected in &rows {
            evaluate_relationship_issues_for_row(
                &mapping.source.package_sha256,
                &"c".repeat(64),
                &mapping,
                projected,
                &relationships,
                &states,
                &mut accumulator,
                &policy(100),
            )
            .expect("evaluate relationship");
        }
        let mut codes = accumulator
            .pending_issues
            .iter()
            .map(|issue| issue.issue_code.as_str())
            .collect::<Vec<_>>();
        codes.sort_unstable();
        assert_eq!(
            codes,
            vec![
                "RELATIONSHIP_MINIMUM_CHILD_MISSING",
                "RELATIONSHIP_PARENT_NOT_FOUND"
            ]
        );
        for issue in &accumulator.pending_issues {
            assert!(!issue.lineage_json.contains("RECEIVED-"));
            assert!(!issue.lineage_json.contains("relationshipKeyHash"));
            assert!(issue.lineage_json.contains("sales-document.items"));
            assert!(issue.lineage_json.contains("\"sourceValueIncluded\":false"));
        }
    }

    #[test]
    fn zero_to_many_relationships_do_not_claim_a_minimum_child() {
        let mapping = mapping("0..*");
        let relationships = compile_relationships(&mapping).expect("compile relationship");
        let parent = row(
            "SalesDocument",
            "deal_book.csv",
            1,
            "sourceDocumentId",
            Some("RECEIVED-PARENT-WITHOUT-CHILD"),
        );
        let mut states = vec![RelationshipKeyState::default(); relationships.len()];
        let mut accumulator = ValidationAccumulator::default();
        index_relationship_keys(
            &parent,
            &relationships,
            &mut states,
            &mut accumulator,
            &policy(100),
        )
        .expect("index parent key");
        evaluate_relationship_issues_for_row(
            &mapping.source.package_sha256,
            &"c".repeat(64),
            &mapping,
            &parent,
            &relationships,
            &states,
            &mut accumulator,
            &policy(100),
        )
        .expect("evaluate optional relationship");
        assert!(accumulator.pending_issues.is_empty());
    }

    #[test]
    fn relationship_key_indexes_share_the_configured_retained_key_bound() {
        let mapping = mapping("1..*");
        let relationships = compile_relationships(&mapping).expect("compile relationship");
        let parent = row(
            "SalesDocument",
            "deal_book.csv",
            1,
            "sourceDocumentId",
            Some("RECEIVED-BOUND-KEY"),
        );
        let child = row(
            "SalesDocumentItem",
            "deal_detail.csv",
            1,
            "parentDocumentId",
            Some("RECEIVED-BOUND-KEY"),
        );
        let mut states = vec![RelationshipKeyState::default(); relationships.len()];
        let mut accumulator = ValidationAccumulator::default();
        index_relationship_keys(
            &parent,
            &relationships,
            &mut states,
            &mut accumulator,
            &policy(1),
        )
        .expect("first retained relationship key fits");
        let error = index_relationship_keys(
            &child,
            &relationships,
            &mut states,
            &mut accumulator,
            &policy(1),
        )
        .expect_err("second retained relationship key exceeds the shared bound");
        assert!(matches!(error, KernelError::ValidationBound(_)));
    }

    #[test]
    fn revised_source_validation_identity_binds_reuse_without_changing_legacy_contract() {
        let source_hash = "1".repeat(64);
        let profile_hash = "2".repeat(64);
        let mapping_hash = "3".repeat(64);
        let rule_set_hash = "4".repeat(64);
        let policy_hash = "5".repeat(64);
        let legacy = validation_identity_hash(
            &source_hash,
            &profile_hash,
            &mapping_hash,
            &rule_set_hash,
            &policy_hash,
            None,
        )
        .expect("legacy validation identity");
        let expected_legacy = canonical_hash(&json!({
            "contract": VALIDATION_ENGINE_VERSION,
            "sourceHash": source_hash,
            "profileHash": profile_hash,
            "mappingHash": mapping_hash,
            "ruleSetHash": rule_set_hash,
            "policyHash": policy_hash,
        }))
        .expect("expected legacy identity");
        assert_eq!(legacy, expected_legacy);

        let first = validation_identity_hash(
            &source_hash,
            &profile_hash,
            &mapping_hash,
            &rule_set_hash,
            &policy_hash,
            Some(&"6".repeat(64)),
        )
        .expect("first reuse identity");
        let second = validation_identity_hash(
            &source_hash,
            &profile_hash,
            &mapping_hash,
            &rule_set_hash,
            &policy_hash,
            Some(&"7".repeat(64)),
        )
        .expect("second reuse identity");
        assert_ne!(legacy, first);
        assert_ne!(first, second);
    }

    #[test]
    fn comparison_record_locator_qualifies_keys_and_retains_full_row_fallback() {
        let mut values = BTreeMap::new();
        values.insert("quantity".to_owned(), Some("2".to_owned()));
        values.insert("parentDocumentId".to_owned(), Some("SD-1001".to_owned()));
        values.insert("sourceItemId".to_owned(), None);
        let projected = ProjectedRow {
            entity_id: "SalesDocumentItem".to_owned(),
            source_file: "deal_detail.csv".to_owned(),
            source_ordinal: 1,
            line_start: 2,
            line_end: 2,
            source_values: Vec::new(),
            canonical_values: values,
            lineage: BTreeMap::new(),
        };
        let keys = BTreeMap::from([(
            "SalesDocumentItem".to_owned(),
            vec!["parentDocumentId".to_owned(), "sourceItemId".to_owned()],
        )]);
        let locator =
            comparison_record_locator(&projected, &keys).expect("comparison record locator");
        assert_eq!(
            locator
                .declared_key
                .iter()
                .map(|field| field.field_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "SalesDocumentItem.parentDocumentId",
                "SalesDocumentItem.sourceItemId"
            ]
        );
        assert!(locator.declared_key[1].value.is_none());
        assert_eq!(
            locator
                .projected_row
                .iter()
                .map(|field| field.field_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "SalesDocumentItem.parentDocumentId",
                "SalesDocumentItem.quantity",
                "SalesDocumentItem.sourceItemId"
            ]
        );
    }

    #[test]
    fn relationship_comparison_rules_keep_parent_and_child_roles_distinct() {
        let mapping = mapping("1..*");
        let relationships = compile_relationships(&mapping).expect("compile relationship");
        let rules =
            relationship_comparison_rules(&relationships).expect("relationship comparison rules");
        let minimum = rules
            .get("relationship.sales-document.items.minimum-child")
            .expect("minimum-child rule");
        let orphan = rules
            .get("relationship.sales-document.items.parent-exists")
            .expect("parent-exists rule");
        assert_eq!(minimum.role, RelationshipRole::Parent);
        assert_eq!(minimum.entity_id, "SalesDocument");
        assert_eq!(orphan.role, RelationshipRole::Child);
        assert_eq!(orphan.entity_id, "SalesDocumentItem");
    }

    #[test]
    fn source_structure_rebuild_is_sorted_and_value_free() {
        let connection = Connection::open_in_memory().expect("open SQLite");
        connection
            .execute_batch(
                "CREATE TABLE csv_files (
                     job_id TEXT NOT NULL,
                     path TEXT NOT NULL,
                     header_json TEXT
                 );",
            )
            .expect("create CSV evidence");
        connection
            .execute(
                "INSERT INTO csv_files (job_id, path, header_json)
                 VALUES ('job-2', 'deal_detail.csv', ?1)",
                [r#"["case_link","seq_ref"]"#],
            )
            .expect("insert item header");
        connection
            .execute(
                "INSERT INTO csv_files (job_id, path, header_json)
                 VALUES ('job-2', 'deal_book.csv', ?1)",
                [r#"["case_ref","party_ref"]"#],
            )
            .expect("insert header");
        let structure = rebuild_source_structure(&connection, "job-2", &mapping("1..*"))
            .expect("rebuild source structure");
        assert_eq!(
            structure,
            canonical_json_string(&json!({
                "schemaVersion": "migration-stream/source-structure/v1",
                "files": [
                    {
                        "path": "deal_book.csv",
                        "entityId": "SalesDocument",
                        "headers": ["case_ref", "party_ref"],
                    },
                    {
                        "path": "deal_detail.csv",
                        "entityId": "SalesDocumentItem",
                        "headers": ["case_link", "seq_ref"],
                    }
                ],
            }))
            .expect("expected structure")
        );
        assert!(!structure.contains("SD-"));
    }
}

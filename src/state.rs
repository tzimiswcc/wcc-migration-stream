//! Durable workspace state and recovery.
//!
//! `SQLite` is the authority for intake state. Filesystem paths stored here are
//! evidence locations only; this module never infers profile approval or target
//! compatibility from technical metadata.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, Read},
    path::Component,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(windows)]
use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    durability::TOMBSTONE_EXTENSION,
    edmx::{TechnicalCatalogueArtifact, TechnicalCatalogueStatus},
    intake::{inspect_stored_package_file, verify_ingested_package},
    policy::IntakePolicy,
};

pub(crate) const DATABASE_FILENAME: &str = "state.sqlite3";
pub(crate) const PACKAGES_DIRECTORY: &str = "packages";
const LOCK_FILENAME: &str = ".migration-stream.lock";
pub const SCHEMA_VERSION: i64 = 14;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CleanupNamespace {
    Packages,
    Metadata,
    Catalogues,
}

impl CleanupNamespace {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Packages => "packages",
            Self::Metadata => "metadata",
            Self::Catalogues => "catalogues",
        }
    }

    #[cfg(windows)]
    const fn job_id(self, attempt_id: &str) -> Option<&str> {
        match self {
            Self::Packages => Some(attempt_id),
            Self::Metadata | Self::Catalogues => None,
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "packages" => Ok(Self::Packages),
            "metadata" => Ok(Self::Metadata),
            "catalogues" => Ok(Self::Catalogues),
            other => Err(KernelError::Reconciliation(format!(
                "partial cleanup has unknown namespace {other:?}"
            ))),
        }
    }
}

/// Explicit durable lifecycle for an intake job.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    Spooling,
    Inspecting,
    Ingesting,
    Reconciling,
    Complete,
    Failed,
    Interrupted,
}

impl JobState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Spooling => "SPOOLING",
            Self::Inspecting => "INSPECTING",
            Self::Ingesting => "INGESTING",
            Self::Reconciling => "RECONCILING",
            Self::Complete => "COMPLETE",
            Self::Failed => "FAILED",
            Self::Interrupted => "INTERRUPTED",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "SPOOLING" => Ok(Self::Spooling),
            "INSPECTING" => Ok(Self::Inspecting),
            "INGESTING" => Ok(Self::Ingesting),
            "RECONCILING" => Ok(Self::Reconciling),
            "COMPLETE" => Ok(Self::Complete),
            "FAILED" => Ok(Self::Failed),
            "INTERRUPTED" => Ok(Self::Interrupted),
            other => Err(KernelError::Reconciliation(format!(
                "database contains unknown job state {other:?}"
            ))),
        }
    }

    const fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Spooling, Self::Inspecting)
                | (Self::Inspecting, Self::Ingesting)
                | (Self::Ingesting, Self::Reconciling)
                | (Self::Reconciling, Self::Complete)
                | (
                    Self::Spooling | Self::Inspecting | Self::Ingesting | Self::Reconciling,
                    Self::Failed | Self::Interrupted
                )
        )
    }

    const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Interrupted)
    }
}

/// Durable, JSON-friendly status for one intake job.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobStatus {
    pub id: String,
    pub state: JobState,
    pub source_name: String,
    pub source_hash: Option<String>,
    pub source_size: Option<u64>,
    pub stored_archive: Option<String>,
    pub policy_json: String,
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub declared_buffer_bytes: Option<u64>,
    #[serde(default)]
    pub peak_database_bytes: Option<u64>,
    #[serde(default)]
    pub peak_wal_bytes: Option<u64>,
    #[serde(default)]
    pub peak_shm_bytes: Option<u64>,
}

/// Snapshot of all durable intake jobs in a workspace.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceStatus {
    pub workspace: PathBuf,
    pub active_jobs: usize,
    pub complete_jobs: usize,
    pub failed_jobs: usize,
    pub interrupted_jobs: usize,
    pub jobs: Vec<JobStatus>,
}

/// Result of converting abandoned nonterminal work into explicit evidence.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecoveryReport {
    pub workspace: PathBuf,
    pub recovered_jobs: usize,
    #[serde(default)]
    pub recovered_profiling_runs: usize,
    #[serde(default)]
    pub recovered_validation_runs: usize,
    #[serde(default)]
    pub recovered_decision_attempts: usize,
    #[serde(default)]
    pub recovered_reconciliation_attempts: usize,
    /// Live `.part` names removed from the recovery namespace. On Windows this
    /// counts successful write-through transitions into inert tombstones.
    pub removed_partial_files: usize,
    #[serde(default)]
    pub quarantined_partial_files: usize,
    #[serde(default)]
    pub deleted_tombstones: usize,
    #[serde(default)]
    pub cleanup_pending: Vec<CleanupPending>,
}

/// Durable warning for an inert tombstone whose physical deletion is pending.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CleanupPending {
    pub cleanup_id: String,
    pub namespace: String,
    pub tombstone: String,
    pub attempts: u64,
    pub diagnostic: String,
}

/// Read-only `SQLite`, package-hash, and reconciliation verification result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IntegrityReport {
    pub workspace: PathBuf,
    pub ok: bool,
    pub sqlite_ok: bool,
    pub checked_jobs: usize,
    pub checked_packages: usize,
    pub checked_metadata_snapshots: usize,
    pub checked_catalogues: usize,
    #[serde(default)]
    pub checked_profiling_runs: usize,
    #[serde(default)]
    pub checked_partial_cleanups: usize,
    #[serde(default)]
    pub checked_profile_versions: usize,
    #[serde(default)]
    pub checked_profile_approvals: usize,
    #[serde(default)]
    pub checked_mappings: usize,
    #[serde(default)]
    pub checked_validation_runs: usize,
    #[serde(default)]
    pub checked_validation_issues: usize,
    #[serde(default)]
    pub checked_engagement_tour_heads: usize,
    #[serde(default)]
    pub checked_engagement_tour_attempts: usize,
    #[serde(default)]
    pub checked_engagement_tour_command_attempts: usize,
    #[serde(default)]
    pub checked_mapping_reuse_authorities: usize,
    #[serde(default)]
    pub checked_comparison_namespaces: usize,
    #[serde(default)]
    pub checked_tour_finding_sets: usize,
    #[serde(default)]
    pub checked_tour_finding_members: usize,
    #[serde(default)]
    pub checked_validation_comparisons: usize,
    #[serde(default)]
    pub checked_validation_comparison_items: usize,
    #[serde(default)]
    pub checked_mapping_authorities: usize,
    #[serde(default)]
    pub checked_remediation_pack_attempts: usize,
    #[serde(default)]
    pub checked_remediation_packs: usize,
    #[serde(default)]
    pub checked_remediation_pack_members: usize,
    #[serde(default)]
    pub checked_remediation_issue_members: usize,
    #[serde(default)]
    pub checked_object_workspaces: usize,
    #[serde(default)]
    pub checked_decision_attempts: usize,
    #[serde(default)]
    pub checked_decision_attempt_events: usize,
    #[serde(default)]
    pub checked_correction_overlays: usize,
    #[serde(default)]
    pub checked_reevaluation_runs: usize,
    #[serde(default)]
    pub checked_reevaluation_events: usize,
    #[serde(default)]
    pub checked_reevaluation_issues: usize,
    #[serde(default)]
    pub checked_issue_decisions: usize,
    #[serde(default)]
    pub checked_correction_effectivations: usize,
    #[serde(default)]
    pub checked_active_decision_heads: usize,
    #[serde(default)]
    pub checked_inert_overlays: usize,
    #[serde(default)]
    pub checked_incomplete_decision_attempts: usize,
    #[serde(default)]
    pub checked_reconciliation_approval: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub issues: Vec<String>,
}

/// Immutable technical-metadata snapshot fields registered as evidence.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MetadataSnapshotRegistration {
    pub(crate) id: String,
    pub(crate) artifact_name: String,
    pub(crate) immutable_path: String,
    pub(crate) sha256: String,
    pub(crate) byte_size: u64,
    pub(crate) system_context: Option<String>,
    pub(crate) client_context: Option<String>,
    pub(crate) release_context: Option<String>,
    pub(crate) imported_at: String,
}

/// Deterministic parser-run evidence associated with one immutable snapshot.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ParserRunRegistration {
    pub(crate) parser_name: String,
    pub(crate) parser_version: String,
    pub(crate) options: serde_json::Value,
    pub(crate) outcome: String,
    pub(crate) catalogue_path: Option<String>,
    pub(crate) catalogue_sha256: Option<String>,
    pub(crate) evidence: serde_json::Value,
    pub(crate) started_at: String,
    pub(crate) completed_at: String,
}

/// One atomic snapshot-and-parser-evidence registration request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MetadataImportRegistration {
    pub(crate) snapshot: MetadataSnapshotRegistration,
    pub(crate) parser_run: ParserRunRegistration,
}

pub(crate) struct WorkspaceWriter {
    pub(crate) connection: Connection,
    pub(crate) root: PathBuf,
    authority: Arc<WorkspaceWriterAuthority>,
}

pub(crate) struct WorkspaceLock {
    root: PathBuf,
    root_handle: File,
    lock_path: PathBuf,
    lock: File,
}

struct WorkspaceWriterAuthority {
    root: PathBuf,
    root_handle: File,
    lock_path: PathBuf,
    lock_handle: File,
    database_path: PathBuf,
    database_handle: File,
    descriptor: Option<(File, String)>,
}

pub(crate) struct NewJob<'a> {
    pub id: &'a str,
    pub source_name: &'a str,
    pub source_path: &'a str,
    pub source_size: Option<u64>,
    pub policy_json: &'a str,
}

pub(crate) struct JobExecutionMetrics {
    pub(crate) elapsed_ms: u64,
    pub(crate) peak_rss_bytes: Option<u64>,
    pub(crate) declared_buffer_bytes: u64,
    pub(crate) peak_database_bytes: Option<u64>,
    pub(crate) peak_wal_bytes: Option<u64>,
    pub(crate) peak_shm_bytes: Option<u64>,
}

struct PersistedSnapshot {
    artifact_name: String,
    immutable_path: String,
    sha256: String,
    byte_size: i64,
    system_context: Option<String>,
    client_context: Option<String>,
    release_context: Option<String>,
    imported_at: String,
}

#[derive(Clone)]
struct SnapshotEvidence {
    artifact_name: String,
    sha256: String,
    byte_size: i64,
    system: Option<String>,
    client: Option<String>,
    release: Option<String>,
}

/// Register metadata while reusing an already-held workspace writer lock.
///
/// This transaction-only function lets the crate-level metadata orchestrator
/// preserve immutable files and register their evidence under one advisory-lock
/// lifetime.
///
/// # Errors
///
/// Returns an error for invalid hashes, numeric overflow, JSON serialization,
/// or an atomic `SQLite` transaction failure.
#[allow(clippy::too_many_lines)]
pub(crate) fn register_metadata_import_locked(
    writer: &WorkspaceWriter,
    registration: &MetadataImportRegistration,
) -> Result<String> {
    let snapshot = &registration.snapshot;
    let parser = &registration.parser_run;
    if !is_sha256(&snapshot.sha256) {
        return Err(KernelError::Reconciliation(
            "metadata snapshot SHA-256 must be 64 hexadecimal characters".to_owned(),
        ));
    }
    if parser
        .catalogue_sha256
        .as_deref()
        .is_some_and(|hash| !is_sha256(hash))
    {
        return Err(KernelError::Reconciliation(
            "catalogue SHA-256 must be 64 hexadecimal characters".to_owned(),
        ));
    }
    validate_metadata_registration(writer, registration)?;
    let byte_size = sqlite_integer(snapshot.byte_size, "metadata snapshot byte size")?;
    let options = serde_json::to_string(&parser.options)?;
    let evidence = serde_json::to_string(&parser.evidence)?;
    let transaction = writer.connection.unchecked_transaction()?;

    transaction.execute(
        "INSERT INTO metadata_snapshots (
             id, artifact_name, immutable_path, sha256, kind, byte_size,
             system_context, client_context, release_context, imported_at
         ) VALUES (?1, ?2, ?3, ?4, 'odata_edmx', ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO NOTHING",
        params![
            snapshot.id,
            snapshot.artifact_name,
            snapshot.immutable_path,
            snapshot.sha256,
            byte_size,
            snapshot.system_context,
            snapshot.client_context,
            snapshot.release_context,
            snapshot.imported_at,
        ],
    )?;
    let persisted_snapshot = transaction.query_row(
        "SELECT artifact_name, immutable_path, sha256, byte_size,
                    system_context, client_context, release_context, imported_at
             FROM metadata_snapshots WHERE id = ?1",
        [&snapshot.id],
        |row| {
            Ok(PersistedSnapshot {
                artifact_name: row.get(0)?,
                immutable_path: row.get(1)?,
                sha256: row.get(2)?,
                byte_size: row.get(3)?,
                system_context: row.get(4)?,
                client_context: row.get(5)?,
                release_context: row.get(6)?,
                imported_at: row.get(7)?,
            })
        },
    )?;
    if persisted_snapshot.artifact_name != snapshot.artifact_name
        || persisted_snapshot.immutable_path != snapshot.immutable_path
        || persisted_snapshot.sha256 != snapshot.sha256
        || persisted_snapshot.byte_size != byte_size
        || persisted_snapshot.system_context != snapshot.system_context
        || persisted_snapshot.client_context != snapshot.client_context
        || persisted_snapshot.release_context != snapshot.release_context
    {
        return Err(KernelError::Reconciliation(format!(
            "metadata snapshot identity {} conflicts with existing evidence",
            snapshot.id
        )));
    }
    let existing_parser: Option<(String, String, Option<String>, String)> = transaction
        .query_row(
            "SELECT options_json, outcome, catalogue_path, evidence_json
             FROM parser_runs
             WHERE snapshot_id = ?1 AND parser_name = ?2 AND parser_version = ?3
                   AND catalogue_sha256 = ?4
             ORDER BY started_at
             LIMIT 1",
            params![
                snapshot.id,
                parser.parser_name,
                parser.parser_version,
                parser.catalogue_sha256,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some(existing) = existing_parser {
        if existing.0 != options
            || existing.1 != parser.outcome
            || existing.2 != parser.catalogue_path
            || existing.3 != evidence
        {
            return Err(KernelError::Reconciliation(format!(
                "parser evidence for snapshot {} conflicts with an existing run",
                snapshot.id
            )));
        }
    } else {
        transaction.execute(
            "INSERT INTO parser_runs (
                 snapshot_id, parser_name, parser_version, options_json, outcome,
                 catalogue_path, catalogue_sha256, evidence_json, started_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                snapshot.id,
                parser.parser_name,
                parser.parser_version,
                options,
                parser.outcome,
                parser.catalogue_path,
                parser.catalogue_sha256,
                evidence,
                parser.started_at,
                parser.completed_at,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(persisted_snapshot.imported_at)
}

fn validate_metadata_registration(
    writer: &WorkspaceWriter,
    registration: &MetadataImportRegistration,
) -> Result<()> {
    let snapshot = &registration.snapshot;
    let parser = &registration.parser_run;
    if parser.outcome != "SUCCEEDED_GENERATED_DRAFT" {
        return Err(KernelError::GeneratedDraftAuthority);
    }
    let expected_snapshot_path = format!("metadata/{}.edmx", snapshot.sha256);
    if snapshot.immutable_path != expected_snapshot_path {
        return Err(KernelError::Reconciliation(format!(
            "metadata snapshot path must be the content-addressed path {expected_snapshot_path:?}"
        )));
    }
    let snapshot_path = safe_evidence_file(&writer.root, &snapshot.immutable_path)
        .map_err(KernelError::Reconciliation)?;
    let (snapshot_hash, snapshot_size, snapshot_readonly) =
        hash_evidence_file(&snapshot_path).map_err(KernelError::Reconciliation)?;
    if !snapshot_readonly || snapshot_hash != snapshot.sha256 || snapshot_size != snapshot.byte_size
    {
        return Err(KernelError::Reconciliation(
            "metadata snapshot file does not match its read-only registration evidence".to_owned(),
        ));
    }
    let (Some(catalogue_path), Some(catalogue_hash)) =
        (&parser.catalogue_path, &parser.catalogue_sha256)
    else {
        return Err(KernelError::Reconciliation(
            "successful generated-draft parser run requires catalogue evidence".to_owned(),
        ));
    };
    let expected_catalogue_path = format!("catalogues/{catalogue_hash}.json");
    if catalogue_path != &expected_catalogue_path {
        return Err(KernelError::Reconciliation(format!(
            "generated catalogue path must be the content-addressed path {expected_catalogue_path:?}"
        )));
    }
    let catalogue_path =
        safe_evidence_file(&writer.root, catalogue_path).map_err(KernelError::Reconciliation)?;
    let (catalogue_bytes, actual_catalogue_hash, _, catalogue_readonly) =
        read_evidence_file(&catalogue_path).map_err(KernelError::Reconciliation)?;
    if !catalogue_readonly || &actual_catalogue_hash != catalogue_hash {
        return Err(KernelError::Reconciliation(
            "generated catalogue file does not match its read-only registration evidence"
                .to_owned(),
        ));
    }
    let artifact: TechnicalCatalogueArtifact = serde_json::from_slice(&catalogue_bytes)?;
    artifact.verify_contract()?;
    let expected_snapshot_id = format!(
        "edmx-{}",
        crate::canonical::lower_hex(Sha256::digest(serde_json::to_vec(
            &artifact.catalogue.snapshot
        )?))
    );
    if snapshot.id != expected_snapshot_id {
        return Err(KernelError::Reconciliation(format!(
            "metadata snapshot ID must be deterministic: expected {expected_snapshot_id}, found {}",
            snapshot.id
        )));
    }
    let source = &artifact.catalogue.snapshot;
    if source.context.artifact != snapshot.artifact_name
        || source.sha256 != snapshot.sha256
        || source.byte_length != snapshot.byte_size
        || source.context.system != snapshot.system_context
        || source.context.client != snapshot.client_context
        || source.context.release != snapshot.release_context
        || artifact.catalogue.parser.name != parser.parser_name
        || artifact.catalogue.parser.version != parser.parser_version
        || serde_json::to_value(&artifact.catalogue.parser.options)? != parser.options
        || serde_json::to_value(&artifact.evidence)? != parser.evidence
    {
        return Err(KernelError::Reconciliation(
            "generated catalogue contents differ from their snapshot/parser registration"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Recover nonterminal prior jobs and remove only workspace-owned `.part` files.
///
/// # Errors
///
/// Returns an error if the workspace cannot be locked or initialized, durable
/// state cannot be updated, or a workspace-owned partial file cannot be removed.
pub fn recover_workspace(workspace: &Path) -> Result<RecoveryReport> {
    let writer = WorkspaceWriter::open(workspace)?;
    recover_locked(&writer)
}

/// Return deterministic durable job status without changing workspace state.
///
/// # Errors
///
/// Returns an error if existing workspace state cannot be opened, queried, or
/// converted without violating its nonnegative counter invariants.
pub fn workspace_status(workspace: &Path) -> Result<WorkspaceStatus> {
    let database_path = workspace.join(DATABASE_FILENAME);
    if !database_path.exists() {
        return Ok(empty_status(workspace));
    }
    let connection = open_read_only(&database_path)?;
    let mut statement = connection.prepare(
        "SELECT id, state, source_name, source_hash, source_size, stored_archive,
                policy_json, source_records, stored_records, pending_records,
                error_code, error_message, created_at, updated_at, completed_at,
                elapsed_ms, peak_rss_bytes, declared_buffer_bytes,
                peak_database_bytes, peak_wal_bytes, peak_shm_bytes
         FROM jobs
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<i64>>(15)?,
            row.get::<_, Option<i64>>(16)?,
            row.get::<_, Option<i64>>(17)?,
            row.get::<_, Option<i64>>(18)?,
            row.get::<_, Option<i64>>(19)?,
            row.get::<_, Option<i64>>(20)?,
        ))
    })?;

    let mut jobs = Vec::new();
    for row in rows {
        let row = row?;
        jobs.push(JobStatus {
            id: row.0,
            state: JobState::parse(&row.1)?,
            source_name: row.2,
            source_hash: row.3,
            source_size: optional_nonnegative(row.4, "source size")?,
            stored_archive: row.5,
            policy_json: row.6,
            source_records: nonnegative(row.7, "source record count")?,
            stored_records: nonnegative(row.8, "stored record count")?,
            pending_records: nonnegative(row.9, "pending record count")?,
            error_code: row.10,
            error_message: row.11,
            created_at: row.12,
            updated_at: row.13,
            completed_at: row.14,
            elapsed_ms: optional_nonnegative(row.15, "elapsed milliseconds")?,
            peak_rss_bytes: optional_nonnegative(row.16, "peak RSS")?,
            declared_buffer_bytes: optional_nonnegative(row.17, "declared buffer bytes")?,
            peak_database_bytes: optional_nonnegative(row.18, "peak database bytes")?,
            peak_wal_bytes: optional_nonnegative(row.19, "peak WAL bytes")?,
            peak_shm_bytes: optional_nonnegative(row.20, "peak shared-memory bytes")?,
        });
    }

    let active_jobs = jobs.iter().filter(|job| !job.state.is_terminal()).count();
    let complete_jobs = jobs
        .iter()
        .filter(|job| job.state == JobState::Complete)
        .count();
    let failed_jobs = jobs
        .iter()
        .filter(|job| job.state == JobState::Failed)
        .count();
    let interrupted_jobs = jobs
        .iter()
        .filter(|job| job.state == JobState::Interrupted)
        .count();
    Ok(WorkspaceStatus {
        workspace: workspace.to_path_buf(),
        active_jobs,
        complete_jobs,
        failed_jobs,
        interrupted_jobs,
        jobs,
    })
}

/// Verify `SQLite` integrity, completed-job reconciliation, and package hashes.
///
/// # Errors
///
/// Returns an error if workspace state or a present immutable package cannot be
/// read. Detected integrity mismatches are returned as report issues.
#[allow(clippy::too_many_lines)]
pub fn integrity_check(workspace: &Path) -> Result<IntegrityReport> {
    let object_workspace = crate::object_workspace::replay_integrity(workspace);
    let mut issues = object_workspace.issues;
    let mut warnings = object_workspace.warnings;
    let database_path = workspace.join(DATABASE_FILENAME);
    if !database_path.exists() {
        return Ok(IntegrityReport {
            workspace: workspace.to_path_buf(),
            ok: issues.is_empty(),
            sqlite_ok: true,
            checked_jobs: 0,
            checked_packages: 0,
            checked_metadata_snapshots: 0,
            checked_catalogues: 0,
            checked_profiling_runs: 0,
            checked_partial_cleanups: 0,
            checked_profile_versions: 0,
            checked_profile_approvals: 0,
            checked_mappings: 0,
            checked_validation_runs: 0,
            checked_validation_issues: 0,
            checked_engagement_tour_heads: 0,
            checked_engagement_tour_attempts: 0,
            checked_engagement_tour_command_attempts: 0,
            checked_mapping_reuse_authorities: 0,
            checked_comparison_namespaces: 0,
            checked_tour_finding_sets: 0,
            checked_tour_finding_members: 0,
            checked_validation_comparisons: 0,
            checked_validation_comparison_items: 0,
            checked_mapping_authorities: 0,
            checked_remediation_pack_attempts: 0,
            checked_remediation_packs: 0,
            checked_remediation_pack_members: 0,
            checked_remediation_issue_members: 0,
            checked_object_workspaces: object_workspace.checked_workspaces,
            checked_decision_attempts: 0,
            checked_decision_attempt_events: 0,
            checked_correction_overlays: 0,
            checked_reevaluation_runs: 0,
            checked_reevaluation_events: 0,
            checked_reevaluation_issues: 0,
            checked_issue_decisions: 0,
            checked_correction_effectivations: 0,
            checked_active_decision_heads: 0,
            checked_inert_overlays: 0,
            checked_incomplete_decision_attempts: 0,
            checked_reconciliation_approval: true,
            warnings,
            issues,
        });
    }
    let connection = open_read_only(&database_path)?;
    let sqlite_result: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let sqlite_integrity_ok = sqlite_result == "ok";
    if !sqlite_integrity_ok {
        issues.push(format!("SQLite integrity_check: {sqlite_result}"));
    }
    let mut foreign_key_statement = connection.prepare("PRAGMA foreign_key_check")?;
    let foreign_key_rows = foreign_key_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut foreign_key_ok = true;
    for violation in foreign_key_rows {
        let (table, row_id, parent, constraint) = violation?;
        foreign_key_ok = false;
        issues.push(format!(
            "SQLite foreign_key_check: table {table:?}, row {row_id:?}, parent {parent:?}, constraint {constraint}"
        ));
    }
    drop(foreign_key_statement);
    let sqlite_ok = sqlite_integrity_ok && foreign_key_ok;

    let mut statement = connection.prepare(
        "SELECT id, state, source_hash, source_size, stored_archive, policy_json,
                manifest_entries, csv_files, expanded_bytes,
                source_records, stored_records, pending_records
         FROM jobs
         ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, i64>(11)?,
        ))
    })?;
    let mut checked_jobs = 0_usize;
    let mut checked_packages = 0_usize;
    for row in rows {
        let (
            id,
            state,
            hash,
            size,
            stored_archive,
            policy_json,
            manifest_entries,
            csv_files,
            expanded_bytes,
            source,
            stored,
            pending,
        ) = row?;
        checked_jobs += 1;
        let state = JobState::parse(&state)?;
        if stored > source {
            issues.push(format!(
                "job {id} stored record count {stored} exceeds source record count {source}"
            ));
        }
        if state == JobState::Complete && (source != stored || pending != 0) {
            issues.push(format!(
                "job {id} reconciliation is source={source}, stored={stored}, pending={pending}"
            ));
        }
        let (actual_manifest_entries, actual_expanded_bytes): (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(expanded_size), 0)
                 FROM archive_entries WHERE job_id = ?1",
            [&id],
            |record| Ok((record.get(0)?, record.get(1)?)),
        )?;
        let (actual_csv_files, csv_source_records, csv_stored_records): (i64, i64, i64) =
            connection.query_row(
                "SELECT COUNT(*), COALESCE(SUM(source_records), 0),
                        COALESCE(SUM(stored_records), 0)
                 FROM csv_files WHERE job_id = ?1",
                [&id],
                |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?)),
            )?;
        let actual_stored_records: i64 = connection.query_row(
            "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
            [&id],
            |record| record.get(0),
        )?;
        if manifest_entries != actual_manifest_entries
            || csv_files != actual_csv_files
            || expanded_bytes != actual_expanded_bytes
        {
            issues.push(format!(
                "job {id} manifest counters differ from durable rows: entries {manifest_entries}/{actual_manifest_entries}, CSV files {csv_files}/{actual_csv_files}, expanded bytes {expanded_bytes}/{actual_expanded_bytes}"
            ));
        }
        if source != csv_source_records
            || stored != csv_stored_records
            || stored != actual_stored_records
            || stored > source
            || pending != source.saturating_sub(stored)
        {
            issues.push(format!(
                "job {id} record counters differ from durable rows: job source/stored/pending {source}/{stored}/{pending}, CSV source/stored {csv_source_records}/{csv_stored_records}, rows {actual_stored_records}"
            ));
        }
        let (Some(expected_hash), Some(expected_size)) = (hash, size) else {
            continue;
        };
        if !is_sha256(&expected_hash) {
            issues.push(format!(
                "job {id} has an invalid source hash and its package was not opened"
            ));
            continue;
        }
        let expected_relative_path = format!("{PACKAGES_DIRECTORY}/{expected_hash}.zip");
        if stored_archive.as_deref() != Some(expected_relative_path.as_str()) {
            issues.push(format!(
                "job {id} stored archive path differs from its content-addressed evidence: expected {expected_relative_path:?}, found {stored_archive:?}"
            ));
        }
        let package_path = workspace
            .join(PACKAGES_DIRECTORY)
            .join(format!("{expected_hash}.zip"));
        let package_metadata = match fs::symlink_metadata(&package_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                issues.push(format!(
                    "job {id} package {} cannot be inspected: {error}",
                    package_path.display()
                ));
                continue;
            }
        };
        if !package_metadata.file_type().is_file() {
            issues.push(format!(
                "job {id} package {} is not a regular file",
                package_path.display()
            ));
            continue;
        }
        let expected_size = nonnegative(expected_size, "package size")?;
        if state == JobState::Complete {
            let policy: IntakePolicy = match serde_json::from_str(&policy_json) {
                Ok(policy) => policy,
                Err(error) => {
                    issues.push(format!(
                        "job {id} persisted intake policy is invalid JSON: {error}"
                    ));
                    continue;
                }
            };
            checked_packages += 1;
            match verify_ingested_package(&connection, &id, &package_path, &policy) {
                Ok(verification) => {
                    if !verification.readonly {
                        issues.push(format!(
                            "job {id} package {} is not read-only",
                            package_path.display()
                        ));
                    }
                    if verification.size != expected_size || verification.sha256 != expected_hash {
                        issues.push(format!(
                            "job {id} package hash/size mismatch: expected {expected_hash}/{expected_size}, actual {}/{}",
                            verification.sha256, verification.size
                        ));
                    }
                    issues.extend(verification.issues);
                }
                Err(error) => issues.push(format!(
                    "job {id} package evidence verification failed: {error}"
                )),
            }
            continue;
        }
        checked_packages += 1;
        let policy: IntakePolicy = match serde_json::from_str(&policy_json) {
            Ok(policy) => policy,
            Err(error) => {
                issues.push(format!(
                    "job {id} persisted intake policy is invalid JSON: {error}"
                ));
                continue;
            }
        };
        match inspect_stored_package_file(&package_path, &policy) {
            Ok(verification) => {
                if !verification.readonly {
                    issues.push(format!(
                        "job {id} package {} is not read-only",
                        package_path.display()
                    ));
                }
                if verification.size != expected_size || verification.sha256 != expected_hash {
                    issues.push(format!(
                        "job {id} package hash/size mismatch: expected {expected_hash}/{expected_size}, actual {}/{}",
                        verification.sha256, verification.size
                    ));
                }
            }
            Err(error) => issues.push(format!(
                "job {id} package file verification failed: {error}"
            )),
        }
    }

    let mut snapshots = HashMap::new();
    let mut checked_metadata_snapshots = 0_usize;
    let mut snapshot_statement = connection.prepare(
        "SELECT id, artifact_name, immutable_path, sha256, byte_size, kind,
                system_context, client_context, release_context
         FROM metadata_snapshots ORDER BY id",
    )?;
    let snapshot_rows = snapshot_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;
    for row in snapshot_rows {
        let (id, artifact_name, relative_path, sha256, byte_size, kind, system, client, release) =
            row?;
        snapshots.insert(
            id.clone(),
            SnapshotEvidence {
                artifact_name,
                sha256: sha256.clone(),
                byte_size,
                system,
                client,
                release,
            },
        );
        if kind != "odata_edmx" {
            issues.push(format!(
                "metadata snapshot {id} has unsupported kind {kind:?}"
            ));
        }
        if !is_sha256(&sha256) {
            issues.push(format!("metadata snapshot {id} has an invalid SHA-256"));
            continue;
        }
        let expected_relative_path = format!("metadata/{sha256}.edmx");
        if relative_path != expected_relative_path {
            issues.push(format!(
                "metadata snapshot {id} path is not content-addressed: expected {expected_relative_path:?}, found {relative_path:?}"
            ));
        }
        let path = match safe_evidence_file(workspace, &relative_path) {
            Ok(path) => path,
            Err(detail) => {
                issues.push(format!("metadata snapshot {id} path is unsafe: {detail}"));
                continue;
            }
        };
        match hash_evidence_file(&path) {
            Ok((actual_hash, actual_size, readonly)) => {
                checked_metadata_snapshots += 1;
                if !readonly {
                    issues.push(format!(
                        "metadata snapshot {id} file {} is not read-only",
                        path.display()
                    ));
                }
                if actual_hash != sha256 || i64::try_from(actual_size).ok() != Some(byte_size) {
                    issues.push(format!(
                        "metadata snapshot {id} hash/size mismatch: expected {sha256}/{byte_size}, actual {actual_hash}/{actual_size}"
                    ));
                }
            }
            Err(detail) => issues.push(format!("metadata snapshot {id} {detail}")),
        }
    }
    drop(snapshot_statement);

    let mut checked_catalogues = 0_usize;
    let mut parser_statement = connection.prepare(
        "SELECT snapshot_id, parser_name, parser_version, options_json, outcome,
                catalogue_path, catalogue_sha256, evidence_json
         FROM parser_runs ORDER BY snapshot_id, parser_name, parser_version, started_at",
    )?;
    let parser_rows = parser_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    for row in parser_rows {
        let (
            snapshot_id,
            parser_name,
            parser_version,
            options_json,
            outcome,
            catalogue_path,
            catalogue_sha256,
            evidence_json,
        ) = row?;
        let Some(snapshot) = snapshots.get(&snapshot_id) else {
            issues.push(format!(
                "parser run references missing metadata snapshot {snapshot_id}"
            ));
            continue;
        };
        if outcome != "SUCCEEDED_GENERATED_DRAFT" {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} has non-draft outcome {outcome:?}"
            ));
        }
        let (Some(relative_path), Some(expected_hash)) = (catalogue_path, catalogue_sha256) else {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} has no generated catalogue evidence"
            ));
            continue;
        };
        if !is_sha256(&expected_hash) {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} has an invalid catalogue SHA-256"
            ));
            continue;
        }
        let expected_relative_path = format!("catalogues/{expected_hash}.json");
        if relative_path != expected_relative_path {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue path is not content-addressed: expected {expected_relative_path:?}, found {relative_path:?}"
            ));
        }
        let path = match safe_evidence_file(workspace, &relative_path) {
            Ok(path) => path,
            Err(detail) => {
                issues.push(format!(
                    "parser run for snapshot {snapshot_id} catalogue path is unsafe: {detail}"
                ));
                continue;
            }
        };
        let (bytes, actual_hash, _, readonly) = match read_evidence_file(&path) {
            Ok(evidence) => evidence,
            Err(detail) => {
                issues.push(format!(
                    "parser run for snapshot {snapshot_id} catalogue {detail}"
                ));
                continue;
            }
        };
        checked_catalogues += 1;
        if !readonly {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue {} is not read-only",
                path.display()
            ));
        }
        if actual_hash != expected_hash {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue hash mismatch: expected {expected_hash}, actual {actual_hash}"
            ));
            continue;
        }
        let artifact: TechnicalCatalogueArtifact = match serde_json::from_slice(&bytes) {
            Ok(artifact) => artifact,
            Err(error) => {
                issues.push(format!(
                    "parser run for snapshot {snapshot_id} catalogue JSON is invalid: {error}"
                ));
                continue;
            }
        };
        if let Err(error) = artifact.verify_contract() {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue contract failed: {error}"
            ));
        }
        match serde_json::to_vec(&artifact.catalogue.snapshot) {
            Ok(identity) => {
                let expected_snapshot_id = format!("edmx-{}", crate::canonical::lower_hex(Sha256::digest(identity)));
                if snapshot_id != expected_snapshot_id {
                    issues.push(format!(
                        "metadata snapshot ID is not deterministic: expected {expected_snapshot_id}, found {snapshot_id}"
                    ));
                }
            }
            Err(error) => issues.push(format!(
                "parser run for snapshot {snapshot_id} snapshot identity cannot be serialized: {error}"
            )),
        }
        if artifact.catalogue.status != TechnicalCatalogueStatus::GeneratedDraft {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue is not GENERATED_DRAFT"
            ));
        }
        if artifact.catalogue.snapshot.context.artifact != snapshot.artifact_name
            || artifact.catalogue.snapshot.sha256 != snapshot.sha256
            || i64::try_from(artifact.catalogue.snapshot.byte_length).ok()
                != Some(snapshot.byte_size)
            || artifact.catalogue.snapshot.context.system != snapshot.system
            || artifact.catalogue.snapshot.context.client != snapshot.client
            || artifact.catalogue.snapshot.context.release != snapshot.release
        {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} catalogue snapshot evidence differs from its registered immutable snapshot"
            ));
        }
        if artifact.catalogue.parser.name != parser_name
            || artifact.catalogue.parser.version != parser_version
            || serde_json::from_str::<serde_json::Value>(&options_json).ok()
                != serde_json::to_value(&artifact.catalogue.parser.options).ok()
        {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} parser identity/options differ from its catalogue"
            ));
        }
        if serde_json::from_str::<serde_json::Value>(&evidence_json).ok()
            != serde_json::to_value(&artifact.evidence).ok()
        {
            issues.push(format!(
                "parser run for snapshot {snapshot_id} stored hash evidence differs from its catalogue"
            ));
        }
    }
    drop(parser_statement);
    let checked_profiling_runs = verify_profiling_runs(&connection, &mut issues)?;
    verify_engagement_integrity(workspace, &connection, &mut issues)?;
    let checked_partial_cleanups =
        verify_partial_cleanups(workspace, &connection, &mut issues, &mut warnings)?;
    let (checked_profile_versions, checked_profile_approvals) =
        crate::profile_registry::verify_integrity(&connection, &mut issues)?;
    let _checked_destination_authorities =
        crate::destination::verify_integrity(&connection, &mut issues)?;
    let checked_mappings = crate::mapping::verify_integrity(&connection, &mut issues)?;
    let (checked_validation_runs, checked_validation_issues) =
        crate::validation::verify_integrity(&connection, &mut issues)?;
    let tour_integrity = crate::engagement::verify_tour_integrity(&connection, &mut issues)?;
    let remediation_integrity =
        crate::engagement::verify_remediation_integrity(workspace, &connection, &mut issues)?;
    let decision_integrity =
        match crate::decision_integrity::verify_decision_integrity_connection(&connection) {
            Ok(summary) => summary,
            Err(KernelError::DecisionIntegrity(detail)) => {
                issues.push(format!("WP-005 decision integrity: {detail}"));
                crate::decision_integrity::DecisionIntegritySummary::default()
            }
            Err(error) => return Err(error),
        };
    let checked_reconciliation_approval =
        match crate::reconciliation_integrity::verify_reconciliation_approval_integrity_connection(
            &connection,
        ) {
            Ok(()) => true,
            Err(KernelError::ResultApprovalIntegrity(detail)) => {
                issues.push(format!(
                    "WP-006 reconciliation/approval integrity: {detail}"
                ));
                false
            }
            Err(error) => return Err(error),
        };
    Ok(IntegrityReport {
        workspace: workspace.to_path_buf(),
        ok: sqlite_ok && issues.is_empty(),
        sqlite_ok,
        checked_jobs,
        checked_packages,
        checked_metadata_snapshots,
        checked_catalogues,
        checked_profiling_runs,
        checked_partial_cleanups,
        checked_profile_versions,
        checked_profile_approvals,
        checked_mappings,
        checked_validation_runs,
        checked_validation_issues,
        checked_engagement_tour_heads: tour_integrity.heads,
        checked_engagement_tour_attempts: tour_integrity.attempts,
        checked_engagement_tour_command_attempts: tour_integrity.commands,
        checked_mapping_reuse_authorities: tour_integrity.reuse_authorities,
        checked_comparison_namespaces: tour_integrity.namespaces,
        checked_tour_finding_sets: tour_integrity.finding_sets,
        checked_tour_finding_members: tour_integrity.finding_members,
        checked_validation_comparisons: tour_integrity.comparisons,
        checked_validation_comparison_items: tour_integrity.comparison_items,
        checked_mapping_authorities: remediation_integrity.mapping_authorities,
        checked_remediation_pack_attempts: remediation_integrity.attempts,
        checked_remediation_packs: remediation_integrity.packs,
        checked_remediation_pack_members: remediation_integrity.members,
        checked_remediation_issue_members: remediation_integrity.issue_members,
        checked_object_workspaces: object_workspace.checked_workspaces,
        checked_decision_attempts: usize::try_from(decision_integrity.decision_attempts)
            .unwrap_or(usize::MAX),
        checked_decision_attempt_events: usize::try_from(
            decision_integrity.decision_attempt_events,
        )
        .unwrap_or(usize::MAX),
        checked_correction_overlays: usize::try_from(decision_integrity.correction_overlays)
            .unwrap_or(usize::MAX),
        checked_reevaluation_runs: usize::try_from(decision_integrity.reevaluation_runs)
            .unwrap_or(usize::MAX),
        checked_reevaluation_events: usize::try_from(decision_integrity.reevaluation_events)
            .unwrap_or(usize::MAX),
        checked_reevaluation_issues: usize::try_from(decision_integrity.reevaluation_issues)
            .unwrap_or(usize::MAX),
        checked_issue_decisions: usize::try_from(decision_integrity.issue_decisions)
            .unwrap_or(usize::MAX),
        checked_correction_effectivations: usize::try_from(
            decision_integrity.correction_effectivations,
        )
        .unwrap_or(usize::MAX),
        checked_active_decision_heads: usize::try_from(decision_integrity.active_decision_heads)
            .unwrap_or(usize::MAX),
        checked_inert_overlays: usize::try_from(decision_integrity.inert_overlays)
            .unwrap_or(usize::MAX),
        checked_incomplete_decision_attempts: usize::try_from(
            decision_integrity.incomplete_attempts,
        )
        .unwrap_or(usize::MAX),
        checked_reconciliation_approval,
        warnings,
        issues,
    })
}

fn verify_partial_cleanups(
    workspace: &Path,
    connection: &Connection,
    issues: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<usize> {
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version < 4 {
        return Ok(0);
    }
    let mut statement = connection.prepare(
        "SELECT cleanup_id, namespace, attempt_id, state, cleanup_attempts,
                last_error_message
         FROM partial_cleanups ORDER BY namespace, cleanup_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut checked = 0_usize;
    let mut tracked_live_partials = HashSet::new();
    let mut tracked_tombstones = HashSet::new();
    for row in rows {
        let (cleanup_id, namespace, attempt_id, state, attempts, last_error) = row?;
        checked += 1;
        if !is_canonical_uuid(&cleanup_id) || !is_canonical_uuid(&attempt_id) {
            issues.push(format!(
                "partial cleanup {cleanup_id:?} contains a noncanonical internal UUID"
            ));
            continue;
        }
        if attempts < 0 {
            issues.push(format!(
                "partial cleanup {cleanup_id} has negative cleanup attempts {attempts}"
            ));
        }
        let namespace = match CleanupNamespace::parse(&namespace) {
            Ok(namespace) => namespace,
            Err(error) => {
                issues.push(error.to_string());
                continue;
            }
        };
        let directory = workspace.join(namespace.as_str());
        let live = directory.join(format!("{attempt_id}.part"));
        let tombstone = directory.join(format!("{cleanup_id}.{TOMBSTONE_EXTENSION}"));
        tracked_live_partials.insert(live.clone());
        tracked_tombstones.insert(tombstone.clone());
        let live_exists = optional_symlink_metadata(&live)?.is_some();
        let tombstone_metadata = optional_symlink_metadata(&tombstone)?;
        let tombstone_exists = tombstone_metadata.is_some();
        let namespace_safe = validate_controlled_namespace(workspace, &directory).is_ok();
        let tombstone_safe = namespace_safe
            && tombstone_metadata.as_ref().is_some_and(|metadata| {
                let regular = metadata.file_type().is_file();
                #[cfg(windows)]
                {
                    regular && !crate::durability::is_reparse_point(metadata)
                }
                #[cfg(not(windows))]
                {
                    regular
                }
            });
        match state.as_str() {
            "CLEANED" if live_exists || tombstone_exists => issues.push(format!(
                "partial cleanup {cleanup_id} is CLEANED but a live or tombstone artifact exists"
            )),
            "CLEANUP_PENDING" if live_exists || !tombstone_safe => issues.push(format!(
                "partial cleanup {cleanup_id} is CLEANUP_PENDING without one inert tombstone and no live partial"
            )),
            "CLEANUP_PENDING" => warnings.push(format!(
                "partial cleanup {cleanup_id} retains an inert tombstone after {attempts} deletion attempt(s): {}",
                last_error.as_deref().unwrap_or("cleanup diagnostic unavailable")
            )),
            "PLANNED" | "QUARANTINED" => issues.push(format!(
                "partial cleanup {cleanup_id} remains in {state} and requires recovery"
            )),
            "CONFLICT" => issues.push(format!(
                "partial cleanup {cleanup_id} is in CONFLICT: {}",
                last_error.as_deref().unwrap_or("conflict diagnostic unavailable")
            )),
            "CLEANED" => {}
            other => issues.push(format!(
                "partial cleanup {cleanup_id} has invalid state {other:?}"
            )),
        }
    }
    drop(statement);
    verify_cleanup_inventory(
        workspace,
        &tracked_live_partials,
        &tracked_tombstones,
        issues,
    )?;
    Ok(checked)
}

fn optional_symlink_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn verify_cleanup_inventory(
    workspace: &Path,
    tracked_live_partials: &HashSet<PathBuf>,
    tracked_tombstones: &HashSet<PathBuf>,
    issues: &mut Vec<String>,
) -> Result<()> {
    for namespace in [
        CleanupNamespace::Packages,
        CleanupNamespace::Metadata,
        CleanupNamespace::Catalogues,
    ] {
        let directory = workspace.join(namespace.as_str());
        if optional_symlink_metadata(&directory)?.is_none() {
            continue;
        }
        if let Err(error) = validate_controlled_namespace(workspace, &directory) {
            issues.push(error.to_string());
            continue;
        }
        for entry in fs::read_dir(&directory).map_err(|error| KernelError::io(&directory, error))? {
            let entry = entry.map_err(|error| KernelError::io(&directory, error))?;
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some((canonical_extension, tracked)) =
                cleanup_inventory_kind(extension, tracked_live_partials, tracked_tombstones)
            else {
                continue;
            };
            if extension != canonical_extension {
                issues.push(format!(
                    "controlled cleanup artifact {} does not use the canonical lowercase extension",
                    path.display()
                ));
                continue;
            }
            let Some(metadata) = optional_symlink_metadata(&path)? else {
                issues.push(format!(
                    "controlled cleanup artifact {} disappeared during integrity replay",
                    path.display()
                ));
                continue;
            };
            let safe_regular = metadata.file_type().is_file();
            #[cfg(windows)]
            let safe_regular = safe_regular && !crate::durability::is_reparse_point(&metadata);
            if !safe_regular {
                issues.push(format!(
                    "controlled cleanup artifact {} is not a regular non-reparse file",
                    path.display()
                ));
                continue;
            }
            if let Err(error) = validated_internal_id(&path, canonical_extension) {
                issues.push(error.to_string());
                continue;
            }
            if !tracked.contains(&path) {
                issues.push(format!(
                    "controlled namespace contains an untracked {canonical_extension} artifact {}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn cleanup_inventory_kind<'a>(
    extension: &str,
    tracked_live_partials: &'a HashSet<PathBuf>,
    tracked_tombstones: &'a HashSet<PathBuf>,
) -> Option<(&'static str, &'a HashSet<PathBuf>)> {
    #[cfg(windows)]
    if extension.eq_ignore_ascii_case("part") {
        return Some(("part", tracked_live_partials));
    }
    #[cfg(not(windows))]
    if extension == "part" {
        return Some(("part", tracked_live_partials));
    }
    #[cfg(windows)]
    if extension.eq_ignore_ascii_case(TOMBSTONE_EXTENSION) {
        return Some((TOMBSTONE_EXTENSION, tracked_tombstones));
    }
    #[cfg(not(windows))]
    if extension == TOMBSTONE_EXTENSION {
        return Some((TOMBSTONE_EXTENSION, tracked_tombstones));
    }
    None
}

struct PersistedProfilingRun {
    id: String,
    state: String,
    job_id: String,
    source_hash: String,
    profiler_version: String,
    policy_json: String,
    policy_hash: String,
    result_json: Option<String>,
    result_hash: Option<String>,
    files_processed: i64,
    rows_processed: i64,
    columns_processed: i64,
}

#[allow(clippy::too_many_lines)]
fn verify_profiling_runs(connection: &Connection, issues: &mut Vec<String>) -> Result<usize> {
    let mut statement = connection.prepare(
        "SELECT id, state, job_id, source_hash, profiler_version, policy_json,
                policy_hash, result_json, result_hash, files_processed,
                rows_processed, columns_processed
         FROM profiling_runs ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(PersistedProfilingRun {
            id: row.get(0)?,
            state: row.get(1)?,
            job_id: row.get(2)?,
            source_hash: row.get(3)?,
            profiler_version: row.get(4)?,
            policy_json: row.get(5)?,
            policy_hash: row.get(6)?,
            result_json: row.get(7)?,
            result_hash: row.get(8)?,
            files_processed: row.get(9)?,
            rows_processed: row.get(10)?,
            columns_processed: row.get(11)?,
        })
    })?;
    let mut checked = 0_usize;
    for run in rows {
        let run = run?;
        let run_id = &run.id;
        checked += 1;
        let persisted_job: Option<(String, Option<String>)> = connection
            .query_row(
                "SELECT state, source_hash FROM jobs WHERE id = ?1",
                [&run.job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match persisted_job {
            Some((job_state, job_hash))
                if job_state == "COMPLETE" && job_hash.as_deref() == Some(&run.source_hash) => {}
            Some((job_state, job_hash)) => issues.push(format!(
                "profiling run {run_id} is not bound to matching completed intake evidence: job state/hash {job_state}/{job_hash:?}"
            )),
            None => issues.push(format!(
                "profiling run {run_id} refers to missing intake job {}",
                run.job_id
            )),
        }
        let actual_policy_hash =
            crate::canonical::lower_hex(Sha256::digest(run.policy_json.as_bytes()));
        let policy_value = match serde_json::from_str::<serde_json::Value>(&run.policy_json) {
            Ok(value) => Some(value),
            Err(error) => {
                issues.push(format!(
                    "profiling run {run_id} policy is not valid JSON: {error}"
                ));
                None
            }
        };
        if actual_policy_hash != run.policy_hash {
            issues.push(format!(
                "profiling run {run_id} policy hash mismatch: expected {}, actual {actual_policy_hash}",
                run.policy_hash
            ));
        }
        let latest_event: Option<String> = connection
            .query_row(
                "SELECT state FROM profiling_events WHERE run_id = ?1
                 ORDER BY sequence DESC LIMIT 1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        if latest_event.as_deref() != Some(run.state.as_str()) {
            issues.push(format!(
                "profiling run {run_id} latest event {latest_event:?} differs from durable state {}",
                run.state
            ));
        }

        let mut parsed_result = None;
        if let (Some(json), Some(expected_hash)) = (&run.result_json, &run.result_hash) {
            match serde_json::from_str::<serde_json::Value>(json) {
                Ok(value) => {
                    match crate::profiling::canonical_result_hash_value(&value) {
                        Ok(actual_hash) if actual_hash != *expected_hash => issues.push(format!(
                            "profiling run {run_id} result hash mismatch: expected {expected_hash}, actual {actual_hash}"
                        )),
                        Ok(_) => {}
                        Err(error) => issues.push(format!(
                            "profiling run {run_id} canonical result hash failed: {error}"
                        )),
                    }
                    match serde_json::from_value::<crate::profiling::ProfilingResult>(value.clone())
                    {
                        Ok(_) => {}
                        Err(error) => issues.push(format!(
                            "profiling run {run_id} result does not match the profiling schema: {error}"
                        )),
                    }
                    parsed_result = Some(value);
                }
                Err(error) => issues.push(format!(
                    "profiling run {run_id} result is not valid JSON: {error}"
                )),
            }
        } else if run.result_json.is_some() != run.result_hash.is_some() {
            issues.push(format!(
                "profiling run {run_id} has incomplete result/hash evidence"
            ));
        }

        let (actual_files, actual_rows): (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(row_count), 0)
             FROM profiling_files WHERE run_id = ?1",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let actual_columns: i64 = connection.query_row(
            "SELECT COUNT(*) FROM profiling_columns WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )?;
        if run.state == "COMPLETE"
            && (run.files_processed != actual_files
                || run.rows_processed != actual_rows
                || run.columns_processed != actual_columns)
        {
            issues.push(format!(
                "profiling run {run_id} counters differ from child evidence: files {}/{actual_files}, rows {}/{actual_rows}, columns {}/{actual_columns}",
                run.files_processed, run.rows_processed, run.columns_processed
            ));
        }
        if run.state != "COMPLETE" && (actual_files != 0 || actual_columns != 0) {
            issues.push(format!(
                "incomplete profiling run {run_id} exposes {actual_files} file and {actual_columns} column result rows"
            ));
        }
        if let Some(result) = &parsed_result {
            verify_profiling_result_identity(&run, result, issues);
        }

        let mut file_statement = connection.prepare(
            "SELECT path, row_count, column_count, duplicate_keys_json,
                    (SELECT COUNT(*) FROM profiling_columns AS columns
                     WHERE columns.run_id = files.run_id AND columns.path = files.path),
                    (SELECT COALESCE(SUM(empty_count + nonempty_count), 0)
                     FROM profiling_columns AS columns
                     WHERE columns.run_id = files.run_id AND columns.path = files.path)
             FROM profiling_files AS files WHERE run_id = ?1 ORDER BY path",
        )?;
        let files = file_statement.query_map([&run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for file in files {
            let (
                path,
                row_count,
                column_count,
                duplicate_keys_json,
                actual_column_count,
                total_observations,
            ) = file?;
            if column_count != actual_column_count {
                issues.push(format!(
                    "profiling run {run_id} file {path:?} column count differs from child evidence: {column_count}/{actual_column_count}"
                ));
            }
            if run.state == "COMPLETE"
                && total_observations != row_count.saturating_mul(column_count)
            {
                issues.push(format!(
                    "profiling run {run_id} file {path:?} observations do not reconcile with rows and columns"
                ));
            }
            verify_profiling_file_evidence(
                connection,
                run_id,
                &path,
                row_count,
                column_count,
                &duplicate_keys_json,
                parsed_result.as_ref(),
                policy_value.as_ref(),
                issues,
            )?;
        }
    }
    Ok(checked)
}

fn verify_profiling_result_identity(
    run: &PersistedProfilingRun,
    result: &serde_json::Value,
    issues: &mut Vec<String>,
) {
    let matches = result.get("job_id").and_then(serde_json::Value::as_str)
        == Some(run.job_id.as_str())
        && result
            .get("source_hash")
            .and_then(serde_json::Value::as_str)
            == Some(run.source_hash.as_str())
        && result
            .get("policy_hash")
            .and_then(serde_json::Value::as_str)
            == Some(run.policy_hash.as_str())
        && result
            .pointer("/profiler/version")
            .and_then(serde_json::Value::as_str)
            == Some(run.profiler_version.as_str())
        && result.get("file_count").and_then(serde_json::Value::as_i64)
            == Some(run.files_processed)
        && result.get("row_count").and_then(serde_json::Value::as_i64) == Some(run.rows_processed)
        && result
            .get("column_count")
            .and_then(serde_json::Value::as_i64)
            == Some(run.columns_processed);
    if !matches {
        issues.push(format!(
            "profiling run {} result identity/counters differ from durable run evidence",
            run.id
        ));
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn verify_profiling_file_evidence(
    connection: &Connection,
    run_id: &str,
    path: &str,
    row_count: i64,
    column_count: i64,
    duplicate_keys_json: &str,
    result: Option<&serde_json::Value>,
    policy: Option<&serde_json::Value>,
    issues: &mut Vec<String>,
) -> Result<()> {
    let result_file = result.and_then(|result| {
        result
            .get("files")
            .and_then(serde_json::Value::as_array)
            .and_then(|files| {
                files
                    .iter()
                    .find(|file| file.get("path").and_then(serde_json::Value::as_str) == Some(path))
            })
    });
    let duplicate_keys = match serde_json::from_str::<serde_json::Value>(duplicate_keys_json) {
        Ok(value) => Some(value),
        Err(error) => {
            issues.push(format!(
                "profiling run {run_id} file {path:?} duplicate-key evidence is invalid JSON: {error}"
            ));
            None
        }
    };
    if let Some(file) = result_file {
        if file.get("row_count").and_then(serde_json::Value::as_i64) != Some(row_count)
            || file.get("column_count").and_then(serde_json::Value::as_i64) != Some(column_count)
            || duplicate_keys.as_ref() != file.get("duplicate_keys")
        {
            issues.push(format!(
                "profiling run {run_id} file {path:?} differs from canonical result evidence"
            ));
        }
    } else if result.is_some() {
        issues.push(format!(
            "profiling run {run_id} file {path:?} is missing from canonical result evidence"
        ));
    }

    let maximum_samples = policy
        .and_then(|value| value.get("maximum_samples_per_column"))
        .and_then(serde_json::Value::as_u64);
    let maximum_sample_bytes = policy
        .and_then(|value| value.get("maximum_sample_value_bytes"))
        .and_then(serde_json::Value::as_u64);
    let mut columns = connection.prepare(
        "SELECT column_ordinal, column_name, empty_count, nonempty_count,
                min_decoded_length, max_decoded_length, oversized_count,
                distinct_mode, distinct_count, samples_json
         FROM profiling_columns
         WHERE run_id = ?1 AND path = ?2 ORDER BY column_ordinal",
    )?;
    let mapped = columns.query_map(params![run_id, path], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    for column in mapped {
        let (
            ordinal,
            name,
            empty_count,
            nonempty_count,
            minimum,
            maximum,
            oversized_count,
            distinct_mode,
            distinct_count,
            samples_json,
        ) = column?;
        let samples = match serde_json::from_str::<serde_json::Value>(&samples_json) {
            Ok(value) => value,
            Err(error) => {
                issues.push(format!(
                    "profiling run {run_id} file {path:?} column {ordinal} samples are invalid JSON: {error}"
                ));
                serde_json::Value::Null
            }
        };
        if empty_count.saturating_add(nonempty_count) != row_count
            || oversized_count > row_count
            || (nonempty_count == 0 && (minimum.is_some() || maximum.is_some()))
            || (nonempty_count > 0 && (minimum.is_none() || maximum.is_none()))
            || (distinct_mode == "EXACT") != distinct_count.is_some()
            || (distinct_mode == "NOT_COMPUTED_DUE_TO_BOUND" && distinct_count.is_some())
        {
            issues.push(format!(
                "profiling run {run_id} file {path:?} column {ordinal} violates profiling counter semantics"
            ));
        }
        verify_samples(
            run_id,
            path,
            ordinal,
            &samples,
            maximum_samples,
            maximum_sample_bytes,
            issues,
        );
        if let Some(file) = result_file {
            let expected = file
                .get("columns")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| {
                    values.iter().find(|value| {
                        value.get("ordinal").and_then(serde_json::Value::as_i64) == Some(ordinal)
                    })
                });
            let actual = serde_json::json!({
                "ordinal": ordinal,
                "name": name,
                "empty_count": empty_count,
                "nonempty_count": nonempty_count,
                "min_decoded_length": minimum,
                "max_decoded_length": maximum,
                "oversized_count": oversized_count,
                "distinct": { "mode": distinct_mode, "count": distinct_count },
                "samples": samples,
            });
            if expected != Some(&actual) {
                issues.push(format!(
                    "profiling run {run_id} file {path:?} column {ordinal} differs from canonical result evidence"
                ));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_samples(
    run_id: &str,
    path: &str,
    ordinal: i64,
    samples: &serde_json::Value,
    maximum_samples: Option<u64>,
    maximum_sample_bytes: Option<u64>,
    issues: &mut Vec<String>,
) {
    let Some(samples) = samples.as_array() else {
        issues.push(format!(
            "profiling run {run_id} file {path:?} column {ordinal} samples are not an array"
        ));
        return;
    };
    if maximum_samples.is_some_and(|limit| samples.len() as u64 > limit) {
        issues.push(format!(
            "profiling run {run_id} file {path:?} column {ordinal} exceeds its sample-count bound"
        ));
    }
    for sample in samples {
        let hash = sample.get("sha256").and_then(serde_json::Value::as_str);
        let utf8_bytes = sample.get("utf8_bytes").and_then(serde_json::Value::as_u64);
        let decoded_length = sample
            .get("decoded_length")
            .and_then(serde_json::Value::as_u64);
        let value = sample.get("value");
        let structurally_valid = hash.is_some_and(is_sha256)
            && utf8_bytes.is_some()
            && decoded_length.is_some()
            && value.is_some();
        let value_valid = match value {
            Some(serde_json::Value::String(value)) => {
                utf8_bytes == u64::try_from(value.len()).ok()
                    && decoded_length == u64::try_from(value.chars().count()).ok()
                    && hash
                        == Some(
                            crate::canonical::lower_hex(Sha256::digest(value.as_bytes())).as_str(),
                        )
                    && maximum_sample_bytes.is_none_or(|limit| {
                        u64::try_from(value.len()).is_ok_and(|length| length <= limit)
                    })
            }
            Some(serde_json::Value::Null) => maximum_sample_bytes
                .zip(utf8_bytes)
                .is_some_and(|(limit, length)| length > limit),
            _ => false,
        };
        if !structurally_valid || !value_valid {
            issues.push(format!(
                "profiling run {run_id} file {path:?} column {ordinal} contains invalid bounded sample evidence"
            ));
        }
    }
}

#[allow(clippy::too_many_lines)]
fn verify_engagement_integrity(
    workspace: &Path,
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    crate::engagement::verify_preflight_authority_integrity(workspace, connection, issues)?;
    crate::engagement::verify_validation_attempt_integrity(connection, issues)?;
    let engagement_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM engagement_records", [], |row| {
            row.get(0)
        })?;
    if engagement_count == 0 {
        return Ok(());
    }
    if engagement_count != 1 {
        issues.push(format!(
            "engagement workspace contains {engagement_count} identities; expected exactly one"
        ));
        return Ok(());
    }
    let engagement_id: String = connection.query_row(
        "SELECT engagement_id FROM engagement_records WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let context_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM engagement_active_context WHERE engagement_id = ?1",
        [&engagement_id],
        |row| row.get(0),
    )?;
    if context_count != 1 {
        issues.push(format!(
            "engagement {engagement_id} has {context_count} active contexts"
        ));
    }

    let mut statement = connection.prepare(
        "SELECT preflight_id, state, retained_relative_path, source_hash,
                source_size, policy_json, policy_hash, manifest_json, manifest_hash
         FROM engagement_source_preflights
         WHERE engagement_id = ?1 ORDER BY preflight_id",
    )?;
    let rows = statement.query_map([&engagement_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;
    for row in rows {
        let (
            preflight_id,
            state,
            relative_path,
            source_hash,
            source_size,
            policy_json,
            policy_hash,
            manifest_json,
            manifest_hash,
        ) = row?;
        let latest_event: Option<String> = connection
            .query_row(
                "SELECT state FROM engagement_preflight_events
                 WHERE preflight_id = ?1 ORDER BY sequence DESC LIMIT 1",
                [&preflight_id],
                |row| row.get(0),
            )
            .optional()?;
        if latest_event.as_deref() != Some(state.as_str()) {
            issues.push(format!(
                "engagement preflight {preflight_id} state {state} differs from latest event {latest_event:?}"
            ));
        }
        let Ok(policy_value) = serde_json::from_str::<serde_json::Value>(&policy_json) else {
            issues.push(format!(
                "engagement preflight {preflight_id} policy JSON is invalid"
            ));
            continue;
        };
        match crate::canonical::canonical_hash(&policy_value) {
            Ok(actual) if actual == policy_hash => {}
            Ok(_) => issues.push(format!(
                "engagement preflight {preflight_id} policy hash mismatch"
            )),
            Err(error) => issues.push(format!(
                "engagement preflight {preflight_id} policy canonicalization failed: {error}"
            )),
        }
        if state != "READY" {
            continue;
        }
        let (Some(relative_path), Some(source_hash), Some(source_size)) =
            (relative_path, source_hash, source_size)
        else {
            issues.push(format!(
                "READY engagement preflight {preflight_id} lacks source evidence"
            ));
            continue;
        };
        let expected_relative = format!("engagement/preflights/retained/{source_hash}.zip");
        if relative_path != expected_relative {
            issues.push(format!(
                "engagement preflight {preflight_id} retained path is not content-addressed"
            ));
            continue;
        }
        let path = match safe_evidence_file(workspace, &relative_path) {
            Ok(path) => path,
            Err(error) => {
                issues.push(format!(
                    "engagement preflight {preflight_id} retained file is unsafe: {error}"
                ));
                continue;
            }
        };
        match hash_evidence_file(&path) {
            Ok((actual_hash, actual_size, readonly)) => {
                if actual_hash != source_hash
                    || i64::try_from(actual_size).ok() != Some(source_size)
                    || !readonly
                {
                    issues.push(format!(
                        "engagement preflight {preflight_id} retained hash/size/read-only evidence differs"
                    ));
                }
            }
            Err(error) => issues.push(format!(
                "engagement preflight {preflight_id} cannot be hashed: {error}"
            )),
        }
        let (Some(manifest_json), Some(manifest_hash)) = (manifest_json, manifest_hash) else {
            issues.push(format!(
                "READY engagement preflight {preflight_id} lacks manifest evidence"
            ));
            continue;
        };
        match crate::canonical::parse_unique_json(manifest_json.as_bytes())
            .and_then(|value| crate::canonical::canonical_hash(&value))
        {
            Ok(actual) if actual == manifest_hash => {}
            Ok(_) => issues.push(format!(
                "engagement preflight {preflight_id} manifest hash mismatch"
            )),
            Err(error) => issues.push(format!(
                "engagement preflight {preflight_id} manifest JSON is invalid: {error}"
            )),
        }
    }
    drop(statement);

    let linkage_issue_count: i64 = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM engagement_active_context c
              LEFT JOIN jobs j ON j.id = c.active_job_id
              LEFT JOIN engagement_source_preflights f
                ON f.preflight_id = c.current_preflight_id
              WHERE c.engagement_id = ?1
                AND c.active_job_id IS NOT NULL
                AND (j.id IS NULL OR j.state <> 'COMPLETE'
                     OR j.source_hash IS NULL
                     OR f.preflight_id IS NULL OR f.state <> 'READY'
                     OR f.source_hash IS NOT j.source_hash
                     OR f.manifest_json IS NULL OR f.manifest_hash IS NULL))
           + (SELECT COUNT(*) FROM engagement_active_context c
              WHERE c.engagement_id = ?1
                AND (c.selected_object_pack_id IS NOT c.selected_profile_id
                     OR (c.selected_object_pack_id IS NULL
                         AND (c.selected_profile_version IS NOT NULL
                              OR c.selected_profile_hash IS NOT NULL))
                     OR (c.selected_object_pack_id IS NOT NULL
                         AND (c.selected_profile_version IS NULL
                              OR c.selected_profile_hash IS NULL))))
           + (SELECT COUNT(*) FROM engagement_active_context c
              LEFT JOIN profiling_runs p ON p.id = c.active_profiling_run_id
              LEFT JOIN jobs j ON j.id = c.active_job_id
              WHERE c.engagement_id = ?1
                AND c.active_profiling_run_id IS NOT NULL
                AND (p.id IS NULL OR p.job_id IS NOT c.active_job_id
                     OR j.id IS NULL OR p.source_hash IS NOT j.source_hash))
           + (SELECT COUNT(*) FROM engagement_active_context c
              LEFT JOIN jobs j ON j.id = c.active_job_id
              LEFT JOIN mapping_versions m
                ON m.mapping_id = c.active_mapping_id
               AND m.mapping_version = c.active_mapping_version
               AND m.mapping_hash = c.active_mapping_hash
              WHERE c.engagement_id = ?1
                AND c.active_mapping_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM engagement_tour_heads h
                    WHERE h.engagement_id = c.engagement_id
                      AND h.active_tour_id IS NOT NULL
                )
                AND (m.mapping_id IS NULL
                     OR m.validated_job_id IS NOT c.active_job_id
                     OR j.id IS NULL OR m.source_hash IS NOT j.source_hash
                     OR m.profile_id IS NOT c.selected_profile_id
                     OR m.profile_version IS NOT c.selected_profile_version
                     OR m.profile_hash IS NOT c.selected_profile_hash))
           + (SELECT COUNT(*) FROM engagement_active_context c
              LEFT JOIN validation_runs v ON v.id = c.active_validation_run_id
              LEFT JOIN jobs j ON j.id = c.active_job_id
                WHERE c.engagement_id = ?1
                AND c.active_validation_run_id IS NOT NULL
                AND (v.id IS NULL OR v.job_id IS NOT c.active_job_id
                     OR j.id IS NULL OR v.source_hash IS NOT j.source_hash
                     OR v.profile_id IS NOT c.selected_profile_id
                     OR v.profile_version IS NOT c.selected_profile_version
                     OR v.profile_hash IS NOT c.selected_profile_hash
                     OR v.mapping_id IS NOT c.active_mapping_id
                     OR v.mapping_version IS NOT c.active_mapping_version
                     OR v.mapping_hash IS NOT c.active_mapping_hash))",
        [&engagement_id],
        |row| row.get(0),
    )?;
    if linkage_issue_count != 0 {
        issues.push(format!(
            "engagement {engagement_id} has {linkage_issue_count} invalid active evidence linkage(s)"
        ));
    }
    crate::engagement::verify_mapping_authority_integrity(connection, issues)?;
    Ok(())
}

fn safe_evidence_path(workspace: &Path, relative: &str) -> std::result::Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "{} is not a normalized relative path",
            relative.display()
        ));
    }
    Ok(workspace.join(relative))
}

fn safe_evidence_file(workspace: &Path, relative: &str) -> std::result::Result<PathBuf, String> {
    let path = safe_evidence_path(workspace, relative)?;
    let relative_path = Path::new(relative);
    let mut parent = workspace.to_path_buf();
    if let Some(components) = relative_path.parent() {
        for component in components.components() {
            let Component::Normal(component) = component else {
                return Err(format!(
                    "{} is not a normalized relative path",
                    relative_path.display()
                ));
            };
            parent.push(component);
            let parent_metadata = fs::symlink_metadata(&parent).map_err(|error| {
                format!(
                    "evidence directory {} cannot be inspected: {error}",
                    parent.display()
                )
            })?;
            let parent_is_safe = parent_metadata.file_type().is_dir();
            #[cfg(windows)]
            let parent_is_safe =
                parent_is_safe && !crate::durability::is_reparse_point(&parent_metadata);
            if !parent_is_safe {
                return Err(format!(
                    "evidence directory {} is not a real directory",
                    parent.display()
                ));
            }
        }
    }
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    let path_is_safe = metadata.file_type().is_file();
    #[cfg(windows)]
    let path_is_safe = path_is_safe && !crate::durability::is_reparse_point(&metadata);
    if !path_is_safe {
        return Err(format!("path {} is not a regular file", path.display()));
    }
    let canonical_workspace = workspace.canonicalize().map_err(|error| {
        format!(
            "workspace {} cannot be canonicalized: {error}",
            workspace.display()
        )
    })?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("file {} cannot be canonicalized: {error}", path.display()))?;
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(format!(
            "path {} resolves outside workspace {}",
            path.display(),
            workspace.display()
        ));
    }
    Ok(path)
}

fn hash_evidence_file(path: &Path) -> std::result::Result<(String, u64, bool), String> {
    let file = open_evidence_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    let mut reader = BufReader::new(&file);
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("file {} cannot be read: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size = size
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| format!("file {} byte count overflow", path.display()))?;
    }
    drop(reader);
    verify_evidence_file_identity(path, &file)?;
    Ok((
        crate::canonical::lower_hex(digest.finalize()),
        size,
        metadata.permissions().readonly(),
    ))
}

fn read_evidence_file(path: &Path) -> std::result::Result<(Vec<u8>, String, u64, bool), String> {
    let file = open_evidence_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    let mut reader = BufReader::new(&file);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("file {} cannot be read: {error}", path.display()))?;
    drop(reader);
    verify_evidence_file_identity(path, &file)?;
    let size = u64::try_from(bytes.len())
        .map_err(|_| format!("file {} byte count overflow", path.display()))?;
    let hash = crate::canonical::lower_hex(Sha256::digest(&bytes));
    Ok((bytes, hash, size, metadata.permissions().readonly()))
}

#[cfg(unix)]
fn open_evidence_nofollow(path: &Path) -> std::result::Result<File, String> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| {
        let error: std::io::Error = error.into();
        evidence_open_error(path, &error)
    })?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?
        .is_file()
    {
        return Err(format!("path {} is not a regular file", path.display()));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_evidence_nofollow(path: &Path) -> std::result::Result<File, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    let path_is_safe = metadata.file_type().is_file();
    #[cfg(windows)]
    let path_is_safe = path_is_safe && !crate::durability::is_reparse_point(&metadata);
    if !path_is_safe {
        return Err(format!("path {} is not a regular file", path.display()));
    }
    let file = File::open(path)
        .map_err(|error| format!("file {} cannot be opened: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?
        .is_file()
    {
        return Err(format!("path {} is not a regular file", path.display()));
    }
    Ok(file)
}

#[cfg(unix)]
fn evidence_open_error(path: &Path, error: &std::io::Error) -> String {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        format!("path {} is not a regular file", path.display())
    } else {
        format!("file {} cannot be opened: {error}", path.display())
    }
}

fn verify_evidence_file_identity(path: &Path, file: &File) -> std::result::Result<(), String> {
    let named = fs::symlink_metadata(path)
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    let named_is_safe = named.file_type().is_file();
    #[cfg(windows)]
    let named_is_safe = named_is_safe && !crate::durability::is_reparse_point(&named);
    if !named_is_safe {
        return Err(format!("path {} is not a regular file", path.display()));
    }
    #[cfg(not(windows))]
    let opened = file
        .metadata()
        .map_err(|error| format!("file {} cannot be inspected: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(format!(
                "path {} no longer names the verified file",
                path.display()
            ));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, file).map_err(|error| {
        format!(
            "file {} identity cannot be verified: {error}",
            path.display()
        )
    })? {
        return Err(format!(
            "path {} no longer names the verified file",
            path.display()
        ));
    }
    #[cfg(not(any(unix, windows)))]
    if named.len() != opened.len() {
        return Err(format!(
            "path {} changed during verification",
            path.display()
        ));
    }
    Ok(())
}

impl WorkspaceWriter {
    pub(crate) fn open(root: &Path) -> Result<Self> {
        #[cfg(test)]
        {
            fs::create_dir_all(root).map_err(|error| KernelError::io(root, error))?;
            let canonical_root = root
                .canonicalize()
                .map_err(|error| KernelError::io(root, error))?;
            WorkspaceLock::acquire(&canonical_root)?.open_writer_for_establishment()
        }
        #[cfg(not(test))]
        {
            let service = crate::object_workspace::ObjectWorkspaceService::new(root)?;
            service.require_active_for_writer_migration()?;
            WorkspaceLock::acquire(service.root())?.open_writer()
        }
    }
}

impl WorkspaceLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        let root_metadata =
            fs::symlink_metadata(root).map_err(|error| KernelError::io(root, error))?;
        if !root_metadata.file_type().is_dir() || read_metadata_is_reparse_point(&root_metadata) {
            return Err(KernelError::IntegrityFailed(
                "workspace root must be one real non-link directory".to_owned(),
            ));
        }
        let root_handle = retain_workspace_path_nofollow(root, true)?;
        #[cfg(unix)]
        if let Err(error) = root_handle.try_lock_exclusive() {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                return Err(KernelError::WorkspaceLocked(root.to_path_buf()));
            }
            return Err(KernelError::io(root, error));
        }
        let lock_path = root.join(LOCK_FILENAME);
        let lock = open_workspace_lock_nofollow(&lock_path)?;
        if let Err(error) = lock.try_lock_exclusive() {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                return Err(KernelError::WorkspaceLocked(root.to_path_buf()));
            }
            return Err(KernelError::io(&lock_path, error));
        }
        let value = Self {
            root: root.to_path_buf(),
            root_handle,
            lock_path,
            lock,
        };
        value.verify()?;
        Ok(value)
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn open_writer(&self) -> Result<WorkspaceWriter> {
        self.open_writer_inner(true, None, true)
    }

    #[cfg(test)]
    pub(crate) fn open_writer_for_establishment(&self) -> Result<WorkspaceWriter> {
        self.open_writer_inner(false, None, true)
    }

    /// Open the selected database under the retained workspace lock without
    /// applying schema migrations. Object-workspace establishment uses this
    /// narrow writer to durably record its exact resumable command before any
    /// migration can change legacy or bootstrap `SQLite` evidence.
    pub(crate) fn open_writer_for_pending_establishment(&self) -> Result<WorkspaceWriter> {
        self.open_writer_inner(false, None, false)
    }

    pub(crate) fn open_writer_for_establishment_with_descriptor(
        &self,
        expected_descriptor_hash: &str,
    ) -> Result<WorkspaceWriter> {
        self.open_writer_inner(false, Some(expected_descriptor_hash), true)
    }

    fn open_writer_inner(
        &self,
        verify_object_workspace: bool,
        expected_descriptor_hash: Option<&str>,
        migrate_schema: bool,
    ) -> Result<WorkspaceWriter> {
        self.verify()?;
        let database_path = self.root.join(DATABASE_FILENAME);
        reject_unsafe_database_path(&database_path)?;
        let database_before = database_path
            .try_exists()
            .map_err(|error| KernelError::io(&database_path, error))?
            .then(|| retain_workspace_path_nofollow(&database_path, false))
            .transpose()?;
        let mut connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let user_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(KernelError::IntegrityFailed(format!(
                "workspace schema version {user_version} is newer than supported version {SCHEMA_VERSION}"
            )));
        }
        let database_handle = match database_before {
            Some(handle) => {
                if !workspace_path_matches(&database_path, &handle, false)? {
                    return Err(KernelError::IntegrityFailed(
                        "workspace database identity changed while opening".to_owned(),
                    ));
                }
                handle
            }
            None => retain_workspace_path_nofollow(&database_path, false)?,
        };
        self.verify()?;
        let descriptor = if verify_object_workspace {
            Some(crate::object_workspace::retain_writer_descriptor_authority(
                &self.root,
                &connection,
            )?)
        } else if let Some(expected_hash) = expected_descriptor_hash {
            Some((
                crate::object_workspace::retain_pending_descriptor_authority(
                    &self.root,
                    expected_hash,
                )?,
                expected_hash.to_owned(),
            ))
        } else {
            None
        };
        let authority = Arc::new(WorkspaceWriterAuthority {
            root: self.root.clone(),
            root_handle: self
                .root_handle
                .try_clone()
                .map_err(|error| KernelError::io(&self.root, error))?,
            lock_path: self.lock_path.clone(),
            lock_handle: self
                .lock
                .try_clone()
                .map_err(|error| KernelError::io(&self.lock_path, error))?,
            database_path: database_path.clone(),
            database_handle,
            descriptor,
        });
        authority.verify()?;
        let commit_authority = Arc::clone(&authority);
        connection.commit_hook(Some(move || commit_authority.verify().is_err()))?;
        configure_writer(&connection)?;
        if migrate_schema {
            migrate(&connection)?;
        }
        if verify_object_workspace {
            let expected_descriptor_hash = authority
                .descriptor
                .as_ref()
                .map(|(_, hash)| hash.as_str())
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "active writer lost its retained object workspace descriptor".to_owned(),
                    )
                })?;
            crate::object_workspace::verify_writer_database_authority(
                &self.root,
                &connection,
                expected_descriptor_hash,
            )?;
        }
        self.verify()?;
        authority.verify()?;
        Ok(WorkspaceWriter {
            connection,
            root: self.root.clone(),
            authority,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn verify(&self) -> Result<()> {
        if !workspace_path_matches(&self.root, &self.root_handle, true)?
            || !workspace_path_matches(&self.lock_path, &self.lock, false)?
        {
            return Err(KernelError::IntegrityFailed(
                "workspace root or exclusive lock identity changed".to_owned(),
            ));
        }
        Ok(())
    }
}

impl WorkspaceWriter {
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn install_supplemental_commit_guard<F>(&mut self, mut guard: F) -> Result<()>
    where
        F: FnMut() -> Result<()> + Send + 'static,
    {
        let authority = Arc::clone(&self.authority);
        self.connection.commit_hook(Some(move || {
            authority.verify().is_err() || guard().is_err()
        }))?;
        Ok(())
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.authority.verify()
    }
}

impl WorkspaceWriterAuthority {
    fn verify(&self) -> Result<()> {
        if !workspace_path_matches(&self.root, &self.root_handle, true)?
            || !workspace_path_matches(&self.lock_path, &self.lock_handle, false)?
            || !workspace_path_matches(&self.database_path, &self.database_handle, false)?
        {
            return Err(KernelError::IntegrityFailed(
                "retained workspace writer pathname identity changed".to_owned(),
            ));
        }
        verify_workspace_writer_file_unaliased(&self.lock_handle, "workspace lock")?;
        verify_workspace_writer_file_unaliased(&self.database_handle, "workspace database")?;
        if let Some((descriptor, expected_hash)) = self.descriptor.as_ref() {
            crate::object_workspace::verify_retained_writer_descriptor(
                &self.root,
                descriptor,
                expected_hash,
            )?;
        }
        Ok(())
    }
}

fn verify_workspace_writer_file_unaliased(file: &File, label: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if file
            .metadata()
            .map_err(|error| KernelError::io(label, error))?
            .nlink()
            != 1
        {
            return Err(KernelError::IntegrityFailed(format!(
                "{label} gained a hard-link alias while writer authority was retained"
            )));
        }
    }
    #[cfg(windows)]
    if crate::durability::file_link_count(file).map_err(|error| KernelError::io(label, error))? != 1
    {
        return Err(KernelError::IntegrityFailed(format!(
            "{label} gained a hard-link alias while writer authority was retained"
        )));
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (file, label);
    Ok(())
}

fn reject_unsafe_database_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_file() && !read_metadata_is_reparse_point(&metadata) =>
        {
            Ok(())
        }
        Ok(_) => Err(KernelError::IntegrityFailed(
            "workspace database must be one real regular non-link file".to_owned(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

pub(crate) fn retain_workspace_root(path: &Path) -> Result<File> {
    retain_workspace_path_nofollow(path, true)
}

pub(crate) fn verify_retained_workspace_root(path: &Path, file: &File) -> Result<()> {
    if workspace_path_matches(path, file, true)? {
        Ok(())
    } else {
        Err(KernelError::IntegrityFailed(
            "startup-retained workspace root changed identity".to_owned(),
        ))
    }
}

#[cfg(unix)]
fn retain_workspace_path_nofollow(path: &Path, directory: bool) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| {
        let error: std::io::Error = error.into();
        KernelError::io(path, error)
    })?;
    let file = File::from(descriptor);
    if !workspace_path_matches(path, &file, directory)? {
        return Err(KernelError::IntegrityFailed(
            "workspace path identity changed while retaining it".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn retain_workspace_path_nofollow(path: &Path, directory: bool) -> Result<File> {
    let file = if directory {
        crate::durability::open_directory_nofollow(path)
    } else {
        crate::durability::open_file_nofollow(path)
    }
    .map_err(|error| KernelError::io(path, error))?;
    if !workspace_path_matches(path, &file, directory)? {
        return Err(KernelError::IntegrityFailed(
            "workspace path identity changed while retaining it".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn retain_workspace_path_nofollow(path: &Path, directory: bool) -> Result<File> {
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    if !workspace_path_matches(path, &file, directory)? {
        return Err(KernelError::IntegrityFailed(
            "workspace path identity changed while retaining it".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn workspace_path_matches(path: &Path, file: &File, directory: bool) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    let expected_kind = if directory {
        named.file_type().is_dir() && opened.file_type().is_dir()
    } else {
        named.file_type().is_file() && opened.file_type().is_file()
    };
    Ok(expected_kind
        && !named.file_type().is_symlink()
        && named.dev() == opened.dev()
        && named.ino() == opened.ino())
}

#[cfg(windows)]
fn workspace_path_matches(path: &Path, file: &File, directory: bool) -> Result<bool> {
    let matches = if directory {
        crate::durability::path_names_open_directory(path, file)
    } else {
        crate::durability::path_names_open_file(path, file)
    }
    .map_err(|error| KernelError::io(path, error))?;
    Ok(matches)
}

#[cfg(not(any(unix, windows)))]
fn workspace_path_matches(path: &Path, file: &File, directory: bool) -> Result<bool> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    Ok(if directory {
        named.file_type().is_dir() && opened.file_type().is_dir()
    } else {
        named.file_type().is_file() && opened.file_type().is_file() && named.len() == opened.len()
    })
}

#[cfg(unix)]
fn open_workspace_lock_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| {
        let error: std::io::Error = error.into();
        KernelError::io(path, error)
    })?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(KernelError::IntegrityFailed(
            "workspace lock must be one real regular non-link file".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_workspace_lock_nofollow(path: &Path) -> Result<File> {
    match std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(windows)]
            {
                crate::durability::open_file_nofollow(path)
                    .map_err(|error| KernelError::io(path, error))
            }
            #[cfg(not(windows))]
            {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|error| KernelError::io(path, error))
            }
        }
        Err(error) => Err(KernelError::io(path, error)),
    }
}

pub(crate) fn create_job(connection: &Connection, job: &NewJob<'_>) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let source_size = job
        .source_size
        .map(|value| sqlite_integer(value, "source size"))
        .transpose()?;
    transaction.execute(
        "INSERT INTO jobs (
             id, state, source_name, source_path, source_size, policy_json,
             created_at, updated_at
         ) VALUES (?1, 'SPOOLING', ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            job.id,
            job.source_name,
            job.source_path,
            source_size,
            job.policy_json,
            now,
        ],
    )?;
    insert_event(&transaction, job.id, JobState::Spooling, None, None, &now)?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn record_spooled_archive(
    connection: &Connection,
    job_id: &str,
    hash: &str,
    size: u64,
    stored_archive: &str,
) -> Result<()> {
    let size = sqlite_integer(size, "source size")?;
    let now = now_rfc3339();
    let changed = connection.execute(
        "UPDATE jobs
         SET source_hash = ?1, source_size = ?2, stored_archive = ?3, updated_at = ?4
         WHERE id = ?5 AND state = 'SPOOLING'",
        params![hash, size, stored_archive, now, job_id],
    )?;
    require_one_job(changed, job_id, "record spooled archive")
}

pub(crate) fn transition_job(connection: &Connection, job_id: &str, next: JobState) -> Result<()> {
    let current: String =
        connection.query_row("SELECT state FROM jobs WHERE id = ?1", [job_id], |row| {
            row.get(0)
        })?;
    let current = JobState::parse(&current)?;
    if !current.permits(next) {
        return Err(KernelError::Reconciliation(format!(
            "job {job_id} cannot transition from {} to {}",
            current.as_str(),
            next.as_str()
        )));
    }
    let transaction = connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let completed_at = (next == JobState::Complete).then_some(now.as_str());
    let changed = transaction.execute(
        "UPDATE jobs
         SET state = ?1, updated_at = ?2, completed_at = COALESCE(?3, completed_at)
         WHERE id = ?4 AND state = ?5",
        params![next.as_str(), now, completed_at, job_id, current.as_str()],
    )?;
    require_one_job(changed, job_id, "transition job")?;
    insert_event(&transaction, job_id, next, None, None, &now)?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn record_job_metrics(
    connection: &Connection,
    job_id: &str,
    metrics: &JobExecutionMetrics,
) -> Result<()> {
    let elapsed = sqlite_integer(metrics.elapsed_ms, "elapsed milliseconds")?;
    let peak = metrics
        .peak_rss_bytes
        .map(|value| sqlite_integer(value, "peak RSS"))
        .transpose()?;
    let declared = sqlite_integer(metrics.declared_buffer_bytes, "declared buffer bytes")?;
    let peak_database =
        optional_sqlite_integer(metrics.peak_database_bytes, "peak database bytes")?;
    let peak_wal = optional_sqlite_integer(metrics.peak_wal_bytes, "peak WAL bytes")?;
    let peak_shm = optional_sqlite_integer(metrics.peak_shm_bytes, "peak shared-memory bytes")?;
    let changed = connection.execute(
        "UPDATE jobs
         SET elapsed_ms = ?1, peak_rss_bytes = ?2, declared_buffer_bytes = ?3,
             peak_database_bytes = ?4, peak_wal_bytes = ?5, peak_shm_bytes = ?6,
             updated_at = ?7
         WHERE id = ?8",
        params![
            elapsed,
            peak,
            declared,
            peak_database,
            peak_wal,
            peak_shm,
            now_rfc3339(),
            job_id
        ],
    )?;
    require_one_job(changed, job_id, "record metrics")
}

pub(crate) fn fail_job(
    connection: &Connection,
    job_id: &str,
    error: &KernelError,
    metrics: &JobExecutionMetrics,
) -> Result<()> {
    let current: Option<String> = connection
        .query_row("SELECT state FROM jobs WHERE id = ?1", [job_id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(current) = current else {
        return Err(KernelError::Reconciliation(format!(
            "cannot fail missing job {job_id}"
        )));
    };
    let current = JobState::parse(&current)?;
    if current.is_terminal() {
        return Ok(());
    }
    if !current.permits(JobState::Failed) {
        return Err(KernelError::Reconciliation(format!(
            "job {job_id} cannot fail from {}",
            current.as_str()
        )));
    }
    let transaction = connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let elapsed = sqlite_integer(metrics.elapsed_ms, "elapsed milliseconds")?;
    let peak = metrics
        .peak_rss_bytes
        .map(|value| sqlite_integer(value, "peak RSS"))
        .transpose()?;
    let declared = sqlite_integer(metrics.declared_buffer_bytes, "declared buffer bytes")?;
    let peak_database =
        optional_sqlite_integer(metrics.peak_database_bytes, "peak database bytes")?;
    let peak_wal = optional_sqlite_integer(metrics.peak_wal_bytes, "peak WAL bytes")?;
    let peak_shm = optional_sqlite_integer(metrics.peak_shm_bytes, "peak shared-memory bytes")?;
    let message = error.to_string();
    let changed = transaction.execute(
        "UPDATE jobs
         SET state = 'FAILED', error_code = ?1, error_message = ?2,
             elapsed_ms = ?3, peak_rss_bytes = ?4, declared_buffer_bytes = ?5,
             peak_database_bytes = ?6, peak_wal_bytes = ?7, peak_shm_bytes = ?8,
             updated_at = ?9, completed_at = ?9
         WHERE id = ?10 AND state = ?11",
        params![
            error.code(),
            message,
            elapsed,
            peak,
            declared,
            peak_database,
            peak_wal,
            peak_shm,
            now,
            job_id,
            current.as_str(),
        ],
    )?;
    require_one_job(changed, job_id, "fail job")?;
    insert_event(
        &transaction,
        job_id,
        JobState::Failed,
        Some(error.code()),
        Some(&message),
        &now,
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn interrupt_job(
    connection: &Connection,
    job_id: &str,
    error: &KernelError,
    metrics: &JobExecutionMetrics,
) -> Result<()> {
    let current: Option<String> = connection
        .query_row("SELECT state FROM jobs WHERE id = ?1", [job_id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(current) = current else {
        return Err(KernelError::Reconciliation(format!(
            "cannot interrupt missing job {job_id}"
        )));
    };
    let current = JobState::parse(&current)?;
    if current.is_terminal() {
        return Ok(());
    }
    if !current.permits(JobState::Interrupted) {
        return Err(KernelError::Reconciliation(format!(
            "job {job_id} cannot be interrupted from {}",
            current.as_str()
        )));
    }

    let transaction = connection.unchecked_transaction()?;
    let (source, stored, pending) = durable_record_counters(&transaction, job_id)?;
    let now = now_rfc3339();
    let elapsed = sqlite_integer(metrics.elapsed_ms, "elapsed milliseconds")?;
    let peak = optional_sqlite_integer(metrics.peak_rss_bytes, "peak RSS")?;
    let declared = sqlite_integer(metrics.declared_buffer_bytes, "declared buffer bytes")?;
    let peak_database =
        optional_sqlite_integer(metrics.peak_database_bytes, "peak database bytes")?;
    let peak_wal = optional_sqlite_integer(metrics.peak_wal_bytes, "peak WAL bytes")?;
    let peak_shm = optional_sqlite_integer(metrics.peak_shm_bytes, "peak shared-memory bytes")?;
    let message = error.to_string();
    let changed = transaction.execute(
        "UPDATE jobs
         SET state = 'INTERRUPTED', error_code = ?1, error_message = ?2,
             source_records = ?3, stored_records = ?4, pending_records = ?5,
             elapsed_ms = ?6, peak_rss_bytes = ?7, declared_buffer_bytes = ?8,
             peak_database_bytes = ?9, peak_wal_bytes = ?10, peak_shm_bytes = ?11,
             updated_at = ?12, completed_at = ?12
         WHERE id = ?13 AND state = ?14",
        params![
            error.code(),
            message,
            source,
            stored,
            pending,
            elapsed,
            peak,
            declared,
            peak_database,
            peak_wal,
            peak_shm,
            now,
            job_id,
            current.as_str(),
        ],
    )?;
    require_one_job(changed, job_id, "interrupt job")?;
    insert_event(
        &transaction,
        job_id,
        JobState::Interrupted,
        Some(error.code()),
        Some(&message),
        &now,
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn recover_locked(writer: &WorkspaceWriter) -> Result<RecoveryReport> {
    let receiving_preflight_count: i64 = writer.connection.query_row(
        "SELECT COUNT(*)
         FROM engagement_source_preflights
         WHERE state = 'RECEIVING'",
        [],
        |row| row.get(0),
    )?;
    if receiving_preflight_count != 0 {
        return Err(KernelError::Conflict(
            "engagement source preflight recovery requires the identity-bound engagement service"
                .to_owned(),
        ));
    }
    let transaction = writer.connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let active_ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM jobs
             WHERE state IN ('SPOOLING', 'INSPECTING', 'INGESTING', 'RECONCILING')
             ORDER BY id",
        )?;
        let mapped = statement.query_map([], |row| row.get::<_, String>(0))?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for job_id in &active_ids {
        let (source, stored, pending) = durable_record_counters(&transaction, job_id)?;
        transaction.execute(
            "UPDATE jobs
             SET state = 'INTERRUPTED', error_code = 'INTERRUPTED_RECOVERED',
                 error_message = 'prior nonterminal job recovered after interruption',
                 source_records = ?1, stored_records = ?2, pending_records = ?3,
                 updated_at = ?4, completed_at = ?4
             WHERE id = ?5",
            params![source, stored, pending, now, job_id],
        )?;
        insert_event(
            &transaction,
            job_id,
            JobState::Interrupted,
            Some("INTERRUPTED_RECOVERED"),
            Some("prior nonterminal job recovered after interruption"),
            &now,
        )?;
    }

    let active_profiling_ids = {
        let mut statement = transaction
            .prepare("SELECT id FROM profiling_runs WHERE state = 'RUNNING' ORDER BY id")?;
        let mapped = statement.query_map([], |row| row.get::<_, String>(0))?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for run_id in &active_profiling_ids {
        let changed = transaction.execute(
            "UPDATE profiling_runs
             SET state = 'INTERRUPTED', error_code = 'INTERRUPTED_RECOVERED',
                 error_message = 'prior profiling run recovered after interruption',
                 updated_at = ?1, completed_at = ?1
             WHERE id = ?2 AND state = 'RUNNING'",
            params![now, run_id],
        )?;
        if changed != 1 {
            return Err(KernelError::Reconciliation(format!(
                "could not recover profiling run {run_id}; changed {changed} rows"
            )));
        }
        insert_profiling_event(
            &transaction,
            run_id,
            "INTERRUPTED",
            Some("INTERRUPTED_RECOVERED"),
            Some("prior profiling run recovered after interruption"),
            &now,
        )?;
    }

    let active_validation_ids = {
        let mut statement = transaction
            .prepare("SELECT id FROM validation_runs WHERE state = 'RUNNING' ORDER BY id")?;
        let mapped = statement.query_map([], |row| row.get::<_, String>(0))?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for run_id in &active_validation_ids {
        let terminal_hash = crate::validation::terminal_checkpoint_hash(
            &transaction,
            run_id,
            "INTERRUPTED",
            Some("INTERRUPTED_RECOVERED"),
            Some("prior validation run recovered after interruption"),
            &now,
            None,
        )?;
        let changed = transaction.execute(
            "UPDATE validation_runs
             SET state = 'INTERRUPTED', error_code = 'INTERRUPTED_RECOVERED',
                 error_message = 'prior validation run recovered after interruption',
                 terminal_hash = ?1, updated_at = ?2, completed_at = ?2
             WHERE id = ?3 AND state = 'RUNNING'",
            params![terminal_hash, now, run_id],
        )?;
        if changed != 1 {
            return Err(KernelError::Reconciliation(format!(
                "could not recover validation run {run_id}; changed {changed} rows"
            )));
        }
        transaction.execute(
            "INSERT INTO validation_events
                 (run_id, state, error_code, detail, occurred_at)
             VALUES (?1, 'INTERRUPTED', 'INTERRUPTED_RECOVERED',
                     'prior validation run recovered after interruption', ?2)",
            params![run_id, now],
        )?;
    }
    let recovered_decision_attempts =
        crate::decision_service::recover_interrupted_decision_attempts(&transaction, &now)?;
    let recovered_reconciliation_attempts =
        crate::reconciliation_service::recover_interrupted_reconciliations(&transaction, &now)?;
    transaction.commit()?;

    let mut cleanup = NamespaceCleanupReport::default();
    for namespace in [
        CleanupNamespace::Packages,
        CleanupNamespace::Metadata,
        CleanupNamespace::Catalogues,
    ] {
        let namespace_cleanup = recover_namespace_partials(writer, namespace)?;
        cleanup.removed_live_partials += namespace_cleanup.removed_live_partials;
        cleanup.quarantined_partials += namespace_cleanup.quarantined_partials;
        cleanup.deleted_tombstones += namespace_cleanup.deleted_tombstones;
        cleanup
            .cleanup_pending
            .extend(namespace_cleanup.cleanup_pending);
    }
    Ok(RecoveryReport {
        workspace: writer.root.clone(),
        recovered_jobs: active_ids.len(),
        recovered_profiling_runs: active_profiling_ids.len(),
        recovered_validation_runs: active_validation_ids.len(),
        recovered_decision_attempts,
        recovered_reconciliation_attempts,
        removed_partial_files: cleanup.removed_live_partials,
        quarantined_partial_files: cleanup.quarantined_partials,
        deleted_tombstones: cleanup.deleted_tombstones,
        cleanup_pending: cleanup
            .cleanup_pending
            .into_iter()
            .map(CleanupPendingInternal::into_public)
            .collect(),
    })
}

pub(crate) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn sqlite_integer(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        KernelError::Reconciliation(format!("{label} does not fit SQLite INTEGER: {value}"))
    })
}

/// Reject new work that depends on a source job whose package cleanup is in
/// an explicit recovery conflict. Inert quarantined or cleanup-pending
/// tombstones do not block because they are already outside the live source
/// namespace.
pub(crate) fn ensure_package_cleanup_not_conflicted(
    connection: &Connection,
    job_id: &str,
) -> Result<()> {
    let conflicted: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM partial_cleanups
             WHERE namespace = 'packages' AND attempt_id = ?1 AND state = 'CONFLICT'
         )",
        [job_id],
        |row| row.get(0),
    )?;
    if conflicted {
        return Err(KernelError::Conflict(format!(
            "source job {job_id} has an unresolved package recovery conflict"
        )));
    }
    Ok(())
}

fn optional_sqlite_integer(value: Option<u64>, label: &str) -> Result<Option<i64>> {
    value.map(|value| sqlite_integer(value, label)).transpose()
}

fn durable_record_counters(transaction: &Transaction<'_>, job_id: &str) -> Result<(i64, i64, i64)> {
    let (source, csv_stored): (i64, i64) = transaction.query_row(
        "SELECT COALESCE(SUM(source_records), 0), COALESCE(SUM(stored_records), 0)
         FROM csv_files WHERE job_id = ?1",
        [job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let stored: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    if source < stored || csv_stored != stored {
        return Err(KernelError::Reconciliation(format!(
            "job {job_id} durable counters do not reconcile: CSV source/stored {source}/{csv_stored}, rows {stored}"
        )));
    }
    Ok((source, stored, source - stored))
}

fn configure_writer(connection: &Connection) -> Result<()> {
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        connection.pragma_update(None, "journal_mode", "WAL")?;
    }
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate(connection: &Connection) -> Result<()> {
    let existing: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if existing > SCHEMA_VERSION {
        return Err(KernelError::Reconciliation(format!(
            "workspace schema version {existing} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if existing == SCHEMA_VERSION {
        return Ok(());
    }
    let mut current = existing;
    if current == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS jobs (
             id TEXT PRIMARY KEY NOT NULL,
             state TEXT NOT NULL CHECK (state IN (
                 'SPOOLING', 'INSPECTING', 'INGESTING', 'RECONCILING',
                 'COMPLETE', 'FAILED', 'INTERRUPTED'
             )),
             source_name TEXT NOT NULL,
             source_path TEXT NOT NULL,
             source_hash TEXT CHECK (source_hash IS NULL OR length(source_hash) = 64),
             source_size INTEGER CHECK (source_size IS NULL OR source_size >= 0),
             stored_archive TEXT,
             policy_json TEXT NOT NULL,
             manifest_entries INTEGER NOT NULL DEFAULT 0 CHECK (manifest_entries >= 0),
             csv_files INTEGER NOT NULL DEFAULT 0 CHECK (csv_files >= 0),
             expanded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (expanded_bytes >= 0),
             source_records INTEGER NOT NULL DEFAULT 0 CHECK (source_records >= 0),
             stored_records INTEGER NOT NULL DEFAULT 0 CHECK (stored_records >= 0),
             pending_records INTEGER NOT NULL DEFAULT 0 CHECK (pending_records >= 0),
             error_code TEXT,
             error_message TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             completed_at TEXT,
             elapsed_ms INTEGER CHECK (elapsed_ms IS NULL OR elapsed_ms >= 0),
             peak_rss_bytes INTEGER CHECK (peak_rss_bytes IS NULL OR peak_rss_bytes >= 0),
             declared_buffer_bytes INTEGER CHECK (
                 declared_buffer_bytes IS NULL OR declared_buffer_bytes >= 0
             ),
             CHECK (state <> 'COMPLETE' OR (
                 source_hash IS NOT NULL AND source_size IS NOT NULL
                 AND source_records = stored_records AND pending_records = 0
                 AND error_code IS NULL AND completed_at IS NOT NULL
             )),
             CHECK (state NOT IN ('FAILED', 'INTERRUPTED') OR error_code IS NOT NULL)
         );
         CREATE TABLE IF NOT EXISTS job_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
             state TEXT NOT NULL,
             error_code TEXT,
             detail TEXT,
             occurred_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS archive_entries (
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             is_directory INTEGER NOT NULL CHECK (is_directory IN (0, 1)),
             is_csv INTEGER NOT NULL CHECK (is_csv IN (0, 1)),
             compressed_size INTEGER NOT NULL CHECK (compressed_size >= 0),
             expanded_size INTEGER NOT NULL CHECK (expanded_size >= 0),
             crc32 INTEGER NOT NULL CHECK (crc32 >= 0),
             PRIMARY KEY (job_id, path)
         );
         CREATE TABLE IF NOT EXISTS csv_files (
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             header_json TEXT,
             source_records INTEGER NOT NULL DEFAULT 0 CHECK (source_records >= 0),
             stored_records INTEGER NOT NULL DEFAULT 0 CHECK (stored_records >= 0),
             PRIMARY KEY (job_id, path),
             FOREIGN KEY (job_id, path) REFERENCES archive_entries(job_id, path)
         );
         CREATE TABLE IF NOT EXISTS source_records (
             record_id INTEGER PRIMARY KEY AUTOINCREMENT,
             job_id TEXT NOT NULL,
             file_path TEXT NOT NULL,
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             fields_json TEXT NOT NULL,
             UNIQUE (job_id, file_path, source_ordinal),
             FOREIGN KEY (job_id, file_path) REFERENCES csv_files(job_id, path)
         );
         CREATE INDEX IF NOT EXISTS source_records_job
             ON source_records(job_id, file_path, source_ordinal);
         CREATE TABLE IF NOT EXISTS metadata_snapshots (
             id TEXT PRIMARY KEY NOT NULL,
             artifact_name TEXT NOT NULL,
             immutable_path TEXT NOT NULL,
             sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
             kind TEXT NOT NULL CHECK (kind = 'odata_edmx'),
             byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
             system_context TEXT,
             client_context TEXT,
             release_context TEXT,
             imported_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS parser_runs (
             snapshot_id TEXT NOT NULL REFERENCES metadata_snapshots(id) ON DELETE RESTRICT,
             parser_name TEXT NOT NULL,
             parser_version TEXT NOT NULL,
             options_json TEXT NOT NULL,
             outcome TEXT NOT NULL,
             catalogue_path TEXT,
             catalogue_sha256 TEXT CHECK (
                 catalogue_sha256 IS NULL OR length(catalogue_sha256) = 64
             ),
             evidence_json TEXT NOT NULL,
             started_at TEXT NOT NULL,
             completed_at TEXT NOT NULL,
             PRIMARY KEY (snapshot_id, parser_name, parser_version, started_at),
             CHECK ((catalogue_path IS NULL) = (catalogue_sha256 IS NULL))
         );
         PRAGMA user_version = 1;
         COMMIT;",
        )?;
        current = 1;
    }
    if current == 1 {
        migrate_v1_to_v2(connection)?;
        current = 2;
    }
    if current == 2 {
        migrate_v2_to_v3(connection)?;
        current = 3;
    }
    if current == 3 {
        migrate_v3_to_v4(connection)?;
        current = 4;
    }
    if current == 4 {
        migrate_v4_to_v5(connection)?;
        current = 5;
    }
    if current == 5 {
        migrate_v5_to_v6(connection)?;
        current = 6;
    }
    if current == 6 {
        migrate_v6_to_v7(connection)?;
        current = 7;
    }
    if current == 7 {
        migrate_v7_to_v8(connection)?;
        current = 8;
    }
    if current == 8 {
        migrate_v8_to_v9(connection)?;
        current = 9;
    }
    if current == 9 {
        migrate_v9_to_v10(connection)?;
        current = 10;
    }
    if current == 10 {
        migrate_v10_to_v11(connection)?;
        current = 11;
    }
    if current == 11 {
        migrate_v11_to_v12(connection)?;
        current = 12;
    }
    if current == 12 {
        migrate_v12_to_v13(connection)?;
        current = 13;
    }
    if current == 13 {
        migrate_v13_to_v14(connection)?;
        current = 14;
    }
    if current != SCHEMA_VERSION {
        return Err(KernelError::Reconciliation(format!(
            "workspace migration stopped at schema version {current}; expected {SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v1_to_v2(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE jobs ADD COLUMN peak_database_bytes INTEGER
             CHECK (peak_database_bytes IS NULL OR peak_database_bytes >= 0);
         ALTER TABLE jobs ADD COLUMN peak_wal_bytes INTEGER
             CHECK (peak_wal_bytes IS NULL OR peak_wal_bytes >= 0);
         ALTER TABLE jobs ADD COLUMN peak_shm_bytes INTEGER
             CHECK (peak_shm_bytes IS NULL OR peak_shm_bytes >= 0);

         CREATE TABLE profiling_runs (
             id TEXT PRIMARY KEY NOT NULL,
             identity_hash TEXT NOT NULL CHECK (length(identity_hash) = 64),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             profiler_version TEXT NOT NULL,
             policy_json TEXT NOT NULL,
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             state TEXT NOT NULL CHECK (state IN (
                 'RUNNING', 'COMPLETE', 'FAILED', 'INTERRUPTED'
             )),
             result_json TEXT,
             result_hash TEXT CHECK (result_hash IS NULL OR length(result_hash) = 64),
             files_processed INTEGER NOT NULL DEFAULT 0 CHECK (files_processed >= 0),
             rows_processed INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
             columns_processed INTEGER NOT NULL DEFAULT 0 CHECK (columns_processed >= 0),
             elapsed_ms INTEGER CHECK (elapsed_ms IS NULL OR elapsed_ms >= 0),
             peak_rss_bytes INTEGER CHECK (peak_rss_bytes IS NULL OR peak_rss_bytes >= 0),
             error_code TEXT,
             error_message TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             completed_at TEXT,
             CHECK ((result_json IS NULL) = (result_hash IS NULL)),
             CHECK (
                 (state = 'RUNNING' AND result_json IS NULL
                     AND error_code IS NULL AND completed_at IS NULL)
                 OR (state = 'COMPLETE' AND result_json IS NOT NULL
                     AND error_code IS NULL AND completed_at IS NOT NULL)
                 OR (state IN ('FAILED', 'INTERRUPTED') AND result_json IS NULL
                     AND error_code IS NOT NULL AND completed_at IS NOT NULL)
             )
         );
         CREATE INDEX profiling_runs_identity
             ON profiling_runs(identity_hash, state);
         CREATE INDEX profiling_runs_job
             ON profiling_runs(job_id, state);
         CREATE INDEX profiling_runs_source
             ON profiling_runs(source_hash);

         CREATE TABLE profiling_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             run_id TEXT NOT NULL REFERENCES profiling_runs(id) ON DELETE CASCADE,
             state TEXT NOT NULL CHECK (state IN (
                 'RUNNING', 'COMPLETE', 'FAILED', 'INTERRUPTED'
             )),
             error_code TEXT,
             detail TEXT,
             occurred_at TEXT NOT NULL
         );
         CREATE INDEX profiling_events_run
             ON profiling_events(run_id, sequence);

         CREATE TABLE profiling_files (
             run_id TEXT NOT NULL REFERENCES profiling_runs(id) ON DELETE CASCADE,
             path TEXT NOT NULL,
             row_count INTEGER NOT NULL CHECK (row_count >= 0),
             column_count INTEGER NOT NULL CHECK (column_count >= 0),
             duplicate_keys_json TEXT NOT NULL,
             PRIMARY KEY (run_id, path)
         );

         CREATE TABLE profiling_columns (
             run_id TEXT NOT NULL,
             path TEXT NOT NULL,
             column_ordinal INTEGER NOT NULL CHECK (column_ordinal >= 0),
             column_name TEXT NOT NULL,
             empty_count INTEGER NOT NULL CHECK (empty_count >= 0),
             nonempty_count INTEGER NOT NULL CHECK (nonempty_count >= 0),
             min_decoded_length INTEGER CHECK (
                 min_decoded_length IS NULL OR min_decoded_length >= 0
             ),
             max_decoded_length INTEGER CHECK (
                 max_decoded_length IS NULL OR max_decoded_length >= min_decoded_length
             ),
             oversized_count INTEGER NOT NULL CHECK (oversized_count >= 0),
             distinct_mode TEXT NOT NULL CHECK (distinct_mode IN (
                 'EXACT', 'NOT_COMPUTED_DUE_TO_BOUND'
             )),
             distinct_count INTEGER CHECK (distinct_count IS NULL OR distinct_count >= 0),
             samples_json TEXT NOT NULL,
             PRIMARY KEY (run_id, path, column_ordinal),
             FOREIGN KEY (run_id, path)
                 REFERENCES profiling_files(run_id, path) ON DELETE CASCADE,
             CHECK (
                 (distinct_mode = 'EXACT' AND distinct_count IS NOT NULL)
                 OR (distinct_mode = 'NOT_COMPUTED_DUE_TO_BOUND' AND distinct_count IS NULL)
             ),
             CHECK (
                 (nonempty_count = 0 AND min_decoded_length IS NULL
                     AND max_decoded_length IS NULL)
                 OR (nonempty_count > 0 AND min_decoded_length IS NOT NULL
                     AND max_decoded_length IS NOT NULL)
             )
         );
         PRAGMA user_version = 2;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_v2_to_v3(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         DROP INDEX IF EXISTS source_records_job;
         PRAGMA user_version = 3;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_v3_to_v4(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE partial_cleanups (
             cleanup_id TEXT PRIMARY KEY NOT NULL,
             namespace TEXT NOT NULL CHECK (namespace IN (
                 'packages', 'metadata', 'catalogues'
             )),
             attempt_id TEXT NOT NULL,
             job_id TEXT REFERENCES jobs(id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (state IN (
                 'PLANNED', 'QUARANTINED', 'CLEANUP_PENDING', 'CLEANED', 'CONFLICT'
             )),
             cleanup_attempts INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_attempts >= 0),
             last_error_code TEXT,
             last_error_message TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             quarantined_at TEXT,
             cleaned_at TEXT,
             UNIQUE (namespace, attempt_id),
             CHECK (
                 (namespace = 'packages' AND job_id = attempt_id)
                 OR (namespace IN ('metadata', 'catalogues') AND job_id IS NULL)
             ),
             CHECK (
                 (state = 'PLANNED' AND quarantined_at IS NULL AND cleaned_at IS NULL)
                 OR (state IN ('QUARANTINED', 'CLEANUP_PENDING')
                     AND quarantined_at IS NOT NULL AND cleaned_at IS NULL)
                 OR (state = 'CLEANED'
                     AND quarantined_at IS NOT NULL AND cleaned_at IS NOT NULL)
                 OR state = 'CONFLICT'
             )
         );
         CREATE INDEX partial_cleanups_state
             ON partial_cleanups(namespace, state, cleanup_id);
         PRAGMA user_version = 4;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v4_to_v5(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE profile_versions (
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             schema_hash TEXT NOT NULL CHECK (length(schema_hash) = 64),
             document_json TEXT NOT NULL,
             declared_status TEXT NOT NULL CHECK (declared_status IN (
                 'GENERATED_DRAFT', 'REVIEWED_DRAFT', 'APPROVED', 'DEPRECATED', 'RETIRED'
             )),
             validation_hash TEXT NOT NULL CHECK (length(validation_hash) = 64),
             registered_at TEXT NOT NULL,
             PRIMARY KEY (profile_id, profile_version),
             UNIQUE (profile_id, profile_version, profile_hash)
         );
         CREATE TRIGGER profile_versions_no_update
             BEFORE UPDATE ON profile_versions
             BEGIN
                 SELECT RAISE(ABORT, 'profile version history is immutable');
             END;
         CREATE TRIGGER profile_versions_no_delete
             BEFORE DELETE ON profile_versions
             BEGIN
                 SELECT RAISE(ABORT, 'profile version history is immutable');
             END;

         CREATE TABLE profile_approvals (
             approval_id TEXT PRIMARY KEY NOT NULL,
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             approver TEXT NOT NULL,
             reason TEXT NOT NULL,
             acknowledged INTEGER NOT NULL CHECK (acknowledged = 1),
             approved_at TEXT NOT NULL,
             approval_hash TEXT NOT NULL UNIQUE CHECK (length(approval_hash) = 64),
             UNIQUE (profile_id, profile_version, profile_hash),
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT
         );
         CREATE TRIGGER profile_approvals_no_update
             BEFORE UPDATE ON profile_approvals
             BEGIN
                 SELECT RAISE(ABORT, 'profile approval history is immutable');
             END;
         CREATE TRIGGER profile_approvals_no_delete
             BEFORE DELETE ON profile_approvals
             BEGIN
                 SELECT RAISE(ABORT, 'profile approval history is immutable');
             END;

         CREATE TABLE profile_lifecycle_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             from_state TEXT,
             to_state TEXT NOT NULL CHECK (to_state IN (
                 'GENERATED_DRAFT', 'REVIEWED_DRAFT', 'APPROVED', 'DEPRECATED', 'RETIRED'
             )),
             actor TEXT NOT NULL,
             reason TEXT NOT NULL,
             previous_event_hash TEXT CHECK (
                 previous_event_hash IS NULL OR length(previous_event_hash) = 64
             ),
             event_hash TEXT NOT NULL UNIQUE CHECK (length(event_hash) = 64),
             occurred_at TEXT NOT NULL,
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT
         );
         CREATE TRIGGER profile_lifecycle_events_no_update
             BEFORE UPDATE ON profile_lifecycle_events
             BEGIN
                 SELECT RAISE(ABORT, 'profile lifecycle history is immutable');
             END;
         CREATE TRIGGER profile_lifecycle_events_no_delete
             BEFORE DELETE ON profile_lifecycle_events
             BEGIN
                 SELECT RAISE(ABORT, 'profile lifecycle history is immutable');
             END;
         CREATE INDEX profile_lifecycle_identity
             ON profile_lifecycle_events(profile_id, profile_version, sequence);

         CREATE TABLE mapping_validation_attempts (
             id TEXT PRIMARY KEY NOT NULL,
             input_hash TEXT NOT NULL CHECK (length(input_hash) = 64),
             state TEXT NOT NULL CHECK (state IN ('VALID', 'INVALID')),
             mapping_id TEXT,
             mapping_version TEXT,
             mapping_hash TEXT CHECK (mapping_hash IS NULL OR length(mapping_hash) = 64),
             errors_json TEXT NOT NULL,
             created_at TEXT NOT NULL,
             attempt_hash TEXT NOT NULL UNIQUE CHECK (length(attempt_hash) = 64)
         );
         CREATE TRIGGER mapping_validation_attempts_no_update
             BEFORE UPDATE ON mapping_validation_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'mapping validation attempt history is immutable');
             END;
         CREATE TRIGGER mapping_validation_attempts_no_delete
             BEFORE DELETE ON mapping_validation_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'mapping validation attempt history is immutable');
             END;
         CREATE INDEX mapping_attempts_input
             ON mapping_validation_attempts(input_hash, created_at, id);

         CREATE TABLE mapping_versions (
             mapping_id TEXT NOT NULL,
             mapping_version TEXT NOT NULL,
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             attempt_id TEXT NOT NULL REFERENCES mapping_validation_attempts(id) ON DELETE RESTRICT,
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             validated_job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             document_json TEXT NOT NULL,
             coverage_json TEXT NOT NULL,
             coverage_hash TEXT NOT NULL CHECK (length(coverage_hash) = 64),
             created_at TEXT NOT NULL,
             PRIMARY KEY (mapping_id, mapping_version),
             UNIQUE (mapping_id, mapping_version, mapping_hash),
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT
         );
         CREATE TRIGGER mapping_versions_no_update
             BEFORE UPDATE ON mapping_versions
             BEGIN
                 SELECT RAISE(ABORT, 'mapping version history is immutable');
             END;
         CREATE TRIGGER mapping_versions_no_delete
             BEFORE DELETE ON mapping_versions
             BEGIN
                 SELECT RAISE(ABORT, 'mapping version history is immutable');
             END;
         CREATE INDEX mappings_profile
             ON mapping_versions(profile_id, profile_version, profile_hash);
         CREATE INDEX mappings_source
             ON mapping_versions(source_hash, mapping_id, mapping_version);

         CREATE TABLE validation_runs (
             id TEXT PRIMARY KEY NOT NULL,
             identity_hash TEXT NOT NULL CHECK (length(identity_hash) = 64),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             mapping_id TEXT NOT NULL,
             mapping_version TEXT NOT NULL,
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             rule_set_hash TEXT NOT NULL CHECK (length(rule_set_hash) = 64),
             engine_version TEXT NOT NULL,
             policy_json TEXT NOT NULL,
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             state TEXT NOT NULL CHECK (state IN (
                 'RUNNING', 'COMPLETE', 'FAILED', 'INTERRUPTED', 'BOUND_EXCEEDED'
             )),
             result_json TEXT,
             result_hash TEXT CHECK (result_hash IS NULL OR length(result_hash) = 64),
             terminal_hash TEXT CHECK (terminal_hash IS NULL OR length(terminal_hash) = 64),
             issue_count INTEGER NOT NULL DEFAULT 0 CHECK (issue_count >= 0),
             rows_processed INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
             error_code TEXT,
             error_message TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             completed_at TEXT,
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(mapping_id, mapping_version, mapping_hash)
                 ON DELETE RESTRICT,
             CHECK ((result_json IS NULL) = (result_hash IS NULL)),
             CHECK (
                 (state = 'RUNNING' AND result_json IS NULL
                     AND terminal_hash IS NULL
                     AND error_code IS NULL AND completed_at IS NULL)
                 OR (state = 'COMPLETE' AND result_json IS NOT NULL
                     AND terminal_hash IS NOT NULL
                     AND error_code IS NULL AND completed_at IS NOT NULL)
                 OR (state IN ('FAILED', 'INTERRUPTED', 'BOUND_EXCEEDED')
                     AND result_json IS NULL AND terminal_hash IS NOT NULL
                     AND error_code IS NOT NULL
                     AND completed_at IS NOT NULL)
             )
         );
         CREATE TRIGGER validation_terminal_hash_no_update
             BEFORE UPDATE OF terminal_hash ON validation_runs
             WHEN OLD.terminal_hash IS NOT NULL
             BEGIN
                 SELECT RAISE(ABORT, 'validation terminal checkpoint is immutable');
             END;
         CREATE TRIGGER validation_terminal_run_no_delete
             BEFORE DELETE ON validation_runs
             WHEN OLD.terminal_hash IS NOT NULL
             BEGIN
                 SELECT RAISE(ABORT, 'validation terminal history is immutable');
             END;
         CREATE INDEX validation_runs_identity
             ON validation_runs(identity_hash, state, id);
         CREATE INDEX validation_runs_job
             ON validation_runs(job_id, state, id);
         CREATE INDEX validation_runs_mapping
             ON validation_runs(mapping_id, mapping_version, state, id);

         CREATE TABLE validation_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             run_id TEXT NOT NULL REFERENCES validation_runs(id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (state IN (
                 'RUNNING', 'COMPLETE', 'FAILED', 'INTERRUPTED', 'BOUND_EXCEEDED'
             )),
             error_code TEXT,
             detail TEXT,
             occurred_at TEXT NOT NULL
         );
         CREATE INDEX validation_events_run
             ON validation_events(run_id, sequence);

         CREATE TABLE validation_issues (
             run_id TEXT NOT NULL REFERENCES validation_runs(id) ON DELETE RESTRICT,
             issue_id TEXT NOT NULL,
             severity TEXT NOT NULL CHECK (severity IN ('ERROR', 'WARNING', 'INFORMATION')),
             rule_id TEXT NOT NULL,
             issue_code TEXT NOT NULL,
             display_message TEXT NOT NULL,
             entity_id TEXT NOT NULL,
             field_id TEXT NOT NULL,
             source_file TEXT NOT NULL,
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             lineage_json TEXT NOT NULL,
             issue_hash TEXT NOT NULL CHECK (length(issue_hash) = 64),
             PRIMARY KEY (run_id, issue_id)
         );
         CREATE INDEX validation_issues_row
             ON validation_issues(run_id, source_file, source_ordinal, issue_id);
         CREATE INDEX validation_issues_filters
             ON validation_issues(run_id, severity, rule_id, entity_id, field_id,
                                  source_file, source_ordinal, issue_id);

         PRAGMA user_version = 5;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v5_to_v6(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE UNIQUE INDEX validation_runs_wp005_scope
             ON validation_runs(
                 id, job_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash
             );
         CREATE UNIQUE INDEX validation_issues_wp005_scope
             ON validation_issues(
                 run_id, issue_id, issue_hash,
                 entity_id, field_id, source_file, source_ordinal,
                 line_start, line_end
             );

         CREATE TABLE decision_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 4194304
             ),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             validation_run_id TEXT NOT NULL CHECK (
                 length(CAST(validation_run_id AS BLOB)) BETWEEN 1 AND 256
             ),
             issue_id TEXT NOT NULL CHECK (
                 length(CAST(issue_id AS BLOB)) BETWEEN 1 AND 256
             ),
             issue_hash TEXT NOT NULL CHECK (length(issue_hash) = 64),
             accepted_source_file TEXT NOT NULL CHECK (
                 length(CAST(accepted_source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             canonical_entity_id TEXT NOT NULL CHECK (
                 length(CAST(canonical_entity_id AS BLOB)) BETWEEN 1 AND 256
             ),
             canonical_field_id TEXT NOT NULL CHECK (
                 length(CAST(canonical_field_id AS BLOB)) BETWEEN 1 AND 256
             ),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 256
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 256
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 256
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 256
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             disposition TEXT NOT NULL CHECK (disposition IN (
                 'CORRECTED', 'WAIVED', 'ACCEPTED_AS_IS', 'EXCLUDED'
             )),
             practitioner_id TEXT NOT NULL CHECK (
                 length(CAST(practitioner_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             evidence_reference TEXT CHECK (
                 evidence_reference IS NULL
                 OR length(CAST(evidence_reference AS BLOB)) BETWEEN 1 AND 2048
             ),
             authority_separation_acknowledged INTEGER NOT NULL CHECK (
                 authority_separation_acknowledged = 1
             ),
             row_scope_exclusion_acknowledged INTEGER NOT NULL CHECK (
                 row_scope_exclusion_acknowledged IN (0, 1)
             ),
             expected_active_decision_id TEXT CHECK (
                 expected_active_decision_id IS NULL
                 OR length(CAST(expected_active_decision_id AS BLOB)) BETWEEN 1 AND 256
             ),
             expected_active_generation INTEGER NOT NULL CHECK (
                 expected_active_generation >= 0
             ),
             expected_effective_value_hash TEXT CHECK (
                 expected_effective_value_hash IS NULL
                 OR length(expected_effective_value_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (job_id, idempotency_key),
             UNIQUE (
                 attempt_id, job_id, validation_run_id, issue_id, disposition
             ),
             UNIQUE (
                 attempt_id, request_hash, disposition, expected_effective_value_hash,
                 practitioner_id, reason
             ),
             UNIQUE (
                 attempt_id, job_id, validation_run_id, issue_id, disposition,
                 practitioner_id, reason
             ),
             UNIQUE (
                 attempt_id, expected_active_decision_id, expected_active_generation
             ),
             FOREIGN KEY (
                 validation_run_id, job_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash
             ) REFERENCES validation_runs(
                 id, job_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 validation_run_id, issue_id, issue_hash,
                 canonical_entity_id, canonical_field_id,
                 accepted_source_file, source_ordinal, line_start, line_end
             ) REFERENCES validation_issues(
                 run_id, issue_id, issue_hash,
                 entity_id, field_id, source_file, source_ordinal,
                 line_start, line_end
             ) ON DELETE RESTRICT,
             FOREIGN KEY (job_id, accepted_source_file, source_ordinal)
                 REFERENCES source_records(job_id, file_path, source_ordinal)
                 ON DELETE RESTRICT,
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_approvals(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(mapping_id, mapping_version, mapping_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (expected_active_decision_id, expected_active_generation)
                 REFERENCES issue_decisions(decision_id, generation)
                 ON DELETE RESTRICT,
             CHECK (
                 (expected_active_decision_id IS NULL AND expected_active_generation = 0)
                 OR (expected_active_decision_id IS NOT NULL
                     AND expected_active_generation > 0)
             ),
             CHECK (
                 (disposition = 'EXCLUDED' AND row_scope_exclusion_acknowledged = 1)
                 OR (disposition <> 'EXCLUDED'
                     AND row_scope_exclusion_acknowledged = 0)
             ),
             CHECK (
                 (disposition IN ('CORRECTED', 'ACCEPTED_AS_IS')
                     AND expected_effective_value_hash IS NOT NULL)
                 OR (disposition IN ('WAIVED', 'EXCLUDED')
                     AND expected_effective_value_hash IS NULL)
             )
         );
         CREATE TRIGGER decision_attempts_no_update
             BEFORE UPDATE ON decision_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'decision attempt history is immutable');
             END;
         CREATE TRIGGER decision_attempts_no_delete
             BEFORE DELETE ON decision_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'decision attempt history is immutable');
             END;
         CREATE INDEX decision_attempts_issue_history
             ON decision_attempts(
                 validation_run_id, issue_id, created_at, attempt_id
             );

         CREATE TABLE decision_attempt_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             attempt_id TEXT NOT NULL
                 REFERENCES decision_attempts(attempt_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (state IN (
                 'RECEIVED', 'VALIDATED', 'REJECTED', 'CONFLICT',
                 'OVERLAY_PERSISTED',
                 'REEVALUATION_PLANNED', 'REEVALUATION_RUNNING',
                 'REEVALUATION_COMPLETE', 'REEVALUATION_INTERRUPTED',
                 'REEVALUATION_FAILED', 'REEVALUATION_BOUND_EXCEEDED',
                 'REEVALUATION_INTEGRITY_BLOCKED', 'REEVALUATION_STALE_INPUT',
                 'EFFECTIVE'
             )),
             error_code TEXT CHECK (
                 error_code IS NULL
                 OR length(CAST(error_code AS BLOB)) BETWEEN 1 AND 128
             ),
             detail TEXT CHECK (
                 detail IS NULL OR length(CAST(detail AS BLOB)) <= 4096
             ),
             previous_event_hash TEXT CHECK (
                 previous_event_hash IS NULL OR length(previous_event_hash) = 64
             ),
             event_hash TEXT NOT NULL UNIQUE CHECK (length(event_hash) = 64),
             occurred_at TEXT NOT NULL CHECK (
                 length(CAST(occurred_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER decision_attempt_events_no_update
             BEFORE UPDATE ON decision_attempt_events
             BEGIN
                 SELECT RAISE(ABORT, 'decision attempt event history is immutable');
             END;
         CREATE TRIGGER decision_attempt_events_no_delete
             BEFORE DELETE ON decision_attempt_events
             BEGIN
                 SELECT RAISE(ABORT, 'decision attempt event history is immutable');
             END;
         CREATE INDEX decision_attempt_events_history
             ON decision_attempt_events(attempt_id, sequence);

         CREATE TABLE correction_overlays (
             overlay_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(overlay_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             decision_attempt_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(decision_attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             decision_request_hash TEXT NOT NULL CHECK (
                 length(decision_request_hash) = 64
             ),
             disposition TEXT NOT NULL CHECK (disposition = 'CORRECTED'),
             prior_effective_value_hash TEXT NOT NULL CHECK (
                 length(prior_effective_value_hash) = 64
             ),
             operation TEXT NOT NULL CHECK (operation IN (
                 'REPLACE_EFFECTIVE_TEXT', 'SET_EFFECTIVE_EMPTY', 'SET_EFFECTIVE_NULL'
             )),
             replacement_text TEXT CHECK (
                 replacement_text IS NULL
                 OR length(CAST(replacement_text AS BLOB)) <= 1048576
             ),
             practitioner_id TEXT NOT NULL CHECK (
                 length(CAST(practitioner_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             canonical_overlay_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_overlay_json AS BLOB)) BETWEEN 2 AND 4194304
             ),
             overlay_hash TEXT NOT NULL UNIQUE CHECK (length(overlay_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (overlay_id, decision_attempt_id),
             UNIQUE (
                 overlay_id, decision_attempt_id, prior_effective_value_hash
             ),
             FOREIGN KEY (
                 decision_attempt_id, decision_request_hash, disposition,
                 prior_effective_value_hash,
                 practitioner_id, reason
             ) REFERENCES decision_attempts(
                 attempt_id, request_hash, disposition, expected_effective_value_hash,
                 practitioner_id, reason
             ) ON DELETE RESTRICT,
             CHECK (
                 (operation = 'REPLACE_EFFECTIVE_TEXT'
                     AND replacement_text IS NOT NULL
                     AND length(replacement_text) > 0)
                 OR (operation = 'SET_EFFECTIVE_EMPTY' AND replacement_text = '')
                 OR (operation = 'SET_EFFECTIVE_NULL' AND replacement_text IS NULL)
             )
         );
         CREATE TRIGGER correction_overlays_no_update
             BEFORE UPDATE ON correction_overlays
             BEGIN
                 SELECT RAISE(ABORT, 'correction overlay history is immutable');
             END;
         CREATE TRIGGER correction_overlays_no_delete
             BEFORE DELETE ON correction_overlays
             BEGIN
                 SELECT RAISE(ABORT, 'correction overlay history is immutable');
             END;

         CREATE TABLE reevaluation_runs (
             reevaluation_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(reevaluation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             identity_hash TEXT NOT NULL UNIQUE CHECK (length(identity_hash) = 64),
             decision_attempt_id TEXT NOT NULL CHECK (
                 length(CAST(decision_attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             correction_overlay_id TEXT NOT NULL CHECK (
                 length(CAST(correction_overlay_id AS BLOB)) BETWEEN 1 AND 256
             ),
             disposition TEXT NOT NULL CHECK (disposition = 'CORRECTED'),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_validation_run_id TEXT NOT NULL CHECK (
                 length(CAST(source_validation_run_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_issue_id TEXT NOT NULL CHECK (
                 length(CAST(source_issue_id AS BLOB)) BETWEEN 1 AND 256
             ),
             rule_set_hash TEXT NOT NULL CHECK (length(rule_set_hash) = 64),
             engine_version TEXT NOT NULL CHECK (
                 length(CAST(engine_version AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_json TEXT NOT NULL CHECK (
                 length(CAST(policy_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             dependency_scope_json TEXT NOT NULL CHECK (
                 length(CAST(dependency_scope_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             dependency_scope_hash TEXT NOT NULL CHECK (
                 length(dependency_scope_hash) = 64
             ),
             state TEXT NOT NULL CHECK (state IN (
                 'PLANNED', 'RUNNING', 'COMPLETE', 'INTERRUPTED', 'FAILED',
                 'BOUND_EXCEEDED', 'INTEGRITY_BLOCKED', 'STALE_INPUT'
             )),
             result_json TEXT CHECK (
                 result_json IS NULL
                 OR length(CAST(result_json AS BLOB)) BETWEEN 2 AND 67108864
             ),
             result_hash TEXT CHECK (result_hash IS NULL OR length(result_hash) = 64),
             terminal_hash TEXT CHECK (terminal_hash IS NULL OR length(terminal_hash) = 64),
             issue_count INTEGER NOT NULL DEFAULT 0 CHECK (issue_count >= 0),
             rows_processed INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
             error_code TEXT CHECK (
                 error_code IS NULL
                 OR length(CAST(error_code AS BLOB)) BETWEEN 1 AND 128
             ),
             error_message TEXT CHECK (
                 error_message IS NULL OR length(CAST(error_message AS BLOB)) <= 4096
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             completed_at TEXT CHECK (
                 completed_at IS NULL
                 OR length(CAST(completed_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 reevaluation_id, state, decision_attempt_id, correction_overlay_id
             ),
             FOREIGN KEY (
                 decision_attempt_id, job_id, source_validation_run_id,
                 source_issue_id, disposition
             ) REFERENCES decision_attempts(
                 attempt_id, job_id, validation_run_id, issue_id, disposition
             ) ON DELETE RESTRICT,
             FOREIGN KEY (correction_overlay_id, decision_attempt_id)
                 REFERENCES correction_overlays(overlay_id, decision_attempt_id)
                 ON DELETE RESTRICT,
             CHECK ((result_json IS NULL) = (result_hash IS NULL)),
             CHECK (
                 (state IN ('PLANNED', 'RUNNING')
                     AND result_json IS NULL AND terminal_hash IS NULL
                     AND error_code IS NULL AND completed_at IS NULL)
                 OR (state = 'COMPLETE'
                     AND result_json IS NOT NULL AND terminal_hash IS NOT NULL
                     AND error_code IS NULL AND completed_at IS NOT NULL)
                 OR (state IN (
                         'INTERRUPTED', 'FAILED', 'BOUND_EXCEEDED',
                         'INTEGRITY_BLOCKED', 'STALE_INPUT'
                     )
                     AND result_json IS NULL AND terminal_hash IS NOT NULL
                     AND error_code IS NOT NULL AND completed_at IS NOT NULL)
             )
         );
         CREATE TRIGGER reevaluation_runs_no_update
             BEFORE UPDATE ON reevaluation_runs
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation run history is immutable');
             END;
         CREATE TRIGGER reevaluation_runs_no_delete
             BEFORE DELETE ON reevaluation_runs
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation run history is immutable');
             END;
         CREATE INDEX reevaluation_runs_history
             ON reevaluation_runs(decision_attempt_id, created_at, reevaluation_id);

         CREATE TABLE reevaluation_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             reevaluation_id TEXT NOT NULL
                 REFERENCES reevaluation_runs(reevaluation_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (state IN (
                 'PLANNED', 'RUNNING', 'COMPLETE', 'INTERRUPTED', 'FAILED',
                 'BOUND_EXCEEDED', 'INTEGRITY_BLOCKED', 'STALE_INPUT'
             )),
             error_code TEXT CHECK (
                 error_code IS NULL
                 OR length(CAST(error_code AS BLOB)) BETWEEN 1 AND 128
             ),
             detail TEXT CHECK (
                 detail IS NULL OR length(CAST(detail AS BLOB)) <= 4096
             ),
             previous_event_hash TEXT CHECK (
                 previous_event_hash IS NULL OR length(previous_event_hash) = 64
             ),
             event_hash TEXT NOT NULL UNIQUE CHECK (length(event_hash) = 64),
             occurred_at TEXT NOT NULL CHECK (
                 length(CAST(occurred_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER reevaluation_events_no_update
             BEFORE UPDATE ON reevaluation_events
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation event history is immutable');
             END;
         CREATE TRIGGER reevaluation_events_no_delete
             BEFORE DELETE ON reevaluation_events
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation event history is immutable');
             END;
         CREATE INDEX reevaluation_events_history
             ON reevaluation_events(reevaluation_id, sequence);

         CREATE TABLE reevaluation_issues (
             reevaluation_id TEXT NOT NULL
                 REFERENCES reevaluation_runs(reevaluation_id) ON DELETE RESTRICT,
             issue_id TEXT NOT NULL CHECK (
                 length(CAST(issue_id AS BLOB)) BETWEEN 1 AND 256
             ),
             outcome TEXT NOT NULL CHECK (outcome IN ('RESOLVED', 'RESIDUAL', 'NEW')),
             source_validation_run_id TEXT CHECK (
                 source_validation_run_id IS NULL
                 OR length(CAST(source_validation_run_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_issue_id TEXT CHECK (
                 source_issue_id IS NULL
                 OR length(CAST(source_issue_id AS BLOB)) BETWEEN 1 AND 256
             ),
             severity TEXT NOT NULL CHECK (severity IN (
                 'ERROR', 'WARNING', 'INFORMATION'
             )),
             rule_id TEXT NOT NULL CHECK (
                 length(CAST(rule_id AS BLOB)) BETWEEN 1 AND 256
             ),
             issue_code TEXT NOT NULL CHECK (
                 length(CAST(issue_code AS BLOB)) BETWEEN 1 AND 256
             ),
             display_message TEXT NOT NULL CHECK (
                 length(CAST(display_message AS BLOB)) BETWEEN 1 AND 4096
             ),
             entity_id TEXT NOT NULL CHECK (
                 length(CAST(entity_id AS BLOB)) BETWEEN 1 AND 256
             ),
             field_id TEXT NOT NULL CHECK (
                 length(CAST(field_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_file TEXT NOT NULL CHECK (
                 length(CAST(source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             lineage_json TEXT NOT NULL CHECK (
                 length(CAST(lineage_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             issue_hash TEXT NOT NULL CHECK (length(issue_hash) = 64),
             PRIMARY KEY (reevaluation_id, issue_id),
             FOREIGN KEY (source_validation_run_id, source_issue_id)
                 REFERENCES validation_issues(run_id, issue_id) ON DELETE RESTRICT,
             CHECK (
                 (outcome IN ('RESOLVED', 'RESIDUAL')
                     AND source_validation_run_id IS NOT NULL
                     AND source_issue_id IS NOT NULL)
                 OR (outcome = 'NEW'
                     AND source_validation_run_id IS NULL
                     AND source_issue_id IS NULL)
             )
         );
         CREATE TRIGGER reevaluation_issues_no_update
             BEFORE UPDATE ON reevaluation_issues
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation issue history is immutable');
             END;
         CREATE TRIGGER reevaluation_issues_no_delete
             BEFORE DELETE ON reevaluation_issues
             BEGIN
                 SELECT RAISE(ABORT, 're-evaluation issue history is immutable');
             END;

         CREATE TABLE issue_decisions (
             commit_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             decision_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(decision_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             decision_attempt_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(decision_attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             validation_run_id TEXT NOT NULL CHECK (
                 length(CAST(validation_run_id AS BLOB)) BETWEEN 1 AND 256
             ),
             issue_id TEXT NOT NULL CHECK (
                 length(CAST(issue_id AS BLOB)) BETWEEN 1 AND 256
             ),
             disposition TEXT NOT NULL CHECK (disposition IN (
                 'CORRECTED', 'WAIVED', 'ACCEPTED_AS_IS', 'EXCLUDED'
             )),
             predecessor_decision_id TEXT UNIQUE CHECK (
                 predecessor_decision_id IS NULL
                 OR length(CAST(predecessor_decision_id AS BLOB)) BETWEEN 1 AND 256
             ),
             predecessor_generation INTEGER NOT NULL CHECK (
                 predecessor_generation >= 0
             ),
             generation INTEGER NOT NULL CHECK (generation > 0),
             correction_overlay_id TEXT UNIQUE CHECK (
                 correction_overlay_id IS NULL
                 OR length(CAST(correction_overlay_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reevaluation_id TEXT UNIQUE CHECK (
                 reevaluation_id IS NULL
                 OR length(CAST(reevaluation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reevaluation_state TEXT CHECK (
                 reevaluation_state IS NULL OR reevaluation_state = 'COMPLETE'
             ),
             practitioner_id TEXT NOT NULL CHECK (
                 length(CAST(practitioner_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             evidence_reference TEXT CHECK (
                 evidence_reference IS NULL
                 OR length(CAST(evidence_reference AS BLOB)) BETWEEN 1 AND 2048
             ),
             lifecycle_outcome TEXT NOT NULL CHECK (lifecycle_outcome = 'EFFECTIVE'),
             canonical_decision_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_decision_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             decision_hash TEXT NOT NULL UNIQUE CHECK (length(decision_hash) = 64),
             committed_at TEXT NOT NULL CHECK (
                 length(CAST(committed_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (decision_id, generation),
             UNIQUE (validation_run_id, issue_id, generation),
             UNIQUE (
                 decision_id, generation, job_id, validation_run_id, issue_id
             ),
             UNIQUE (
                 decision_id, decision_attempt_id, correction_overlay_id,
                 reevaluation_id, disposition, reevaluation_state, generation
             ),
             FOREIGN KEY (
                 decision_attempt_id, job_id, validation_run_id, issue_id, disposition,
                 practitioner_id, reason
             ) REFERENCES decision_attempts(
                 attempt_id, job_id, validation_run_id, issue_id, disposition,
                 practitioner_id, reason
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 decision_attempt_id, predecessor_decision_id, predecessor_generation
             ) REFERENCES decision_attempts(
                 attempt_id, expected_active_decision_id, expected_active_generation
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 predecessor_decision_id, predecessor_generation,
                 job_id, validation_run_id, issue_id
             ) REFERENCES issue_decisions(
                 decision_id, generation, job_id, validation_run_id, issue_id
             ) ON DELETE RESTRICT,
             FOREIGN KEY (correction_overlay_id, decision_attempt_id)
                 REFERENCES correction_overlays(overlay_id, decision_attempt_id)
                 ON DELETE RESTRICT,
             FOREIGN KEY (
                 reevaluation_id, reevaluation_state,
                 decision_attempt_id, correction_overlay_id
             ) REFERENCES reevaluation_runs(
                 reevaluation_id, state, decision_attempt_id, correction_overlay_id
             ) ON DELETE RESTRICT,
             CHECK (
                 (predecessor_decision_id IS NULL
                     AND predecessor_generation = 0 AND generation = 1)
                 OR (predecessor_decision_id IS NOT NULL
                     AND predecessor_generation > 0
                     AND generation = predecessor_generation + 1)
             ),
             CHECK (
                 (disposition = 'CORRECTED'
                     AND correction_overlay_id IS NOT NULL
                     AND reevaluation_id IS NOT NULL
                     AND reevaluation_state = 'COMPLETE')
                 OR (disposition IN ('WAIVED', 'ACCEPTED_AS_IS', 'EXCLUDED')
                     AND correction_overlay_id IS NULL
                     AND reevaluation_id IS NULL
                     AND reevaluation_state IS NULL)
             )
         );
         CREATE TRIGGER issue_decisions_no_update
             BEFORE UPDATE ON issue_decisions
             BEGIN
                 SELECT RAISE(ABORT, 'issue decision history is immutable');
             END;
         CREATE TRIGGER issue_decisions_no_delete
             BEFORE DELETE ON issue_decisions
             BEGIN
                 SELECT RAISE(ABORT, 'issue decision history is immutable');
             END;
         CREATE INDEX issue_decisions_history
             ON issue_decisions(
                 validation_run_id, issue_id, committed_at, decision_id
             );

         CREATE TABLE correction_effectivations (
             effectivation_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(effectivation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             decision_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(decision_id AS BLOB)) BETWEEN 1 AND 256
             ),
             decision_attempt_id TEXT NOT NULL CHECK (
                 length(CAST(decision_attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             correction_overlay_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(correction_overlay_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reevaluation_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(reevaluation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             decision_disposition TEXT NOT NULL CHECK (
                 decision_disposition = 'CORRECTED'
             ),
             reevaluation_state TEXT NOT NULL CHECK (reevaluation_state = 'COMPLETE'),
             decision_generation INTEGER NOT NULL CHECK (decision_generation > 0),
             original_value_hash TEXT NOT NULL CHECK (length(original_value_hash) = 64),
             prior_effective_value_hash TEXT NOT NULL CHECK (
                 length(prior_effective_value_hash) = 64
             ),
             effective_value_kind TEXT NOT NULL CHECK (effective_value_kind IN (
                 'TEXT', 'EMPTY', 'NULL'
             )),
             effective_value_text TEXT CHECK (
                 effective_value_text IS NULL
                 OR length(CAST(effective_value_text AS BLOB)) <= 1048576
             ),
             effective_value_hash TEXT NOT NULL CHECK (length(effective_value_hash) = 64),
             effective_lineage_json TEXT NOT NULL CHECK (
                 length(CAST(effective_lineage_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             effective_lineage_hash TEXT NOT NULL CHECK (
                 length(effective_lineage_hash) = 64
             ),
             effectivation_hash TEXT NOT NULL UNIQUE CHECK (
                 length(effectivation_hash) = 64
             ),
             effectivated_at TEXT NOT NULL CHECK (
                 length(CAST(effectivated_at AS BLOB)) BETWEEN 1 AND 64
             ),
             FOREIGN KEY (
                 decision_id, decision_attempt_id, correction_overlay_id,
                 reevaluation_id, decision_disposition,
                 reevaluation_state, decision_generation
             ) REFERENCES issue_decisions(
                 decision_id, decision_attempt_id, correction_overlay_id,
                 reevaluation_id, disposition, reevaluation_state, generation
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 correction_overlay_id, decision_attempt_id,
                 prior_effective_value_hash
             ) REFERENCES correction_overlays(
                 overlay_id, decision_attempt_id, prior_effective_value_hash
             )
                 ON DELETE RESTRICT,
             FOREIGN KEY (
                 reevaluation_id, reevaluation_state,
                 decision_attempt_id, correction_overlay_id
             ) REFERENCES reevaluation_runs(
                 reevaluation_id, state, decision_attempt_id, correction_overlay_id
             ) ON DELETE RESTRICT,
             CHECK (
                 (effective_value_kind = 'TEXT'
                     AND effective_value_text IS NOT NULL
                     AND length(effective_value_text) > 0)
                 OR (effective_value_kind = 'EMPTY' AND effective_value_text = '')
                 OR (effective_value_kind = 'NULL' AND effective_value_text IS NULL)
             )
         );
         CREATE TRIGGER correction_effectivations_no_update
             BEFORE UPDATE ON correction_effectivations
             BEGIN
                 SELECT RAISE(ABORT, 'correction effectivation history is immutable');
             END;
         CREATE TRIGGER correction_effectivations_no_delete
             BEFORE DELETE ON correction_effectivations
             BEGIN
                 SELECT RAISE(ABORT, 'correction effectivation history is immutable');
             END;
         CREATE INDEX correction_effectivations_history
             ON correction_effectivations(
                 decision_attempt_id, decision_generation, effectivation_id
             );

         PRAGMA user_version = 6;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v6_to_v7(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE TABLE reconciliation_policy_documents (
             policy_identity TEXT NOT NULL CHECK (
                 length(CAST(policy_identity AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_version TEXT NOT NULL CHECK (
                 length(CAST(policy_version AS BLOB)) BETWEEN 1 AND 64
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             canonicalization_version TEXT NOT NULL CHECK (
                 length(CAST(canonicalization_version AS BLOB)) BETWEEN 1 AND 128
             ),
             engine_version TEXT NOT NULL CHECK (
                 length(CAST(engine_version AS BLOB)) BETWEEN 1 AND 128
             ),
             canonical_policy_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_policy_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             provenance_json TEXT NOT NULL CHECK (
                 length(CAST(provenance_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             provenance_hash TEXT NOT NULL CHECK (length(provenance_hash) = 64),
             registered_by TEXT NOT NULL CHECK (
                 length(CAST(registered_by AS BLOB)) BETWEEN 1 AND 256
             ),
             registration_reason TEXT NOT NULL CHECK (
                 length(CAST(registration_reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             registered_at TEXT NOT NULL CHECK (
                 length(CAST(registered_at AS BLOB)) BETWEEN 1 AND 64
             ),
             PRIMARY KEY (policy_identity, policy_version),
             UNIQUE (policy_identity, policy_version, policy_hash)
         );
         CREATE TRIGGER reconciliation_policy_documents_no_update
             BEFORE UPDATE ON reconciliation_policy_documents
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation policy history is immutable');
             END;
         CREATE TRIGGER reconciliation_policy_documents_no_delete
             BEFORE DELETE ON reconciliation_policy_documents
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation policy history is immutable');
             END;

         CREATE TABLE reconciliation_policy_activations (
             activation_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             activation_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(activation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             policy_identity TEXT NOT NULL CHECK (
                 length(CAST(policy_identity AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_version TEXT NOT NULL CHECK (
                 length(CAST(policy_version AS BLOB)) BETWEEN 1 AND 64
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             predecessor_activation_id TEXT UNIQUE CHECK (
                 predecessor_activation_id IS NULL
                 OR length(CAST(predecessor_activation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             predecessor_generation INTEGER NOT NULL CHECK (
                 predecessor_generation >= 0
             ),
             predecessor_activation_hash TEXT CHECK (
                 predecessor_activation_hash IS NULL
                 OR length(predecessor_activation_hash) = 64
             ),
             observed_generation INTEGER NOT NULL CHECK (observed_generation >= 0),
             generation INTEGER NOT NULL UNIQUE CHECK (generation > 0),
             actor TEXT NOT NULL CHECK (
                 length(CAST(actor AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_activation_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_activation_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             activation_hash TEXT NOT NULL UNIQUE CHECK (length(activation_hash) = 64),
             activated_at TEXT NOT NULL CHECK (
                 length(CAST(activated_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 activation_id, policy_identity, policy_version, policy_hash,
                 activation_hash
             ),
             UNIQUE (activation_id, generation, activation_hash),
             FOREIGN KEY (policy_identity, policy_version, policy_hash)
                 REFERENCES reconciliation_policy_documents(
                     policy_identity, policy_version, policy_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 predecessor_activation_id, predecessor_generation,
                 predecessor_activation_hash
             ) REFERENCES reconciliation_policy_activations(
                 activation_id, generation, activation_hash
             ) ON DELETE RESTRICT,
             CHECK (generation = observed_generation + 1),
             CHECK (
                 (predecessor_activation_id IS NULL
                     AND predecessor_generation = 0
                     AND predecessor_activation_hash IS NULL
                     AND observed_generation = 0
                     AND generation = 1)
                 OR (predecessor_activation_id IS NOT NULL
                     AND predecessor_generation > 0
                     AND predecessor_activation_hash IS NOT NULL
                     AND observed_generation = predecessor_generation
                     AND generation = predecessor_generation + 1)
             )
         );
         CREATE TRIGGER reconciliation_policy_activations_no_update
             BEFORE UPDATE ON reconciliation_policy_activations
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation policy activation history is immutable');
             END;
         CREATE TRIGGER reconciliation_policy_activations_no_delete
             BEFORE DELETE ON reconciliation_policy_activations
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation policy activation history is immutable');
             END;
         CREATE UNIQUE INDEX reconciliation_policy_activation_idempotency
             ON reconciliation_policy_activations(idempotency_key);
         CREATE INDEX reconciliation_policy_activation_history
             ON reconciliation_policy_activations(generation, activation_sequence);

         CREATE TABLE reconciliation_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             validation_run_id TEXT NOT NULL
                 REFERENCES validation_runs(id) ON DELETE RESTRICT CHECK (
                     length(CAST(validation_run_id AS BLOB)) BETWEEN 1 AND 256
                 ),
             policy_identity TEXT NOT NULL CHECK (
                 length(CAST(policy_identity AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_version TEXT NOT NULL CHECK (
                 length(CAST(policy_version AS BLOB)) BETWEEN 1 AND 64
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             policy_activation_id TEXT NOT NULL CHECK (
                 length(CAST(policy_activation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_activation_hash TEXT NOT NULL CHECK (
                 length(policy_activation_hash) = 64
             ),
             engine_version TEXT NOT NULL CHECK (
                 length(CAST(engine_version AS BLOB)) BETWEEN 1 AND 128
             ),
             canonicalization_version TEXT NOT NULL CHECK (
                 length(CAST(canonicalization_version AS BLOB)) BETWEEN 1 AND 128
             ),
             governing_input_hash TEXT NOT NULL CHECK (
                 length(governing_input_hash) = 64
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 attempt_id, job_id, validation_run_id,
                 policy_identity, policy_version, policy_hash,
                 policy_activation_id, policy_activation_hash,
                 governing_input_hash
             ),
             FOREIGN KEY (
                 policy_activation_id, policy_identity, policy_version,
                 policy_hash, policy_activation_hash
             ) REFERENCES reconciliation_policy_activations(
                 activation_id, policy_identity, policy_version,
                 policy_hash, activation_hash
             ) ON DELETE RESTRICT
         );
         CREATE TRIGGER reconciliation_attempts_no_update
             BEFORE UPDATE ON reconciliation_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation attempt history is immutable');
             END;
         CREATE TRIGGER reconciliation_attempts_no_delete
             BEFORE DELETE ON reconciliation_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation attempt history is immutable');
             END;
         CREATE UNIQUE INDEX reconciliation_attempts_idempotency
             ON reconciliation_attempts(job_id, idempotency_key);
         CREATE INDEX reconciliation_attempts_job_history
             ON reconciliation_attempts(job_id, created_at, attempt_id);

         CREATE TABLE reconciliation_attempt_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             attempt_id TEXT NOT NULL
                 REFERENCES reconciliation_attempts(attempt_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (state IN (
                 'PLANNED', 'RUNNING', 'COMPLETE', 'INTERRUPTED', 'FAILED',
                 'BOUND_EXCEEDED', 'STALE_INPUT', 'INTEGRITY_BLOCKED', 'CONFLICTED'
             )),
             rows_processed INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
             error_code TEXT CHECK (
                 error_code IS NULL
                 OR length(CAST(error_code AS BLOB)) BETWEEN 1 AND 128
             ),
             detail TEXT CHECK (
                 detail IS NULL OR length(CAST(detail AS BLOB)) <= 4096
             ),
             resulting_snapshot_hash TEXT CHECK (
                 resulting_snapshot_hash IS NULL
                 OR length(resulting_snapshot_hash) = 64
             ),
             previous_event_hash TEXT CHECK (
                 previous_event_hash IS NULL OR length(previous_event_hash) = 64
             ),
             event_hash TEXT NOT NULL UNIQUE CHECK (length(event_hash) = 64),
             occurred_at TEXT NOT NULL CHECK (
                 length(CAST(occurred_at AS BLOB)) BETWEEN 1 AND 64
             ),
             CHECK (
                 (state IN ('PLANNED', 'RUNNING')
                     AND error_code IS NULL AND resulting_snapshot_hash IS NULL)
                 OR (state = 'COMPLETE'
                     AND error_code IS NULL AND resulting_snapshot_hash IS NOT NULL)
                 OR (state IN (
                         'INTERRUPTED', 'FAILED', 'BOUND_EXCEEDED', 'STALE_INPUT',
                         'INTEGRITY_BLOCKED', 'CONFLICTED'
                     )
                     AND error_code IS NOT NULL AND resulting_snapshot_hash IS NULL)
             )
         );
         CREATE TRIGGER reconciliation_attempt_events_no_update
             BEFORE UPDATE ON reconciliation_attempt_events
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation attempt event history is immutable');
             END;
         CREATE TRIGGER reconciliation_attempt_events_no_delete
             BEFORE DELETE ON reconciliation_attempt_events
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation attempt event history is immutable');
             END;
         CREATE INDEX reconciliation_attempt_events_history
             ON reconciliation_attempt_events(attempt_id, sequence);

         CREATE TABLE reconciliation_snapshots (
             snapshot_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(snapshot_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             attempt_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             state TEXT NOT NULL CHECK (state = 'COMPLETE'),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             accepted_source_hash TEXT NOT NULL CHECK (
                 length(accepted_source_hash) = 64
             ),
             accepted_row_set_hash TEXT NOT NULL CHECK (
                 length(accepted_row_set_hash) = 64
             ),
             accepted_row_count INTEGER NOT NULL CHECK (accepted_row_count >= 0),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 256
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 256
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 256
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 256
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             validation_run_id TEXT NOT NULL CHECK (
                 length(CAST(validation_run_id AS BLOB)) BETWEEN 1 AND 256
             ),
             validation_result_hash TEXT NOT NULL CHECK (
                 length(validation_result_hash) = 64
             ),
             active_decision_set_hash TEXT NOT NULL CHECK (
                 length(active_decision_set_hash) = 64
             ),
             active_overlay_set_hash TEXT NOT NULL CHECK (
                 length(active_overlay_set_hash) = 64
             ),
             effective_value_set_hash TEXT NOT NULL CHECK (
                 length(effective_value_set_hash) = 64
             ),
             qualifying_reevaluation_set_hash TEXT NOT NULL CHECK (
                 length(qualifying_reevaluation_set_hash) = 64
             ),
             policy_identity TEXT NOT NULL CHECK (
                 length(CAST(policy_identity AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_version TEXT NOT NULL CHECK (
                 length(CAST(policy_version AS BLOB)) BETWEEN 1 AND 64
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             policy_activation_id TEXT NOT NULL CHECK (
                 length(CAST(policy_activation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_activation_hash TEXT NOT NULL CHECK (
                 length(policy_activation_hash) = 64
             ),
             engine_version TEXT NOT NULL CHECK (
                 length(CAST(engine_version AS BLOB)) BETWEEN 1 AND 128
             ),
             canonicalization_version TEXT NOT NULL CHECK (
                 length(CAST(canonicalization_version AS BLOB)) BETWEEN 1 AND 128
             ),
             governing_input_hash TEXT NOT NULL CHECK (
                 length(governing_input_hash) = 64
             ),
             category_counts_hash TEXT NOT NULL CHECK (
                 length(category_counts_hash) = 64
             ),
             warning_codes_json TEXT NOT NULL CHECK (
                 length(CAST(warning_codes_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             warning_codes_hash TEXT NOT NULL CHECK (length(warning_codes_hash) = 64),
             blocker_codes_json TEXT NOT NULL CHECK (
                 length(CAST(blocker_codes_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             blocker_codes_hash TEXT NOT NULL CHECK (length(blocker_codes_hash) = 64),
             row_set_hash TEXT NOT NULL CHECK (length(row_set_hash) = 64),
             snapshot_hash TEXT NOT NULL UNIQUE CHECK (length(snapshot_hash) = 64),
             completed_at TEXT NOT NULL CHECK (
                 length(CAST(completed_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (snapshot_id, job_id),
             UNIQUE (snapshot_id, job_id, snapshot_hash, governing_input_hash),
             FOREIGN KEY (
                 attempt_id, job_id, validation_run_id,
                 policy_identity, policy_version, policy_hash,
                 policy_activation_id, policy_activation_hash,
                 governing_input_hash
             ) REFERENCES reconciliation_attempts(
                 attempt_id, job_id, validation_run_id,
                 policy_identity, policy_version, policy_hash,
                 policy_activation_id, policy_activation_hash,
                 governing_input_hash
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 validation_run_id, job_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash
             ) REFERENCES validation_runs(
                 id, job_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash
             ) ON DELETE RESTRICT
         );
         CREATE TRIGGER reconciliation_snapshots_no_update
             BEFORE UPDATE ON reconciliation_snapshots
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation snapshot history is immutable');
             END;
         CREATE TRIGGER reconciliation_snapshots_no_delete
             BEFORE DELETE ON reconciliation_snapshots
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation snapshot history is immutable');
             END;
         CREATE INDEX reconciliation_snapshots_job_history
             ON reconciliation_snapshots(job_id, completed_at, snapshot_id);

         CREATE TABLE reconciliation_category_counts (
             snapshot_id TEXT NOT NULL
                 REFERENCES reconciliation_snapshots(snapshot_id) ON DELETE RESTRICT,
             category TEXT NOT NULL CHECK (category IN (
                 'INCLUDED_VALID', 'INCLUDED_CORRECTED', 'INCLUDED_WAIVED',
                 'INCLUDED_ACCEPTED_AS_IS', 'EXCLUDED', 'BLOCKED_UNRESOLVED',
                 'BLOCKED_INCOMPLETE', 'BLOCKED_CONFLICT'
             )),
             row_count INTEGER NOT NULL CHECK (row_count >= 0),
             PRIMARY KEY (snapshot_id, category)
         );
         CREATE TRIGGER reconciliation_category_counts_no_update
             BEFORE UPDATE ON reconciliation_category_counts
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation category-count history is immutable');
             END;
         CREATE TRIGGER reconciliation_category_counts_no_delete
             BEFORE DELETE ON reconciliation_category_counts
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation category-count history is immutable');
             END;

         CREATE TABLE reconciliation_row_classifications (
             snapshot_id TEXT NOT NULL,
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_file TEXT NOT NULL CHECK (
                 length(CAST(source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             row_identity_hash TEXT NOT NULL CHECK (length(row_identity_hash) = 64),
             entity_id TEXT NOT NULL CHECK (
                 length(CAST(entity_id AS BLOB)) BETWEEN 1 AND 256
             ),
             category TEXT NOT NULL CHECK (category IN (
                 'INCLUDED_VALID', 'INCLUDED_CORRECTED', 'INCLUDED_WAIVED',
                 'INCLUDED_ACCEPTED_AS_IS', 'EXCLUDED', 'BLOCKED_UNRESOLVED',
                 'BLOCKED_INCOMPLETE', 'BLOCKED_CONFLICT'
             )),
             explanation_codes_json TEXT NOT NULL CHECK (
                 length(CAST(explanation_codes_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             explanation_codes_hash TEXT NOT NULL CHECK (
                 length(explanation_codes_hash) = 64
             ),
             lineage_references_json TEXT NOT NULL CHECK (
                 length(CAST(lineage_references_json AS BLOB)) BETWEEN 2 AND 262144
             ),
             lineage_references_hash TEXT NOT NULL CHECK (
                 length(lineage_references_hash) = 64
             ),
             classification_hash TEXT NOT NULL CHECK (
                 length(classification_hash) = 64
             ),
             PRIMARY KEY (snapshot_id, source_file, source_ordinal),
             UNIQUE (snapshot_id, row_identity_hash),
             UNIQUE (snapshot_id, classification_hash),
             FOREIGN KEY (snapshot_id, job_id)
                 REFERENCES reconciliation_snapshots(snapshot_id, job_id)
                 ON DELETE RESTRICT,
             FOREIGN KEY (job_id, source_file, source_ordinal)
                 REFERENCES source_records(job_id, file_path, source_ordinal)
                 ON DELETE RESTRICT
         );
         CREATE TRIGGER reconciliation_row_classifications_no_update
             BEFORE UPDATE ON reconciliation_row_classifications
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation row-classification history is immutable');
             END;
         CREATE TRIGGER reconciliation_row_classifications_no_delete
             BEFORE DELETE ON reconciliation_row_classifications
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation row-classification history is immutable');
             END;
         CREATE INDEX reconciliation_rows_category_page
             ON reconciliation_row_classifications(
                 snapshot_id, category, source_file, source_ordinal
             );
         CREATE INDEX reconciliation_rows_entity_page
             ON reconciliation_row_classifications(
                 snapshot_id, entity_id, source_file, source_ordinal
             );

         CREATE TABLE reconciliation_row_lineage_references (
             snapshot_id TEXT NOT NULL,
             source_file TEXT NOT NULL CHECK (
                 length(CAST(source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             reference_kind TEXT NOT NULL CHECK (reference_kind IN (
                 'VALIDATION_RUN', 'VALIDATION_ISSUE', 'ISSUE_DECISION',
                 'CORRECTION_OVERLAY', 'CORRECTION_EFFECTIVATION', 'REEVALUATION_RUN'
             )),
             reference_id TEXT NOT NULL CHECK (
                 length(CAST(reference_id AS BLOB)) BETWEEN 1 AND 256
             ),
             reference_hash TEXT NOT NULL CHECK (length(reference_hash) = 64),
             PRIMARY KEY (
                 snapshot_id, source_file, source_ordinal,
                 reference_kind, reference_id
             ),
             FOREIGN KEY (snapshot_id, source_file, source_ordinal)
                 REFERENCES reconciliation_row_classifications(
                     snapshot_id, source_file, source_ordinal
                 ) ON DELETE RESTRICT
         );
         CREATE TRIGGER reconciliation_row_lineage_references_no_update
             BEFORE UPDATE ON reconciliation_row_lineage_references
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation row-lineage history is immutable');
             END;
         CREATE TRIGGER reconciliation_row_lineage_references_no_delete
             BEFORE DELETE ON reconciliation_row_lineage_references
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation row-lineage history is immutable');
             END;
         CREATE INDEX reconciliation_row_lineage_filter
             ON reconciliation_row_lineage_references(
                 snapshot_id, reference_kind, reference_id,
                 source_file, source_ordinal
             );

         CREATE TABLE migration_result_candidates (
             candidate_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             candidate_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(candidate_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             snapshot_id TEXT NOT NULL CHECK (
                 length(CAST(snapshot_id AS BLOB)) BETWEEN 1 AND 256
             ),
             snapshot_hash TEXT NOT NULL CHECK (length(snapshot_hash) = 64),
             governing_input_hash TEXT NOT NULL CHECK (
                 length(governing_input_hash) = 64
             ),
             policy_identity TEXT NOT NULL CHECK (
                 length(CAST(policy_identity AS BLOB)) BETWEEN 1 AND 256
             ),
             policy_version TEXT NOT NULL CHECK (
                 length(CAST(policy_version AS BLOB)) BETWEEN 1 AND 64
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             engine_version TEXT NOT NULL CHECK (
                 length(CAST(engine_version AS BLOB)) BETWEEN 1 AND 128
             ),
             canonicalization_version TEXT NOT NULL CHECK (
                 length(CAST(canonicalization_version AS BLOB)) BETWEEN 1 AND 128
             ),
             accepted_row_count INTEGER NOT NULL CHECK (accepted_row_count >= 0),
             included_count INTEGER NOT NULL CHECK (included_count >= 0),
             excluded_count INTEGER NOT NULL CHECK (excluded_count >= 0),
             blocked_count INTEGER NOT NULL CHECK (blocked_count >= 0),
             warning_codes_json TEXT NOT NULL CHECK (
                 length(CAST(warning_codes_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             warning_codes_hash TEXT NOT NULL CHECK (length(warning_codes_hash) = 64),
             required_acknowledgments_json TEXT NOT NULL CHECK (
                 length(CAST(required_acknowledgments_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             required_acknowledgments_hash TEXT NOT NULL CHECK (
                 length(required_acknowledgments_hash) = 64
             ),
             creator TEXT NOT NULL CHECK (
                 length(CAST(creator AS BLOB)) BETWEEN 1 AND 256
             ),
             preparation_reason TEXT NOT NULL CHECK (
                 length(CAST(preparation_reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             creation_generation INTEGER NOT NULL CHECK (creation_generation > 0),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_candidate_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_candidate_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             candidate_hash TEXT NOT NULL UNIQUE CHECK (length(candidate_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 candidate_id, job_id, snapshot_id, snapshot_hash, candidate_hash
             ),
             FOREIGN KEY (
                 snapshot_id, job_id, snapshot_hash, governing_input_hash
             ) REFERENCES reconciliation_snapshots(
                 snapshot_id, job_id, snapshot_hash, governing_input_hash
             ) ON DELETE RESTRICT,
             CHECK (
                 included_count + excluded_count + blocked_count = accepted_row_count
             )
         );
         CREATE TRIGGER migration_result_candidates_no_update
             BEFORE UPDATE ON migration_result_candidates
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result candidate history is immutable');
             END;
         CREATE TRIGGER migration_result_candidates_no_delete
             BEFORE DELETE ON migration_result_candidates
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result candidate history is immutable');
             END;
         CREATE UNIQUE INDEX migration_result_candidates_idempotency
             ON migration_result_candidates(job_id, idempotency_key);
         CREATE UNIQUE INDEX migration_result_candidates_generation
             ON migration_result_candidates(job_id, creation_generation);
         CREATE INDEX migration_result_candidates_job_history
             ON migration_result_candidates(job_id, created_at, candidate_id);

         CREATE TABLE migration_result_actions (
             action_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             action_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(action_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             job_id TEXT NOT NULL CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_id TEXT NOT NULL CHECK (
                 length(CAST(candidate_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_hash TEXT NOT NULL CHECK (length(candidate_hash) = 64),
             snapshot_id TEXT NOT NULL CHECK (
                 length(CAST(snapshot_id AS BLOB)) BETWEEN 1 AND 256
             ),
             snapshot_hash TEXT NOT NULL CHECK (length(snapshot_hash) = 64),
             action_kind TEXT NOT NULL CHECK (action_kind IN (
                 'APPROVED', 'REJECTED', 'APPROVAL_REVOKED', 'APPROVAL_SUPERSEDED'
             )),
             predecessor_action_id TEXT CHECK (
                 predecessor_action_id IS NULL
                 OR length(CAST(predecessor_action_id AS BLOB)) BETWEEN 1 AND 256
             ),
             predecessor_generation INTEGER NOT NULL CHECK (
                 predecessor_generation >= 0
             ),
             observed_generation INTEGER NOT NULL CHECK (observed_generation >= 0),
             generation INTEGER NOT NULL CHECK (generation > 0),
             actor TEXT NOT NULL CHECK (
                 length(CAST(actor AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             warning_acknowledgments_json TEXT NOT NULL CHECK (
                 length(CAST(warning_acknowledgments_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             warning_acknowledgments_hash TEXT NOT NULL CHECK (
                 length(warning_acknowledgments_hash) = 64
             ),
             authority_boundary_acknowledged INTEGER NOT NULL CHECK (
                 authority_boundary_acknowledged = 1
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_action_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_action_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             action_hash TEXT NOT NULL UNIQUE CHECK (length(action_hash) = 64),
             occurred_at TEXT NOT NULL CHECK (
                 length(CAST(occurred_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (action_id, generation, job_id),
             FOREIGN KEY (
                 candidate_id, job_id, snapshot_id, snapshot_hash, candidate_hash
             ) REFERENCES migration_result_candidates(
                 candidate_id, job_id, snapshot_id, snapshot_hash, candidate_hash
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 predecessor_action_id, predecessor_generation, job_id
             ) REFERENCES migration_result_actions(
                 action_id, generation, job_id
             ) ON DELETE RESTRICT,
             CHECK (generation = observed_generation + 1),
             CHECK (
                 (predecessor_action_id IS NULL
                     AND predecessor_generation = 0
                     AND observed_generation = 0
                     AND generation = 1
                     AND action_kind IN ('APPROVED', 'REJECTED'))
                 OR (predecessor_action_id IS NOT NULL
                     AND predecessor_generation > 0
                     AND observed_generation = predecessor_generation
                     AND generation = predecessor_generation + 1)
             ),
             CHECK (
                 action_kind NOT IN ('APPROVAL_REVOKED', 'APPROVAL_SUPERSEDED')
                 OR predecessor_action_id IS NOT NULL
             )
         );
         CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;
         CREATE TRIGGER migration_result_actions_no_delete
             BEFORE DELETE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;
         CREATE UNIQUE INDEX migration_result_actions_idempotency
             ON migration_result_actions(job_id, idempotency_key);
         CREATE UNIQUE INDEX migration_result_actions_generation
             ON migration_result_actions(job_id, generation);
         CREATE UNIQUE INDEX migration_result_actions_predecessor
             ON migration_result_actions(job_id, predecessor_action_id)
             WHERE predecessor_action_id IS NOT NULL;
         CREATE INDEX migration_result_actions_job_history
             ON migration_result_actions(job_id, generation, action_sequence);
         CREATE INDEX migration_result_actions_candidate_history
             ON migration_result_actions(candidate_id, occurred_at, action_sequence);

         PRAGMA user_version = 7;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v7_to_v8(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE TABLE IF NOT EXISTS publication_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_id TEXT NOT NULL CHECK (
                 length(CAST(candidate_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_hash TEXT NOT NULL CHECK (length(candidate_hash) = 64),
             approval_id TEXT NOT NULL CHECK (
                 length(CAST(approval_id AS BLOB)) BETWEEN 1 AND 256
             ),
             approval_hash TEXT NOT NULL CHECK (length(approval_hash) = 64),
             contract_version TEXT NOT NULL CHECK (
                 length(CAST(contract_version AS BLOB)) BETWEEN 1 AND 128
             ),
             practitioner TEXT NOT NULL CHECK (
                 length(CAST(practitioner AS BLOB)) BETWEEN 1 AND 256
             ),
             publication_reason TEXT NOT NULL CHECK (
                 length(CAST(publication_reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             semantic_publication_hash TEXT CHECK (
                 semantic_publication_hash IS NULL OR length(semantic_publication_hash) = 64
             ),
             physical_archive_digest TEXT CHECK (
                 physical_archive_digest IS NULL OR length(physical_archive_digest) = 64
             ),
             package_size_bytes INTEGER CHECK (
                 package_size_bytes IS NULL OR package_size_bytes >= 0
             ),
             archive_relative_path TEXT CHECK (
                 archive_relative_path IS NULL OR length(CAST(archive_relative_path AS BLOB)) BETWEEN 1 AND 512
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER IF NOT EXISTS publication_attempts_no_update
             BEFORE UPDATE ON publication_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'publication attempt history is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS publication_attempts_no_delete
             BEFORE DELETE ON publication_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'publication attempt history is immutable');
             END;
         CREATE UNIQUE INDEX IF NOT EXISTS publication_attempts_idempotency
             ON publication_attempts(job_id, idempotency_key);
         CREATE INDEX IF NOT EXISTS publication_attempts_job_history
             ON publication_attempts(job_id, created_at, attempt_id);

         CREATE TABLE IF NOT EXISTS publication_records (
             publication_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(publication_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version INTEGER NOT NULL CHECK (schema_version = 1),
             attempt_id TEXT NOT NULL UNIQUE REFERENCES publication_attempts(attempt_id) ON DELETE RESTRICT,
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT CHECK (
                 length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_id TEXT NOT NULL CHECK (
                 length(CAST(candidate_id AS BLOB)) BETWEEN 1 AND 256
             ),
             candidate_hash TEXT NOT NULL CHECK (length(candidate_hash) = 64),
             approval_id TEXT NOT NULL CHECK (
                 length(CAST(approval_id AS BLOB)) BETWEEN 1 AND 256
             ),
             approval_hash TEXT NOT NULL CHECK (length(approval_hash) = 64),
             contract_version TEXT NOT NULL CHECK (
                 length(CAST(contract_version AS BLOB)) BETWEEN 1 AND 128
             ),
             semantic_publication_hash TEXT NOT NULL UNIQUE CHECK (
                 length(semantic_publication_hash) = 64
             ),
             physical_archive_digest TEXT NOT NULL CHECK (
                 length(physical_archive_digest) = 64
             ),
             package_size_bytes INTEGER NOT NULL CHECK (package_size_bytes >= 0),
             member_count INTEGER NOT NULL CHECK (member_count >= 0),
             included_row_count INTEGER NOT NULL CHECK (included_row_count >= 0),
             excluded_row_count INTEGER NOT NULL CHECK (excluded_row_count >= 0),
             archive_relative_path TEXT NOT NULL CHECK (
                 length(CAST(archive_relative_path AS BLOB)) BETWEEN 1 AND 512
             ),
             practitioner TEXT NOT NULL CHECK (
                 length(CAST(practitioner AS BLOB)) BETWEEN 1 AND 256
             ),
             publication_reason TEXT NOT NULL CHECK (
                 length(CAST(publication_reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             canonical_manifest_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_manifest_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER IF NOT EXISTS publication_records_no_update
             BEFORE UPDATE ON publication_records
             BEGIN
                 SELECT RAISE(ABORT, 'publication records are immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS publication_records_no_delete
             BEFORE DELETE ON publication_records
             BEGIN
                 SELECT RAISE(ABORT, 'publication records are immutable');
             END;

         CREATE TABLE IF NOT EXISTS publication_manifest_members (
             publication_id TEXT NOT NULL REFERENCES publication_records(publication_id) ON DELETE RESTRICT,
             filename TEXT NOT NULL CHECK (
                 length(CAST(filename AS BLOB)) BETWEEN 1 AND 256
             ),
             media_type TEXT NOT NULL CHECK (
                 length(CAST(media_type AS BLOB)) BETWEEN 1 AND 128
             ),
             row_count INTEGER CHECK (row_count IS NULL OR row_count >= 0),
             byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
             sha256_digest TEXT NOT NULL CHECK (length(sha256_digest) = 64),
             PRIMARY KEY (publication_id, filename)
         );
         CREATE TRIGGER IF NOT EXISTS publication_manifest_members_no_update
             BEFORE UPDATE ON publication_manifest_members
             BEGIN
                 SELECT RAISE(ABORT, 'publication manifest members are immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS publication_manifest_members_no_delete
             BEFORE DELETE ON publication_manifest_members
             BEGIN
                 SELECT RAISE(ABORT, 'publication manifest members are immutable');
             END;

         PRAGMA user_version = 8;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v8_to_v9(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE TABLE IF NOT EXISTS engagement_records (
             singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
             engagement_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(engagement_id AS BLOB)) BETWEEN 1 AND 256
             ),
             schema_version TEXT NOT NULL CHECK (
                 schema_version = 'migration-stream/engagement/record/v1'
             ),
             display_name TEXT NOT NULL CHECK (
                 length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256
             ),
             idempotency_key TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER IF NOT EXISTS engagement_records_no_update
             BEFORE UPDATE ON engagement_records
             BEGIN
                 SELECT RAISE(ABORT, 'engagement identity is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS engagement_records_no_delete
             BEFORE DELETE ON engagement_records
             BEGIN
                 SELECT RAISE(ABORT, 'engagement identity is immutable');
             END;

         CREATE TABLE IF NOT EXISTS engagement_active_context (
             engagement_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
             current_preflight_id TEXT,
             active_job_id TEXT REFERENCES jobs(id) ON DELETE RESTRICT,
             active_profiling_run_id TEXT
                 REFERENCES profiling_runs(id) ON DELETE RESTRICT,
             selected_object_pack_id TEXT,
             selected_profile_id TEXT,
             selected_profile_version TEXT,
             selected_profile_hash TEXT CHECK (
                 selected_profile_hash IS NULL OR length(selected_profile_hash) = 64
             ),
             profile_stage TEXT NOT NULL DEFAULT 'NOT_SELECTED' CHECK (
                 profile_stage IN ('NOT_SELECTED', 'REVIEWED', 'APPROVED')
             ),
             active_mapping_id TEXT,
             active_mapping_version TEXT,
             active_mapping_hash TEXT CHECK (
                 active_mapping_hash IS NULL OR length(active_mapping_hash) = 64
             ),
             mapping_stage TEXT NOT NULL DEFAULT 'NOT_PREPARED' CHECK (
                 mapping_stage IN ('NOT_PREPARED', 'PREPARED', 'VALIDATED')
             ),
             active_validation_run_id TEXT
                 REFERENCES validation_runs(id) ON DELETE RESTRICT,
             validation_stage TEXT NOT NULL DEFAULT 'NOT_LAUNCHED' CHECK (
                 validation_stage IN (
                     'NOT_LAUNCHED', 'RUNNING', 'COMPLETE', 'FAILED',
                     'INTERRUPTED', 'BOUND_EXCEEDED'
                 )
             ),
             updated_at TEXT NOT NULL CHECK (
                 length(CAST(updated_at AS BLOB)) BETWEEN 1 AND 64
             ),
             CHECK (
                 (profile_stage = 'NOT_SELECTED'
                     AND selected_object_pack_id IS NULL
                     AND selected_profile_id IS NULL
                     AND selected_profile_version IS NULL
                     AND selected_profile_hash IS NULL)
                 OR (profile_stage IN ('REVIEWED', 'APPROVED')
                     AND selected_object_pack_id IS NOT NULL
                     AND selected_profile_id IS NOT NULL
                     AND selected_profile_version IS NOT NULL
                     AND selected_profile_hash IS NOT NULL)
             ),
             CHECK (
                 (mapping_stage = 'NOT_PREPARED'
                     AND active_mapping_id IS NULL
                     AND active_mapping_version IS NULL
                     AND active_mapping_hash IS NULL)
                 OR (mapping_stage IN ('PREPARED', 'VALIDATED')
                     AND active_mapping_id IS NOT NULL
                     AND active_mapping_version IS NOT NULL
                     AND active_mapping_hash IS NOT NULL)
             ),
             CHECK (
                 (validation_stage = 'NOT_LAUNCHED'
                     AND active_validation_run_id IS NULL)
                 OR validation_stage <> 'NOT_LAUNCHED'
             )
         );

         CREATE TABLE IF NOT EXISTS engagement_source_preflights (
             preflight_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(preflight_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (
                 state IN ('RECEIVING', 'READY', 'REJECTED', 'INTERRUPTED')
             ),
             source_name TEXT NOT NULL CHECK (
                 length(CAST(source_name AS BLOB)) BETWEEN 1 AND 512
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             partial_relative_path TEXT NOT NULL CHECK (
                 length(CAST(partial_relative_path AS BLOB)) BETWEEN 1 AND 1024
             ),
             retained_relative_path TEXT,
             source_hash TEXT CHECK (
                 source_hash IS NULL OR length(source_hash) = 64
             ),
             source_size INTEGER CHECK (source_size IS NULL OR source_size >= 0),
             policy_json TEXT NOT NULL CHECK (
                 length(CAST(policy_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             manifest_json TEXT,
             manifest_hash TEXT CHECK (
                 manifest_hash IS NULL OR length(manifest_hash) = 64
             ),
             error_code TEXT,
             error_message TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             completed_at TEXT,
             UNIQUE (engagement_id, idempotency_key),
             CHECK (
                 (state = 'RECEIVING'
                     AND retained_relative_path IS NULL
                     AND source_hash IS NULL AND source_size IS NULL
                     AND manifest_json IS NULL AND manifest_hash IS NULL
                     AND error_code IS NULL AND completed_at IS NULL)
                 OR (state = 'READY'
                     AND retained_relative_path IS NOT NULL
                     AND source_hash IS NOT NULL AND source_size IS NOT NULL
                     AND manifest_json IS NOT NULL AND manifest_hash IS NOT NULL
                     AND error_code IS NULL AND completed_at IS NOT NULL)
                 OR (state IN ('REJECTED', 'INTERRUPTED')
                     AND error_code IS NOT NULL AND completed_at IS NOT NULL)
             )
         );
         CREATE INDEX IF NOT EXISTS engagement_source_preflights_history
             ON engagement_source_preflights(engagement_id, created_at, preflight_id);

         CREATE TABLE IF NOT EXISTS engagement_preflight_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             preflight_id TEXT NOT NULL
                 REFERENCES engagement_source_preflights(preflight_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (
                 state IN ('RECEIVING', 'READY', 'REJECTED', 'INTERRUPTED')
             ),
             error_code TEXT,
             detail TEXT,
             occurred_at TEXT NOT NULL
         );
         CREATE TRIGGER IF NOT EXISTS engagement_preflight_events_no_update
             BEFORE UPDATE ON engagement_preflight_events
             BEGIN
                 SELECT RAISE(ABORT, 'engagement preflight history is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS engagement_preflight_events_no_delete
             BEFORE DELETE ON engagement_preflight_events
             BEGIN
                 SELECT RAISE(ABORT, 'engagement preflight history is immutable');
             END;

         CREATE TABLE IF NOT EXISTS engagement_acceptance_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             preflight_id TEXT NOT NULL
                 REFERENCES engagement_source_preflights(preflight_id) ON DELETE RESTRICT,
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             accepted_source_hash TEXT NOT NULL CHECK (length(accepted_source_hash) = 64),
             accepted_manifest_hash TEXT NOT NULL CHECK (length(accepted_manifest_hash) = 64),
             profiling_policy_hash TEXT NOT NULL CHECK (
                 length(profiling_policy_hash) = 64
             ),
             acknowledgement TEXT NOT NULL CHECK (
                 acknowledgement = 'migration-stream/source-acceptance/v1'
             ),
             controlled_source_relative_path TEXT NOT NULL,
             job_id TEXT REFERENCES jobs(id) ON DELETE RESTRICT,
             profiling_run_id TEXT REFERENCES profiling_runs(id) ON DELETE RESTRICT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             UNIQUE (engagement_id, idempotency_key)
         );

         CREATE TABLE IF NOT EXISTS engagement_mapping_preparations (
             preparation_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(preparation_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             mapping_id TEXT NOT NULL,
             mapping_version TEXT NOT NULL,
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             document_json TEXT NOT NULL CHECK (
                 length(CAST(document_json AS BLOB)) BETWEEN 2 AND 2097152
             ),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             created_at TEXT NOT NULL,
             UNIQUE (engagement_id, mapping_id, mapping_version)
         );
         CREATE TRIGGER IF NOT EXISTS engagement_mapping_preparations_no_update
             BEFORE UPDATE ON engagement_mapping_preparations
             BEGIN
                 SELECT RAISE(ABORT, 'prepared mapping bytes are immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS engagement_mapping_preparations_no_delete
             BEFORE DELETE ON engagement_mapping_preparations
             BEGIN
                 SELECT RAISE(ABORT, 'prepared mapping bytes are immutable');
             END;

         CREATE TABLE IF NOT EXISTS engagement_validation_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             idempotency_key TEXT NOT NULL,
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL,
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             mapping_id TEXT NOT NULL,
             mapping_version TEXT NOT NULL,
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             policy_hash TEXT NOT NULL CHECK (length(policy_hash) = 64),
             run_id TEXT REFERENCES validation_runs(id) ON DELETE RESTRICT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             UNIQUE (engagement_id, idempotency_key)
         );

         CREATE TABLE IF NOT EXISTS engagement_command_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             command_kind TEXT NOT NULL CHECK (
                 command_kind IN (
                     'SELECT_OBJECT_PACK', 'APPROVE_PROFILE', 'PREPARE_MAPPING'
                 )
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 2097152
             ),
             result_reference TEXT,
             created_at TEXT NOT NULL,
             UNIQUE (engagement_id, idempotency_key)
         );
         CREATE TRIGGER IF NOT EXISTS engagement_command_attempts_no_update
             BEFORE UPDATE ON engagement_command_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'engagement command attempts are immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS engagement_command_attempts_no_delete
             BEFORE DELETE ON engagement_command_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'engagement command attempts are immutable');
             END;

         PRAGMA user_version = 9;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v9_to_v10(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE TABLE engagement_mapping_draft_heads (
             draft_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(draft_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             source_job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             object_pack_id TEXT NOT NULL CHECK (
                 length(CAST(object_pack_id AS BLOB)) BETWEEN 1 AND 128
             ),
             object_pack_version TEXT NOT NULL CHECK (
                 length(CAST(object_pack_version AS BLOB)) BETWEEN 1 AND 32
             ),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 128
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 32
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             generation INTEGER NOT NULL CHECK (generation > 0),
             state TEXT NOT NULL CHECK (
                 state IN ('EDITABLE', 'SUBMITTED', 'APPROVED')
             ),
             head_revision_hash TEXT NOT NULL CHECK (
                 length(head_revision_hash) = 64
             ),
             submitted_mapping_id TEXT,
             submitted_mapping_version TEXT,
             submitted_mapping_hash TEXT CHECK (
                 submitted_mapping_hash IS NULL
                 OR length(submitted_mapping_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             updated_at TEXT NOT NULL CHECK (
                 length(CAST(updated_at AS BLOB)) BETWEEN 1 AND 64
             ),
             submitted_at TEXT CHECK (
                 submitted_at IS NULL
                 OR length(CAST(submitted_at AS BLOB)) BETWEEN 1 AND 64
             ),
             approved_at TEXT CHECK (
                 approved_at IS NULL
                 OR length(CAST(approved_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 engagement_id, source_job_id, source_hash,
                 object_pack_id, object_pack_version,
                 profile_id, profile_version, profile_hash
             ),
             UNIQUE (draft_id, generation, head_revision_hash),
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (
                 submitted_mapping_id,
                 submitted_mapping_version,
                 submitted_mapping_hash
             ) REFERENCES mapping_versions(
                 mapping_id, mapping_version, mapping_hash
             ) ON DELETE RESTRICT,
             CHECK (
                 (state = 'EDITABLE'
                     AND submitted_mapping_id IS NULL
                     AND submitted_mapping_version IS NULL
                     AND submitted_mapping_hash IS NULL
                     AND submitted_at IS NULL
                     AND approved_at IS NULL)
                 OR (state = 'SUBMITTED'
                     AND submitted_mapping_id IS NOT NULL
                     AND submitted_mapping_version IS NOT NULL
                     AND submitted_mapping_hash IS NOT NULL
                     AND submitted_at IS NOT NULL
                     AND approved_at IS NULL)
                 OR (state = 'APPROVED'
                     AND submitted_mapping_id IS NOT NULL
                     AND submitted_mapping_version IS NOT NULL
                     AND submitted_mapping_hash IS NOT NULL
                     AND submitted_at IS NOT NULL
                     AND approved_at IS NOT NULL)
             )
         );
         CREATE INDEX engagement_mapping_draft_heads_history
             ON engagement_mapping_draft_heads(
                 engagement_id, state, updated_at, draft_id
             );
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
             END;
         CREATE TRIGGER engagement_mapping_draft_heads_no_delete
             BEFORE DELETE ON engagement_mapping_draft_heads
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping draft heads cannot be deleted'
                 );
             END;

         CREATE TABLE engagement_mapping_draft_revisions (
             draft_id TEXT NOT NULL
                 REFERENCES engagement_mapping_draft_heads(draft_id)
                 ON DELETE RESTRICT,
             generation INTEGER NOT NULL CHECK (generation > 0),
             revision_hash TEXT NOT NULL CHECK (length(revision_hash) = 64),
             parent_revision_hash TEXT CHECK (
                 parent_revision_hash IS NULL
                 OR length(parent_revision_hash) = 64
             ),
             canonical_draft_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_draft_json AS BLOB))
                     BETWEEN 2 AND 2097152
             ),
             actor TEXT NOT NULL CHECK (
                 length(CAST(actor AS BLOB)) BETWEEN 1 AND 256
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             PRIMARY KEY (draft_id, generation),
             UNIQUE (draft_id, revision_hash),
             FOREIGN KEY (draft_id, parent_revision_hash)
                 REFERENCES engagement_mapping_draft_revisions(
                     draft_id, revision_hash
                 ) ON DELETE RESTRICT,
             CHECK (
                 (generation = 1 AND parent_revision_hash IS NULL)
                 OR (generation > 1 AND parent_revision_hash IS NOT NULL)
             )
         );
         CREATE TRIGGER engagement_mapping_draft_revisions_no_update
             BEFORE UPDATE ON engagement_mapping_draft_revisions
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping draft revisions are immutable'
                 );
             END;
         CREATE TRIGGER engagement_mapping_draft_revisions_no_delete
             BEFORE DELETE ON engagement_mapping_draft_revisions
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping draft revisions are immutable'
                 );
             END;

         CREATE TABLE engagement_mapping_workspace_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             draft_id TEXT NOT NULL
                 REFERENCES engagement_mapping_draft_heads(draft_id)
                 ON DELETE RESTRICT,
             command_kind TEXT NOT NULL CHECK (
                 command_kind IN (
                     'SAVE_DRAFT', 'SUBMIT_MAPPING', 'APPROVE_MAPPING'
                 )
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 16 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB))
                     BETWEEN 2 AND 2097152
             ),
             expected_generation INTEGER NOT NULL CHECK (
                 expected_generation >= 0
             ),
             expected_state TEXT NOT NULL CHECK (
                 expected_state IN ('ABSENT', 'EDITABLE', 'SUBMITTED')
             ),
             result_generation INTEGER NOT NULL CHECK (
                 result_generation > 0
             ),
             result_state TEXT NOT NULL CHECK (
                 result_state IN ('EDITABLE', 'SUBMITTED', 'APPROVED')
             ),
             result_reference TEXT NOT NULL CHECK (
                 length(CAST(result_reference AS BLOB)) BETWEEN 1 AND 256
             ),
             result_hash TEXT NOT NULL CHECK (length(result_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (engagement_id, idempotency_key),
             CHECK (
                 (command_kind = 'SAVE_DRAFT'
                     AND result_state = 'EDITABLE'
                     AND (
                         (expected_generation = 0
                             AND expected_state = 'ABSENT')
                         OR (expected_generation > 0
                             AND expected_state = 'EDITABLE')
                     ))
                 OR (command_kind = 'SUBMIT_MAPPING'
                     AND expected_generation > 0
                     AND expected_state = 'EDITABLE'
                     AND result_state = 'SUBMITTED')
                 OR (command_kind = 'APPROVE_MAPPING'
                     AND expected_generation > 0
                     AND expected_state = 'SUBMITTED'
                     AND result_state = 'APPROVED')
             )
         );
         CREATE INDEX engagement_mapping_workspace_attempts_history
             ON engagement_mapping_workspace_attempts(
                 engagement_id, created_at, attempt_id
             );
         CREATE TRIGGER engagement_mapping_workspace_attempts_no_update
             BEFORE UPDATE ON engagement_mapping_workspace_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping workspace attempts are immutable'
                 );
             END;
         CREATE TRIGGER engagement_mapping_workspace_attempts_no_delete
             BEFORE DELETE ON engagement_mapping_workspace_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping workspace attempts are immutable'
                 );
             END;

         CREATE TABLE engagement_mapping_approvals (
             approval_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(approval_id AS BLOB)) BETWEEN 1 AND 256
             ),
             attempt_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_mapping_workspace_attempts(attempt_id)
                 ON DELETE RESTRICT,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             draft_id TEXT NOT NULL
                 REFERENCES engagement_mapping_draft_heads(draft_id)
                 ON DELETE RESTRICT,
             draft_generation INTEGER NOT NULL CHECK (draft_generation > 0),
             draft_revision_hash TEXT NOT NULL CHECK (
                 length(draft_revision_hash) = 64
             ),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 128
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 32
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             approver TEXT NOT NULL CHECK (
                 length(CAST(approver AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             acknowledgement TEXT NOT NULL CHECK (
                 acknowledgement =
                     'migration-stream/local-mapping-version-approval/v1'
             ),
             canonical_approval_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_approval_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             approval_hash TEXT NOT NULL UNIQUE CHECK (
                 length(approval_hash) = 64
             ),
             approved_at TEXT NOT NULL CHECK (
                 length(CAST(approved_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (draft_id, draft_generation),
             FOREIGN KEY (draft_id, draft_generation, draft_revision_hash)
                 REFERENCES engagement_mapping_draft_heads(
                     draft_id, generation, head_revision_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(
                     mapping_id, mapping_version, mapping_hash
                 ) ON DELETE RESTRICT
         );
         CREATE INDEX engagement_mapping_approvals_mapping
             ON engagement_mapping_approvals(
                 mapping_id, mapping_version, mapping_hash, approved_at
             );
         CREATE TRIGGER engagement_mapping_approvals_no_update
             BEFORE UPDATE ON engagement_mapping_approvals
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping approvals are immutable'
                 );
             END;
         CREATE TRIGGER engagement_mapping_approvals_no_delete
             BEFORE DELETE ON engagement_mapping_approvals
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement mapping approvals are immutable'
                 );
             END;

         PRAGMA user_version = 10;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v10_to_v11(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;

         CREATE TABLE engagement_tour_attempts (
             tour_attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(tour_attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             planned_tour_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(planned_tour_id AS BLOB)) BETWEEN 1 AND 256
             ),
             tour_number INTEGER NOT NULL CHECK (tour_number > 0),
             predecessor_tour_id TEXT REFERENCES engagement_tours(tour_id)
                 ON DELETE RESTRICT,
             started_by TEXT NOT NULL CHECK (
                 length(CAST(started_by AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             canonical_attempt_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_attempt_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             attempt_hash TEXT NOT NULL UNIQUE CHECK (
                 length(attempt_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             CHECK (
                 (tour_number = 1 AND predecessor_tour_id IS NULL)
                 OR (tour_number > 1 AND predecessor_tour_id IS NOT NULL)
             )
         );
         CREATE INDEX engagement_tour_attempts_history
             ON engagement_tour_attempts(
                 engagement_id, tour_number, created_at, tour_attempt_id
             );
         CREATE TRIGGER engagement_tour_attempts_no_update
             BEFORE UPDATE ON engagement_tour_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement tour attempt identities are immutable'
                 );
             END;
         CREATE TRIGGER engagement_tour_attempts_no_delete
             BEFORE DELETE ON engagement_tour_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement tour attempt identities are immutable'
                 );
             END;

         CREATE TABLE engagement_tour_command_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             tour_attempt_id TEXT
                 REFERENCES engagement_tour_attempts(tour_attempt_id)
                 ON DELETE RESTRICT,
             command_kind TEXT NOT NULL CHECK (
                 command_kind IN (
                     'START_TOUR', 'RESERVE_SOURCE',
                     'ACCEPT_SOURCE', 'LAUNCH_VALIDATION'
                 )
             ),
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 16 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB))
                     BETWEEN 2 AND 2097152
             ),
             expected_generation INTEGER NOT NULL CHECK (
                 expected_generation >= 0
             ),
             result_generation INTEGER NOT NULL CHECK (
                 result_generation > expected_generation
                 AND result_generation = expected_generation + 1
             ),
             result_reference TEXT NOT NULL CHECK (
                 length(CAST(result_reference AS BLOB)) BETWEEN 1 AND 256
             ),
             result_hash TEXT NOT NULL CHECK (length(result_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (engagement_id, idempotency_key),
             CHECK (
                 (command_kind = 'START_TOUR' AND tour_attempt_id IS NULL)
                 OR (command_kind <> 'START_TOUR'
                     AND tour_attempt_id IS NOT NULL)
             )
         );
         CREATE INDEX engagement_tour_command_attempts_history
             ON engagement_tour_command_attempts(
                 engagement_id, created_at, attempt_id
             );
         CREATE TRIGGER engagement_tour_command_attempts_no_update
             BEFORE UPDATE ON engagement_tour_command_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement tour command attempts are immutable'
                 );
             END;
         CREATE TRIGGER engagement_tour_command_attempts_no_delete
             BEFORE DELETE ON engagement_tour_command_attempts
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement tour command attempts are immutable'
                 );
             END;

         CREATE TABLE engagement_comparison_namespaces (
             namespace_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(namespace_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             algorithm_version TEXT NOT NULL CHECK (
                 algorithm_version =
                     'migration-stream/compared-validation-fingerprint/v1'
             ),
             secret BLOB NOT NULL CHECK (length(secret) = 32),
             secret_hash TEXT NOT NULL UNIQUE CHECK (
                 length(secret_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (namespace_id, secret_hash)
         );
         CREATE TRIGGER engagement_comparison_namespaces_no_update
             BEFORE UPDATE ON engagement_comparison_namespaces
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'comparison identity namespaces are immutable'
                 );
             END;
         CREATE TRIGGER engagement_comparison_namespaces_no_delete
             BEFORE DELETE ON engagement_comparison_namespaces
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'comparison identity namespaces are immutable'
                 );
             END;

         CREATE TABLE engagement_mapping_reuse_authorities (
             source_reuse_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(source_reuse_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             tour_attempt_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_tour_attempts(tour_attempt_id)
                 ON DELETE RESTRICT,
             predecessor_tour_id TEXT NOT NULL
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             preflight_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_source_preflights(preflight_id)
                 ON DELETE RESTRICT,
             acceptance_attempt_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_acceptance_attempts(attempt_id)
                 ON DELETE RESTRICT,
             candidate_job_id TEXT NOT NULL UNIQUE
                 REFERENCES jobs(id) ON DELETE RESTRICT,
             candidate_profiling_run_id TEXT NOT NULL UNIQUE
                 REFERENCES profiling_runs(id) ON DELETE RESTRICT,
             candidate_source_hash TEXT NOT NULL CHECK (
                 length(candidate_source_hash) = 64
             ),
             candidate_manifest_hash TEXT NOT NULL CHECK (
                 length(candidate_manifest_hash) = 64
             ),
             candidate_profiling_result_hash TEXT NOT NULL CHECK (
                 length(candidate_profiling_result_hash) = 64
             ),
             baseline_job_id TEXT NOT NULL
                 REFERENCES jobs(id) ON DELETE RESTRICT,
             mapping_source_hash TEXT NOT NULL CHECK (
                 length(mapping_source_hash) = 64
             ),
             structure_json TEXT NOT NULL CHECK (
                 length(CAST(structure_json AS BLOB))
                     BETWEEN 2 AND 1048576
             ),
             baseline_structure_hash TEXT NOT NULL CHECK (
                 length(baseline_structure_hash) = 64
             ),
             candidate_structure_hash TEXT NOT NULL CHECK (
                 length(candidate_structure_hash) = 64
             ),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 128
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 32
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             profile_approval_id TEXT NOT NULL
                 REFERENCES profile_approvals(approval_id) ON DELETE RESTRICT,
             profile_approval_hash TEXT NOT NULL CHECK (
                 length(profile_approval_hash) = 64
             ),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 128
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 32
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             mapping_approval_id TEXT NOT NULL
                 REFERENCES engagement_mapping_approvals(approval_id)
                 ON DELETE RESTRICT,
             mapping_approval_hash TEXT NOT NULL CHECK (
                 length(mapping_approval_hash) = 64
             ),
             algorithm_version TEXT NOT NULL CHECK (
                 algorithm_version =
                     'migration-stream/mapping-source-reuse/v1'
             ),
             canonical_reuse_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_reuse_json AS BLOB))
                     BETWEEN 2 AND 1048576
             ),
             source_reuse_hash TEXT NOT NULL UNIQUE CHECK (
                 length(source_reuse_hash) = 64
             ),
             verified_at TEXT NOT NULL CHECK (
                 length(CAST(verified_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (source_reuse_id, source_reuse_hash),
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(
                     profile_id, profile_version, profile_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(
                     mapping_id, mapping_version, mapping_hash
                 ) ON DELETE RESTRICT,
             CHECK (baseline_structure_hash = candidate_structure_hash)
         );
         CREATE INDEX engagement_mapping_reuse_authorities_history
             ON engagement_mapping_reuse_authorities(
                 engagement_id, verified_at, source_reuse_id
             );
         CREATE TRIGGER engagement_mapping_reuse_authorities_no_update
             BEFORE UPDATE ON engagement_mapping_reuse_authorities
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'mapping source-reuse authorities are immutable'
                 );
             END;
         CREATE TRIGGER engagement_mapping_reuse_authorities_no_delete
             BEFORE DELETE ON engagement_mapping_reuse_authorities
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'mapping source-reuse authorities are immutable'
                 );
             END;

         ALTER TABLE validation_runs
             ADD COLUMN source_reuse_id TEXT
                 REFERENCES engagement_mapping_reuse_authorities(source_reuse_id)
                 ON DELETE RESTRICT;
         ALTER TABLE validation_runs
             ADD COLUMN source_reuse_hash TEXT CHECK (
                 source_reuse_hash IS NULL OR length(source_reuse_hash) = 64
             );
         CREATE INDEX validation_runs_source_reuse
             ON validation_runs(source_reuse_id, source_reuse_hash, id);
         CREATE TRIGGER validation_runs_source_reuse_insert_guard
             BEFORE INSERT ON validation_runs
             WHEN
                 (NEW.source_reuse_id IS NULL)
                     <> (NEW.source_reuse_hash IS NULL)
                 OR (
                     NEW.source_reuse_id IS NOT NULL
                     AND NOT EXISTS (
                         SELECT 1
                         FROM engagement_mapping_reuse_authorities r
                         WHERE r.source_reuse_id = NEW.source_reuse_id
                           AND r.source_reuse_hash = NEW.source_reuse_hash
                           AND r.candidate_job_id = NEW.job_id
                           AND r.candidate_source_hash = NEW.source_hash
                           AND r.profile_id = NEW.profile_id
                           AND r.profile_version = NEW.profile_version
                           AND r.profile_hash = NEW.profile_hash
                           AND r.mapping_id = NEW.mapping_id
                           AND r.mapping_version = NEW.mapping_version
                           AND r.mapping_hash = NEW.mapping_hash
                     )
                 )
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'validation source-reuse authority is absent or inconsistent'
                 );
             END;
         CREATE TRIGGER validation_runs_source_reuse_no_update
             BEFORE UPDATE OF source_reuse_id, source_reuse_hash
             ON validation_runs
             WHEN NEW.source_reuse_id IS NOT OLD.source_reuse_id
               OR NEW.source_reuse_hash IS NOT OLD.source_reuse_hash
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'validation source-reuse authority is immutable'
                 );
             END;

         CREATE TABLE engagement_tours (
             tour_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(tour_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             tour_attempt_id TEXT UNIQUE
                 REFERENCES engagement_tour_attempts(tour_attempt_id)
                 ON DELETE RESTRICT,
             tour_number INTEGER NOT NULL CHECK (tour_number > 0),
             predecessor_tour_id TEXT
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             source_reuse_id TEXT,
             source_reuse_hash TEXT CHECK (
                 source_reuse_hash IS NULL OR length(source_reuse_hash) = 64
             ),
             preflight_id TEXT NOT NULL
                 REFERENCES engagement_source_preflights(preflight_id)
                 ON DELETE RESTRICT,
             acceptance_attempt_id TEXT NOT NULL
                 REFERENCES engagement_acceptance_attempts(attempt_id)
                 ON DELETE RESTRICT,
             job_id TEXT NOT NULL UNIQUE
                 REFERENCES jobs(id) ON DELETE RESTRICT,
             profiling_run_id TEXT NOT NULL UNIQUE
                 REFERENCES profiling_runs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64),
             profiling_result_hash TEXT NOT NULL CHECK (
                 length(profiling_result_hash) = 64
             ),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 128
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 32
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             profile_approval_id TEXT NOT NULL
                 REFERENCES profile_approvals(approval_id) ON DELETE RESTRICT,
             profile_approval_hash TEXT NOT NULL CHECK (
                 length(profile_approval_hash) = 64
             ),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 128
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 32
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             mapping_approval_id TEXT NOT NULL
                 REFERENCES engagement_mapping_approvals(approval_id)
                 ON DELETE RESTRICT,
             mapping_approval_hash TEXT NOT NULL CHECK (
                 length(mapping_approval_hash) = 64
             ),
             validation_run_id TEXT NOT NULL UNIQUE
                 REFERENCES validation_runs(id) ON DELETE RESTRICT,
             validation_identity_hash TEXT NOT NULL CHECK (
                 length(validation_identity_hash) = 64
             ),
             validation_result_hash TEXT NOT NULL CHECK (
                 length(validation_result_hash) = 64
             ),
             canonical_tour_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_tour_json AS BLOB))
                     BETWEEN 2 AND 1048576
             ),
             tour_hash TEXT NOT NULL UNIQUE CHECK (length(tour_hash) = 64),
             registered_at TEXT NOT NULL CHECK (
                 length(CAST(registered_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (engagement_id, tour_number),
             UNIQUE (source_reuse_id, source_reuse_hash),
             FOREIGN KEY (source_reuse_id, source_reuse_hash)
                 REFERENCES engagement_mapping_reuse_authorities(
                     source_reuse_id, source_reuse_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(
                     profile_id, profile_version, profile_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(
                     mapping_id, mapping_version, mapping_hash
                 ) ON DELETE RESTRICT,
             CHECK (
                 (tour_number = 1
                     AND predecessor_tour_id IS NULL
                     AND tour_attempt_id IS NULL
                     AND source_reuse_id IS NULL
                     AND source_reuse_hash IS NULL)
                 OR (tour_number > 1
                     AND predecessor_tour_id IS NOT NULL
                     AND tour_attempt_id IS NOT NULL
                     AND source_reuse_id IS NOT NULL
                     AND source_reuse_hash IS NOT NULL)
             )
         );
         CREATE INDEX engagement_tours_history
             ON engagement_tours(
                 engagement_id, tour_number, registered_at, tour_id
             );
         CREATE TRIGGER engagement_tours_no_update
             BEFORE UPDATE ON engagement_tours
             BEGIN
                 SELECT RAISE(ABORT, 'registered engagement tours are immutable');
             END;
         CREATE TRIGGER engagement_tours_no_delete
             BEFORE DELETE ON engagement_tours
             BEGIN
                 SELECT RAISE(ABORT, 'registered engagement tours are immutable');
             END;

         CREATE TABLE engagement_tour_heads (
             engagement_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             generation INTEGER NOT NULL CHECK (generation >= 0),
             active_tour_id TEXT
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             pending_tour_attempt_id TEXT
                 REFERENCES engagement_tour_attempts(tour_attempt_id)
                 ON DELETE RESTRICT,
             updated_at TEXT NOT NULL CHECK (
                 length(CAST(updated_at AS BLOB)) BETWEEN 1 AND 64
             ),
             CHECK (
                 active_tour_id IS NOT NULL
                 OR pending_tour_attempt_id IS NOT NULL
             )
         );
         CREATE TRIGGER engagement_tour_heads_guarded_update
             BEFORE UPDATE ON engagement_tour_heads
             WHEN NEW.engagement_id IS NOT OLD.engagement_id
               OR NEW.generation != OLD.generation + 1
               OR (
                   OLD.active_tour_id IS NOT NULL
                   AND NEW.active_tour_id IS NULL
               )
               OR NEW.updated_at IS OLD.updated_at
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'engagement tour head transition is invalid'
                 );
             END;
         CREATE TRIGGER engagement_tour_heads_no_delete
             BEFORE DELETE ON engagement_tour_heads
             BEGIN
                 SELECT RAISE(ABORT, 'engagement tour heads cannot be deleted');
             END;

         CREATE TABLE engagement_tour_finding_sets (
             finding_set_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(finding_set_id AS BLOB)) BETWEEN 1 AND 256
             ),
             tour_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             namespace_id TEXT NOT NULL,
             namespace_secret_hash TEXT NOT NULL CHECK (
                 length(namespace_secret_hash) = 64
             ),
             algorithm_version TEXT NOT NULL CHECK (
                 algorithm_version =
                     'migration-stream/compared-validation-fingerprint/v1'
             ),
             validation_run_id TEXT NOT NULL UNIQUE
                 REFERENCES validation_runs(id) ON DELETE RESTRICT,
             validation_result_hash TEXT NOT NULL CHECK (
                 length(validation_result_hash) = 64
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             rule_set_hash TEXT NOT NULL CHECK (length(rule_set_hash) = 64),
             validation_engine_version TEXT NOT NULL CHECK (
                 length(CAST(validation_engine_version AS BLOB))
                     BETWEEN 1 AND 128
             ),
             validation_policy_hash TEXT NOT NULL CHECK (
                 length(validation_policy_hash) = 64
             ),
             distinct_finding_count INTEGER NOT NULL CHECK (
                 distinct_finding_count >= 0
             ),
             total_occurrence_count INTEGER NOT NULL CHECK (
                 total_occurrence_count >= distinct_finding_count
             ),
             membership_hash TEXT NOT NULL CHECK (
                 length(membership_hash) = 64
             ),
             canonical_set_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_set_json AS BLOB))
                     BETWEEN 2 AND 16777216
             ),
             finding_set_hash TEXT NOT NULL UNIQUE CHECK (
                 length(finding_set_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (finding_set_id, finding_set_hash),
             FOREIGN KEY (namespace_id, namespace_secret_hash)
                 REFERENCES engagement_comparison_namespaces(
                     namespace_id, secret_hash
                 ) ON DELETE RESTRICT
         );
         CREATE TRIGGER engagement_tour_finding_sets_no_update
             BEFORE UPDATE ON engagement_tour_finding_sets
             BEGIN
                 SELECT RAISE(ABORT, 'tour finding sets are immutable');
             END;
         CREATE TRIGGER engagement_tour_finding_sets_no_delete
             BEFORE DELETE ON engagement_tour_finding_sets
             BEGIN
                 SELECT RAISE(ABORT, 'tour finding sets are immutable');
             END;

         CREATE TABLE engagement_tour_finding_members (
             finding_set_id TEXT NOT NULL
                 REFERENCES engagement_tour_finding_sets(finding_set_id)
                 ON DELETE RESTRICT,
             finding_fingerprint TEXT NOT NULL CHECK (
                 length(finding_fingerprint) = 64
             ),
             occurrence_index INTEGER NOT NULL CHECK (occurrence_index > 0),
             subject_kind TEXT NOT NULL CHECK (
                 subject_kind IN (
                     'RULE_RECORD', 'UNIQUE_GROUP', 'RELATIONSHIP_RECORD'
                 )
             ),
             subject_algorithm TEXT NOT NULL CHECK (
                 subject_algorithm IN (
                     'ENTITY_KEY_HMAC_SHA256_V1',
                     'PROJECTED_ROW_HMAC_SHA256_V1',
                     'UNIQUE_SCOPE_HMAC_SHA256_V1',
                     'RELATIONSHIP_ENTITY_KEY_HMAC_SHA256_V1',
                     'RELATIONSHIP_PROJECTED_ROW_HMAC_SHA256_V1'
                 )
             ),
             record_identity_hash TEXT NOT NULL CHECK (
                 length(record_identity_hash) = 64
             ),
             validation_run_id TEXT NOT NULL
                 REFERENCES validation_runs(id) ON DELETE RESTRICT,
             issue_id TEXT NOT NULL,
             issue_hash TEXT NOT NULL CHECK (length(issue_hash) = 64),
             severity TEXT NOT NULL CHECK (
                 severity IN ('ERROR', 'WARNING', 'INFORMATION')
             ),
             rule_id TEXT NOT NULL CHECK (
                 length(CAST(rule_id AS BLOB)) BETWEEN 1 AND 256
             ),
             issue_code TEXT NOT NULL CHECK (
                 length(CAST(issue_code AS BLOB)) BETWEEN 1 AND 256
             ),
             display_message TEXT NOT NULL CHECK (
                 length(CAST(display_message AS BLOB)) BETWEEN 1 AND 4096
             ),
             entity_id TEXT NOT NULL CHECK (
                 length(CAST(entity_id AS BLOB)) BETWEEN 1 AND 256
             ),
             field_id TEXT NOT NULL CHECK (
                 length(CAST(field_id AS BLOB)) BETWEEN 1 AND 256
             ),
             relationship_id TEXT CHECK (
                 relationship_id IS NULL
                 OR length(CAST(relationship_id AS BLOB)) BETWEEN 1 AND 256
             ),
             relationship_role TEXT CHECK (
                 relationship_role IS NULL
                 OR relationship_role IN ('PARENT', 'CHILD')
             ),
             relationship_cardinality TEXT CHECK (
                 relationship_cardinality IS NULL
                 OR length(CAST(relationship_cardinality AS BLOB))
                     BETWEEN 1 AND 32
             ),
             relationship_key_pair_hash TEXT CHECK (
                 relationship_key_pair_hash IS NULL
                 OR length(relationship_key_pair_hash) = 64
             ),
             source_file TEXT NOT NULL CHECK (
                 length(CAST(source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             canonical_member_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_member_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             member_hash TEXT NOT NULL UNIQUE CHECK (
                 length(member_hash) = 64
             ),
             PRIMARY KEY (
                 finding_set_id, finding_fingerprint, occurrence_index
             ),
             UNIQUE (finding_set_id, validation_run_id, issue_id),
             FOREIGN KEY (validation_run_id, issue_id)
                 REFERENCES validation_issues(run_id, issue_id)
                 ON DELETE RESTRICT,
             CHECK (
                 (
                     subject_kind = 'RULE_RECORD'
                     AND subject_algorithm IN (
                         'ENTITY_KEY_HMAC_SHA256_V1',
                         'PROJECTED_ROW_HMAC_SHA256_V1'
                     )
                     AND relationship_id IS NULL
                     AND relationship_role IS NULL
                     AND relationship_cardinality IS NULL
                     AND relationship_key_pair_hash IS NULL
                 )
                 OR (
                     subject_kind = 'UNIQUE_GROUP'
                     AND subject_algorithm =
                         'UNIQUE_SCOPE_HMAC_SHA256_V1'
                     AND relationship_id IS NULL
                     AND relationship_role IS NULL
                     AND relationship_cardinality IS NULL
                     AND relationship_key_pair_hash IS NULL
                 )
                 OR (
                     subject_kind = 'RELATIONSHIP_RECORD'
                     AND subject_algorithm IN (
                         'RELATIONSHIP_ENTITY_KEY_HMAC_SHA256_V1',
                         'RELATIONSHIP_PROJECTED_ROW_HMAC_SHA256_V1'
                     )
                     AND
                     relationship_id IS NOT NULL
                     AND relationship_role IS NOT NULL
                     AND relationship_cardinality IS NOT NULL
                     AND relationship_key_pair_hash IS NOT NULL
                 )
             )
         );
         CREATE INDEX engagement_tour_finding_members_subject
             ON engagement_tour_finding_members(
                 finding_set_id, finding_fingerprint, occurrence_index
             );
         CREATE TRIGGER engagement_tour_finding_members_no_update
             BEFORE UPDATE ON engagement_tour_finding_members
             BEGIN
                 SELECT RAISE(ABORT, 'tour finding members are immutable');
             END;
         CREATE TRIGGER engagement_tour_finding_members_no_delete
             BEFORE DELETE ON engagement_tour_finding_members
             BEGIN
                 SELECT RAISE(ABORT, 'tour finding members are immutable');
             END;

         CREATE TABLE engagement_validation_comparisons (
             comparison_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(comparison_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             baseline_tour_id TEXT NOT NULL
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             revised_tour_id TEXT NOT NULL
                 REFERENCES engagement_tours(tour_id) ON DELETE RESTRICT,
             baseline_finding_set_id TEXT NOT NULL,
             baseline_finding_set_hash TEXT NOT NULL CHECK (
                 length(baseline_finding_set_hash) = 64
             ),
             baseline_membership_hash TEXT NOT NULL CHECK (
                 length(baseline_membership_hash) = 64
             ),
             revised_finding_set_id TEXT NOT NULL,
             revised_finding_set_hash TEXT NOT NULL CHECK (
                 length(revised_finding_set_hash) = 64
             ),
             revised_membership_hash TEXT NOT NULL CHECK (
                 length(revised_membership_hash) = 64
             ),
             namespace_id TEXT NOT NULL,
             namespace_secret_hash TEXT NOT NULL CHECK (
                 length(namespace_secret_hash) = 64
             ),
             algorithm_version TEXT NOT NULL CHECK (
                 algorithm_version =
                     'migration-stream/compared-validation-fingerprint/v1'
             ),
             profile_id TEXT NOT NULL CHECK (
                 length(CAST(profile_id AS BLOB)) BETWEEN 1 AND 128
             ),
             profile_version TEXT NOT NULL CHECK (
                 length(CAST(profile_version AS BLOB)) BETWEEN 1 AND 32
             ),
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 128
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 32
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             rule_set_hash TEXT NOT NULL CHECK (length(rule_set_hash) = 64),
             validation_engine_version TEXT NOT NULL CHECK (
                 length(CAST(validation_engine_version AS BLOB))
                     BETWEEN 1 AND 128
             ),
             validation_policy_hash TEXT NOT NULL CHECK (
                 length(validation_policy_hash) = 64
             ),
             closed_count INTEGER NOT NULL CHECK (closed_count >= 0),
             remaining_count INTEGER NOT NULL CHECK (remaining_count >= 0),
             new_count INTEGER NOT NULL CHECK (new_count >= 0),
             canonical_comparison_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_comparison_json AS BLOB))
                     BETWEEN 2 AND 16777216
             ),
             comparison_hash TEXT NOT NULL UNIQUE CHECK (
                 length(comparison_hash) = 64
             ),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (engagement_id, baseline_tour_id, revised_tour_id),
             FOREIGN KEY (
                 baseline_finding_set_id, baseline_finding_set_hash
             ) REFERENCES engagement_tour_finding_sets(
                 finding_set_id, finding_set_hash
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 revised_finding_set_id, revised_finding_set_hash
             ) REFERENCES engagement_tour_finding_sets(
                 finding_set_id, finding_set_hash
             ) ON DELETE RESTRICT,
             FOREIGN KEY (namespace_id, namespace_secret_hash)
                 REFERENCES engagement_comparison_namespaces(
                     namespace_id, secret_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(
                     profile_id, profile_version, profile_hash
                 ) ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(
                     mapping_id, mapping_version, mapping_hash
                 ) ON DELETE RESTRICT,
             CHECK (baseline_tour_id <> revised_tour_id)
         );
         CREATE INDEX engagement_validation_comparisons_history
             ON engagement_validation_comparisons(
                 engagement_id, created_at, comparison_id
             );
         CREATE TRIGGER engagement_validation_comparisons_no_update
             BEFORE UPDATE ON engagement_validation_comparisons
             BEGIN
                 SELECT RAISE(ABORT, 'validation comparisons are immutable');
             END;
         CREATE TRIGGER engagement_validation_comparisons_no_delete
             BEFORE DELETE ON engagement_validation_comparisons
             BEGIN
                 SELECT RAISE(ABORT, 'validation comparisons are immutable');
             END;

         CREATE TABLE engagement_validation_comparison_items (
             comparison_id TEXT NOT NULL
                 REFERENCES engagement_validation_comparisons(comparison_id)
                 ON DELETE RESTRICT,
             category TEXT NOT NULL CHECK (
                 category IN ('CLOSED', 'REMAINING', 'NEW')
             ),
             finding_fingerprint TEXT NOT NULL CHECK (
                 length(finding_fingerprint) = 64
             ),
             category_occurrence_index INTEGER NOT NULL CHECK (
                 category_occurrence_index > 0
             ),
             baseline_finding_set_id TEXT,
             baseline_occurrence_index INTEGER CHECK (
                 baseline_occurrence_index IS NULL
                 OR baseline_occurrence_index > 0
             ),
             baseline_member_hash TEXT CHECK (
                 baseline_member_hash IS NULL
                 OR length(baseline_member_hash) = 64
             ),
             revised_finding_set_id TEXT,
             revised_occurrence_index INTEGER CHECK (
                 revised_occurrence_index IS NULL
                 OR revised_occurrence_index > 0
             ),
             revised_member_hash TEXT CHECK (
                 revised_member_hash IS NULL
                 OR length(revised_member_hash) = 64
             ),
             explanation_code TEXT NOT NULL CHECK (
                 length(CAST(explanation_code AS BLOB)) BETWEEN 1 AND 128
             ),
             canonical_item_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_item_json AS BLOB))
                     BETWEEN 2 AND 65536
             ),
             item_hash TEXT NOT NULL UNIQUE CHECK (length(item_hash) = 64),
             PRIMARY KEY (
                 comparison_id, category, finding_fingerprint,
                 category_occurrence_index
             ),
             FOREIGN KEY (
                 baseline_finding_set_id,
                 finding_fingerprint,
                 baseline_occurrence_index
             ) REFERENCES engagement_tour_finding_members(
                 finding_set_id, finding_fingerprint, occurrence_index
             ) ON DELETE RESTRICT,
             FOREIGN KEY (
                 revised_finding_set_id,
                 finding_fingerprint,
                 revised_occurrence_index
             ) REFERENCES engagement_tour_finding_members(
                 finding_set_id, finding_fingerprint, occurrence_index
             ) ON DELETE RESTRICT,
             CHECK (
                 (
                     category = 'CLOSED'
                     AND baseline_finding_set_id IS NOT NULL
                     AND baseline_occurrence_index IS NOT NULL
                     AND baseline_member_hash IS NOT NULL
                     AND revised_finding_set_id IS NULL
                     AND revised_occurrence_index IS NULL
                     AND revised_member_hash IS NULL
                 )
                 OR (
                     category = 'REMAINING'
                     AND baseline_finding_set_id IS NOT NULL
                     AND baseline_occurrence_index IS NOT NULL
                     AND baseline_member_hash IS NOT NULL
                     AND revised_finding_set_id IS NOT NULL
                     AND revised_occurrence_index IS NOT NULL
                     AND revised_member_hash IS NOT NULL
                 )
                 OR (
                     category = 'NEW'
                     AND baseline_finding_set_id IS NULL
                     AND baseline_occurrence_index IS NULL
                     AND baseline_member_hash IS NULL
                     AND revised_finding_set_id IS NOT NULL
                     AND revised_occurrence_index IS NOT NULL
                     AND revised_member_hash IS NOT NULL
                 )
             )
         );
         CREATE INDEX engagement_validation_comparison_items_category
             ON engagement_validation_comparison_items(
                 comparison_id, category, finding_fingerprint,
                 category_occurrence_index
             );
         CREATE TRIGGER engagement_validation_comparison_items_no_update
             BEFORE UPDATE ON engagement_validation_comparison_items
             BEGIN
                 SELECT RAISE(ABORT, 'validation comparison items are immutable');
             END;
         CREATE TRIGGER engagement_validation_comparison_items_no_delete
             BEFORE DELETE ON engagement_validation_comparison_items
             BEGIN
                 SELECT RAISE(ABORT, 'validation comparison items are immutable');
             END;

         PRAGMA user_version = 11;
         COMMIT;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn migrate_v11_to_v12(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    let destination_columns = [
        (
            "destination_origin",
            "ALTER TABLE engagement_active_context
             ADD COLUMN destination_origin TEXT CHECK (
                 destination_origin IS NULL
                 OR destination_origin IN ('CONTROLLED_PACK', 'WORKSPACE_PROFILE')
             )",
        ),
        (
            "destination_authority_id",
            "ALTER TABLE engagement_active_context
             ADD COLUMN destination_authority_id TEXT CHECK (
                 destination_authority_id IS NULL
                 OR length(CAST(destination_authority_id AS BLOB)) BETWEEN 1 AND 256
             )",
        ),
        (
            "destination_origin_hash",
            "ALTER TABLE engagement_active_context
             ADD COLUMN destination_origin_hash TEXT CHECK (
                 destination_origin_hash IS NULL
                 OR length(destination_origin_hash) = 64
             )",
        ),
    ];
    for (column, statement) in destination_columns {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM pragma_table_info('engagement_active_context')
                 WHERE name = ?1
             )",
            [column],
            |row| row.get(0),
        )?;
        if !exists {
            transaction.execute_batch(statement)?;
        }
    }
    transaction.execute_batch(
        "
         UPDATE engagement_active_context
            SET destination_origin = 'CONTROLLED_PACK',
                destination_authority_id = selected_object_pack_id,
                destination_origin_hash = selected_profile_hash
          WHERE profile_stage IN ('REVIEWED', 'APPROVED');

         CREATE TRIGGER IF NOT EXISTS engagement_active_destination_insert_guard
             BEFORE INSERT ON engagement_active_context
             WHEN (
                 (NEW.profile_stage = 'NOT_SELECTED'
                  AND (NEW.destination_origin IS NOT NULL
                       OR NEW.destination_authority_id IS NOT NULL
                       OR NEW.destination_origin_hash IS NOT NULL))
                 OR
                 (NEW.profile_stage IN ('REVIEWED', 'APPROVED')
                  AND (NEW.destination_origin IS NULL
                       OR NEW.destination_authority_id IS NULL
                       OR NEW.destination_origin_hash IS NULL))
                 OR
                 (NEW.profile_stage = 'APPROVED'
                  AND NEW.destination_origin = 'WORKSPACE_PROFILE'
                  AND NOT EXISTS (
                      SELECT 1 FROM profile_approvals
                       WHERE profile_id = NEW.selected_profile_id
                         AND profile_version = NEW.selected_profile_version
                         AND profile_hash = NEW.selected_profile_hash
                         AND acknowledged = 1
                  ))
             )
             BEGIN
                 SELECT RAISE(ABORT, 'active destination authority is incomplete');
             END;
         CREATE TRIGGER IF NOT EXISTS engagement_active_destination_update_guard
             BEFORE UPDATE ON engagement_active_context
             WHEN (
                 (NEW.profile_stage = 'NOT_SELECTED'
                  AND (NEW.destination_origin IS NOT NULL
                       OR NEW.destination_authority_id IS NOT NULL
                       OR NEW.destination_origin_hash IS NOT NULL))
                 OR
                 (NEW.profile_stage IN ('REVIEWED', 'APPROVED')
                  AND (NEW.destination_origin IS NULL
                       OR NEW.destination_authority_id IS NULL
                       OR NEW.destination_origin_hash IS NULL))
                 OR
                 (NEW.profile_stage = 'APPROVED'
                  AND NEW.destination_origin = 'WORKSPACE_PROFILE'
                  AND NOT EXISTS (
                      SELECT 1 FROM profile_approvals
                       WHERE profile_id = NEW.selected_profile_id
                         AND profile_version = NEW.selected_profile_version
                         AND profile_hash = NEW.selected_profile_hash
                         AND acknowledged = 1
                  ))
             )
             BEGIN
                 SELECT RAISE(ABORT, 'active destination authority is incomplete');
             END;

         CREATE TABLE IF NOT EXISTS destination_artifact_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             workspace_id TEXT NOT NULL,
             workspace_descriptor_hash TEXT NOT NULL CHECK (
                 length(workspace_descriptor_hash) = 64
             ),
             idempotency_key TEXT NOT NULL,
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL,
             state TEXT NOT NULL CHECK (
                 state IN ('RECEIVING', 'READY', 'REJECTED', 'INTERRUPTED')
             ),
             partial_relative_path TEXT NOT NULL,
             artifact_label TEXT NOT NULL,
             source_name TEXT NOT NULL,
             system_context TEXT,
             client_context TEXT,
             release_context TEXT,
             source_sha256 TEXT CHECK (
                 source_sha256 IS NULL OR length(source_sha256) = 64
             ),
             source_size INTEGER CHECK (source_size IS NULL OR source_size >= 0),
             snapshot_id TEXT REFERENCES metadata_snapshots(id) ON DELETE RESTRICT,
             catalogue_authority_id TEXT,
             error_code TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             completed_at TEXT,
             UNIQUE (engagement_id, idempotency_key),
             CHECK (
                 (state = 'RECEIVING'
                  AND source_sha256 IS NULL
                  AND source_size IS NULL
                  AND snapshot_id IS NULL
                  AND catalogue_authority_id IS NULL
                  AND completed_at IS NULL)
                 OR
                 (state = 'READY'
                  AND source_sha256 IS NOT NULL
                  AND source_size IS NOT NULL
                  AND snapshot_id IS NOT NULL
                  AND catalogue_authority_id IS NOT NULL
                  AND error_code IS NULL
                  AND completed_at IS NOT NULL)
                 OR
                 (state IN ('REJECTED', 'INTERRUPTED')
                  AND error_code IS NOT NULL
                  AND completed_at IS NOT NULL)
             )
         );
         CREATE TABLE IF NOT EXISTS destination_artifact_events (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             attempt_id TEXT NOT NULL
                 REFERENCES destination_artifact_attempts(attempt_id) ON DELETE RESTRICT,
             state TEXT NOT NULL CHECK (
                 state IN ('RECEIVING', 'READY', 'REJECTED', 'INTERRUPTED')
             ),
             event_hash TEXT NOT NULL CHECK (length(event_hash) = 64),
             previous_event_hash TEXT CHECK (
                 previous_event_hash IS NULL OR length(previous_event_hash) = 64
             ),
             occurred_at TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS destination_catalogue_authorities (
             catalogue_authority_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             origin TEXT NOT NULL CHECK (origin IN ('EDMX', 'MANUAL')),
             snapshot_id TEXT REFERENCES metadata_snapshots(id) ON DELETE RESTRICT,
             snapshot_sha256 TEXT CHECK (
                 snapshot_sha256 IS NULL OR length(snapshot_sha256) = 64
             ),
             catalogue_path TEXT,
             catalogue_file_sha256 TEXT CHECK (
                 catalogue_file_sha256 IS NULL OR length(catalogue_file_sha256) = 64
             ),
             catalogue_payload_sha256 TEXT NOT NULL CHECK (
                 length(catalogue_payload_sha256) = 64
             ),
             canonical_catalogue_json TEXT NOT NULL,
             catalogue_hash TEXT NOT NULL CHECK (length(catalogue_hash) = 64),
             compatibility_report_json TEXT NOT NULL,
             compatibility_report_hash TEXT NOT NULL CHECK (
                 length(compatibility_report_hash) = 64
             ),
             report_algorithm TEXT NOT NULL,
             artifact_label TEXT NOT NULL,
             odata_generation TEXT,
             created_at TEXT NOT NULL,
             UNIQUE (engagement_id, catalogue_hash),
             CHECK (
                 (origin = 'EDMX'
                  AND snapshot_id IS NOT NULL
                  AND snapshot_sha256 IS NOT NULL
                  AND catalogue_path IS NOT NULL
                  AND catalogue_file_sha256 IS NOT NULL
                  AND odata_generation IS NOT NULL)
                 OR
                 (origin = 'MANUAL'
                  AND snapshot_id IS NULL
                  AND snapshot_sha256 IS NULL
                  AND catalogue_path IS NULL
                  AND catalogue_file_sha256 IS NULL
                  AND odata_generation IS NULL)
             )
         );

         CREATE TABLE IF NOT EXISTS destination_profile_draft_heads (
             draft_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             catalogue_authority_id TEXT NOT NULL
                 REFERENCES destination_catalogue_authorities(catalogue_authority_id)
                 ON DELETE RESTRICT,
             catalogue_hash TEXT NOT NULL CHECK (length(catalogue_hash) = 64),
             generation INTEGER NOT NULL CHECK (generation > 0),
             state TEXT NOT NULL CHECK (state IN ('EDITABLE', 'FROZEN')),
             head_revision_hash TEXT NOT NULL CHECK (length(head_revision_hash) = 64),
             frozen_profile_id TEXT,
             frozen_profile_version TEXT,
             frozen_profile_hash TEXT CHECK (
                 frozen_profile_hash IS NULL OR length(frozen_profile_hash) = 64
             ),
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             CHECK (
                 (state = 'EDITABLE'
                  AND frozen_profile_id IS NULL
                  AND frozen_profile_version IS NULL
                  AND frozen_profile_hash IS NULL)
                 OR
                 (state = 'FROZEN'
                  AND frozen_profile_id IS NOT NULL
                  AND frozen_profile_version IS NOT NULL
                  AND frozen_profile_hash IS NOT NULL)
             )
         );
         CREATE UNIQUE INDEX IF NOT EXISTS destination_one_editable_draft_per_catalogue
             ON destination_profile_draft_heads(
                 engagement_id, catalogue_authority_id
             ) WHERE state = 'EDITABLE';
         CREATE TABLE IF NOT EXISTS destination_profile_draft_revisions (
             draft_id TEXT NOT NULL
                 REFERENCES destination_profile_draft_heads(draft_id) ON DELETE RESTRICT,
             generation INTEGER NOT NULL CHECK (generation > 0),
             revision_hash TEXT NOT NULL CHECK (length(revision_hash) = 64),
             parent_revision_hash TEXT CHECK (
                 parent_revision_hash IS NULL OR length(parent_revision_hash) = 64
             ),
             canonical_draft_json TEXT NOT NULL,
             actor TEXT NOT NULL,
             reason TEXT NOT NULL,
             created_at TEXT NOT NULL,
             PRIMARY KEY (draft_id, generation),
             UNIQUE (draft_id, revision_hash),
             CHECK (
                 (generation = 1 AND parent_revision_hash IS NULL)
                 OR (generation > 1 AND parent_revision_hash IS NOT NULL)
             )
         );
         CREATE TRIGGER IF NOT EXISTS destination_profile_draft_revisions_insert_guard
             BEFORE INSERT ON destination_profile_draft_revisions
             WHEN NOT (
                 (
                     NEW.generation = 1
                     AND NEW.parent_revision_hash IS NULL
                     AND EXISTS (
                         SELECT 1 FROM destination_profile_draft_heads AS head
                          WHERE head.draft_id = NEW.draft_id
                            AND head.generation = 1
                            AND head.head_revision_hash = NEW.revision_hash
                     )
                     AND NOT EXISTS (
                         SELECT 1 FROM destination_profile_draft_revisions
                          WHERE draft_id = NEW.draft_id
                     )
                 )
                 OR
                 (
                     EXISTS (
                         SELECT 1 FROM destination_profile_draft_heads AS head
                          WHERE head.draft_id = NEW.draft_id
                            AND head.state = 'EDITABLE'
                            AND NEW.generation = head.generation + 1
                            AND NEW.parent_revision_hash = head.head_revision_hash
                     )
                 )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'destination draft revision is not the next immutable generation');
             END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_draft_heads_transition_guard
             BEFORE UPDATE ON destination_profile_draft_heads
             WHEN (
                 NEW.draft_id IS NOT OLD.draft_id
                 OR NEW.engagement_id IS NOT OLD.engagement_id
                 OR NEW.catalogue_authority_id IS NOT OLD.catalogue_authority_id
                 OR NEW.catalogue_hash IS NOT OLD.catalogue_hash
                 OR NEW.created_at IS NOT OLD.created_at
                 OR NOT (
                     (
                         OLD.state = 'EDITABLE' AND NEW.state = 'EDITABLE'
                         AND NEW.generation = OLD.generation + 1
                         AND EXISTS (
                             SELECT 1
                               FROM destination_profile_draft_revisions AS revision
                              WHERE revision.draft_id = OLD.draft_id
                                AND revision.generation = NEW.generation
                                AND revision.revision_hash = NEW.head_revision_hash
                                AND revision.parent_revision_hash = OLD.head_revision_hash
                         )
                     )
                     OR
                     (
                         OLD.state = 'EDITABLE' AND NEW.state = 'FROZEN'
                         AND NEW.generation = OLD.generation
                         AND NEW.head_revision_hash = OLD.head_revision_hash
                         AND EXISTS (
                             SELECT 1 FROM profile_versions AS profile
                              WHERE profile.profile_id = NEW.frozen_profile_id
                                AND profile.profile_version = NEW.frozen_profile_version
                                AND profile.profile_hash = NEW.frozen_profile_hash
                         )
                     )
                 )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'destination draft head transition is not monotonic');
             END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_draft_heads_no_delete
             BEFORE DELETE ON destination_profile_draft_heads
             BEGIN SELECT RAISE(ABORT, 'destination draft heads are immutable'); END;

         CREATE TABLE IF NOT EXISTS destination_workshop_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             command_kind TEXT NOT NULL,
             idempotency_key TEXT NOT NULL,
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL,
             expected_generation INTEGER NOT NULL CHECK (expected_generation >= 0),
             result_generation INTEGER NOT NULL CHECK (result_generation >= 0),
             result_reference TEXT NOT NULL,
             result_hash TEXT NOT NULL CHECK (length(result_hash) = 64),
             created_at TEXT NOT NULL,
             UNIQUE (engagement_id, command_kind, idempotency_key)
         );

         CREATE TABLE IF NOT EXISTS destination_profile_origin_bindings (
             origin_binding_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             draft_id TEXT NOT NULL
                 REFERENCES destination_profile_draft_heads(draft_id) ON DELETE RESTRICT,
             draft_generation INTEGER NOT NULL CHECK (draft_generation > 0),
             draft_revision_hash TEXT NOT NULL CHECK (
                 length(draft_revision_hash) = 64
             ),
             catalogue_authority_id TEXT NOT NULL
                 REFERENCES destination_catalogue_authorities(catalogue_authority_id)
                 ON DELETE RESTRICT,
             catalogue_hash TEXT NOT NULL CHECK (length(catalogue_hash) = 64),
             compatibility_report_hash TEXT NOT NULL CHECK (
                 length(compatibility_report_hash) = 64
             ),
             canonical_origin_json TEXT NOT NULL,
             origin_binding_hash TEXT NOT NULL CHECK (
                 length(origin_binding_hash) = 64
             ),
             created_at TEXT NOT NULL,
             UNIQUE (profile_id, profile_version, profile_hash),
             UNIQUE (origin_binding_hash)
         );

         CREATE TABLE IF NOT EXISTS destination_rebases (
             rebase_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             from_draft_id TEXT NOT NULL,
             from_revision_hash TEXT NOT NULL CHECK (length(from_revision_hash) = 64),
             to_catalogue_authority_id TEXT NOT NULL,
             to_catalogue_hash TEXT NOT NULL CHECK (length(to_catalogue_hash) = 64),
             result_draft_id TEXT NOT NULL,
             result_revision_hash TEXT NOT NULL CHECK (length(result_revision_hash) = 64),
             algorithm TEXT NOT NULL,
             canonical_diff_json TEXT NOT NULL,
             diff_hash TEXT NOT NULL CHECK (length(diff_hash) = 64),
             actor TEXT NOT NULL,
             reason TEXT NOT NULL,
             created_at TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS destination_mapping_carry_forwards (
             carry_forward_id TEXT PRIMARY KEY NOT NULL,
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             source_mapping_id TEXT NOT NULL,
             source_mapping_version TEXT NOT NULL,
             source_mapping_hash TEXT NOT NULL CHECK (length(source_mapping_hash) = 64),
             source_profile_id TEXT NOT NULL,
             source_profile_version TEXT NOT NULL,
             source_profile_hash TEXT NOT NULL CHECK (length(source_profile_hash) = 64),
             target_profile_id TEXT NOT NULL,
             target_profile_version TEXT NOT NULL,
             target_profile_hash TEXT NOT NULL CHECK (length(target_profile_hash) = 64),
             result_draft_id TEXT NOT NULL,
             result_draft_hash TEXT NOT NULL CHECK (length(result_draft_hash) = 64),
             algorithm TEXT NOT NULL,
             canonical_result_json TEXT NOT NULL,
             result_hash TEXT NOT NULL CHECK (length(result_hash) = 64),
             actor TEXT NOT NULL,
             reason TEXT NOT NULL,
             created_at TEXT NOT NULL
         );

         CREATE TRIGGER IF NOT EXISTS destination_artifact_events_no_update
             BEFORE UPDATE ON destination_artifact_events
             BEGIN SELECT RAISE(ABORT, 'destination artifact events are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_artifact_events_no_delete
             BEFORE DELETE ON destination_artifact_events
             BEGIN SELECT RAISE(ABORT, 'destination artifact events are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_catalogue_authorities_no_update
             BEFORE UPDATE ON destination_catalogue_authorities
             BEGIN SELECT RAISE(ABORT, 'destination catalogue authority is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_catalogue_authorities_no_delete
             BEFORE DELETE ON destination_catalogue_authorities
             BEGIN SELECT RAISE(ABORT, 'destination catalogue authority is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_draft_revisions_no_update
             BEFORE UPDATE ON destination_profile_draft_revisions
             BEGIN SELECT RAISE(ABORT, 'destination draft revisions are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_draft_revisions_no_delete
             BEFORE DELETE ON destination_profile_draft_revisions
             BEGIN SELECT RAISE(ABORT, 'destination draft revisions are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_workshop_attempts_no_update
             BEFORE UPDATE ON destination_workshop_attempts
             BEGIN SELECT RAISE(ABORT, 'destination workshop attempts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_workshop_attempts_no_delete
             BEFORE DELETE ON destination_workshop_attempts
             BEGIN SELECT RAISE(ABORT, 'destination workshop attempts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_origin_bindings_no_update
             BEFORE UPDATE ON destination_profile_origin_bindings
             BEGIN SELECT RAISE(ABORT, 'destination profile origins are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_profile_origin_bindings_no_delete
             BEFORE DELETE ON destination_profile_origin_bindings
             BEGIN SELECT RAISE(ABORT, 'destination profile origins are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_rebases_no_update
             BEFORE UPDATE ON destination_rebases
             BEGIN SELECT RAISE(ABORT, 'destination rebases are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_rebases_no_delete
             BEFORE DELETE ON destination_rebases
             BEGIN SELECT RAISE(ABORT, 'destination rebases are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_mapping_carry_forwards_no_update
             BEFORE UPDATE ON destination_mapping_carry_forwards
             BEGIN SELECT RAISE(ABORT, 'destination mapping carry-forward evidence is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS destination_mapping_carry_forwards_no_delete
             BEFORE DELETE ON destination_mapping_carry_forwards
             BEGIN SELECT RAISE(ABORT, 'destination mapping carry-forward evidence is immutable'); END;

         PRAGMA user_version = 12;",
    )?;
    transaction.commit()?;
    Ok(())
}

/// Bind every new engagement-owned validation launch to one immutable,
/// independently replayable authority envelope and one preallocated run ID.
///
/// Rows created before this migration remain visibly legacy so existing
/// controlled-pack workspaces can still be inspected and recovered. The
/// insert guard prevents any new legacy row after schema 13 is active.
#[allow(clippy::too_many_lines)]
fn migrate_v12_to_v13(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    let columns = [
        (
            "authority_schema",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN authority_schema TEXT NOT NULL DEFAULT 'LEGACY_V9'
             CHECK (authority_schema IN ('LEGACY_V9', 'EXACT_V13'))",
        ),
        (
            "expected_engagement_generation",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN expected_engagement_generation INTEGER
             CHECK (
                 expected_engagement_generation IS NULL
                 OR expected_engagement_generation >= 0
             )",
        ),
        (
            "planned_run_id",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN planned_run_id TEXT",
        ),
        (
            "source_hash",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN source_hash TEXT
             CHECK (source_hash IS NULL OR length(source_hash) = 64)",
        ),
        (
            "profile_id",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN profile_id TEXT",
        ),
        (
            "profile_version",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN profile_version TEXT",
        ),
        (
            "profile_hash",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN profile_hash TEXT
             CHECK (profile_hash IS NULL OR length(profile_hash) = 64)",
        ),
        (
            "profile_approval_id",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN profile_approval_id TEXT",
        ),
        (
            "profile_approval_hash",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN profile_approval_hash TEXT
             CHECK (
                 profile_approval_hash IS NULL
                 OR length(profile_approval_hash) = 64
             )",
        ),
        (
            "mapping_mode",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN mapping_mode TEXT
             CHECK (
                 mapping_mode IS NULL
                 OR mapping_mode IN ('CONTROLLED_SUGGESTIONS', 'OPERATOR_GOVERNED')
             )",
        ),
        (
            "mapping_approval_id",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN mapping_approval_id TEXT",
        ),
        (
            "mapping_approval_hash",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN mapping_approval_hash TEXT
             CHECK (
                 mapping_approval_hash IS NULL
                 OR length(mapping_approval_hash) = 64
             )",
        ),
        (
            "canonical_authority_json",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN canonical_authority_json TEXT",
        ),
        (
            "authority_hash",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN authority_hash TEXT
             CHECK (authority_hash IS NULL OR length(authority_hash) = 64)",
        ),
        (
            "attempt_state",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN attempt_state TEXT NOT NULL DEFAULT 'LEGACY'
             CHECK (attempt_state IN ('LEGACY', 'RESERVED', 'COMPLETE', 'FAILED'))",
        ),
        (
            "error_code",
            "ALTER TABLE engagement_validation_attempts
             ADD COLUMN error_code TEXT",
        ),
    ];
    for (column, statement) in columns {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM pragma_table_info('engagement_validation_attempts')
                 WHERE name = ?1
             )",
            [column],
            |row| row.get(0),
        )?;
        if !exists {
            transaction.execute_batch(statement)?;
        }
    }
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS engagement_validation_attempts_planned_run
             ON engagement_validation_attempts(planned_run_id)
             WHERE planned_run_id IS NOT NULL;

         CREATE TRIGGER IF NOT EXISTS engagement_validation_attempts_exact_insert_guard
             BEFORE INSERT ON engagement_validation_attempts
             WHEN (
                 NEW.authority_schema <> 'EXACT_V13'
                 OR NEW.expected_engagement_generation IS NULL
                 OR NEW.planned_run_id IS NULL
                 OR length(CAST(NEW.planned_run_id AS BLOB)) NOT BETWEEN 1 AND 128
                 OR NEW.source_hash IS NULL
                 OR NEW.profile_id IS NULL
                 OR NEW.profile_version IS NULL
                 OR NEW.profile_hash IS NULL
                 OR NEW.profile_approval_id IS NULL
                 OR NEW.profile_approval_hash IS NULL
                 OR NEW.mapping_mode IS NULL
                 OR (
                     NEW.mapping_mode = 'OPERATOR_GOVERNED'
                     AND (
                         NEW.mapping_approval_id IS NULL
                         OR NEW.mapping_approval_hash IS NULL
                     )
                 )
                 OR (
                     NEW.mapping_mode = 'CONTROLLED_SUGGESTIONS'
                     AND (
                         NEW.mapping_approval_id IS NOT NULL
                         OR NEW.mapping_approval_hash IS NOT NULL
                     )
                 )
                 OR NEW.canonical_authority_json IS NULL
                 OR length(CAST(NEW.canonical_authority_json AS BLOB))
                    NOT BETWEEN 2 AND 65536
                 OR NEW.authority_hash IS NULL
                 OR NEW.attempt_state <> 'RESERVED'
                 OR NEW.run_id IS NOT NULL
                 OR NEW.error_code IS NOT NULL
                 OR NOT EXISTS (
                     SELECT 1 FROM profile_approvals AS approval
                      WHERE approval.approval_id = NEW.profile_approval_id
                        AND approval.profile_id = NEW.profile_id
                        AND approval.profile_version = NEW.profile_version
                        AND approval.profile_hash = NEW.profile_hash
                        AND approval.approval_hash = NEW.profile_approval_hash
                        AND approval.acknowledged = 1
                 )
                 OR (
                     NEW.mapping_mode = 'OPERATOR_GOVERNED'
                     AND NOT EXISTS (
                         SELECT 1 FROM engagement_mapping_approvals AS approval
                          WHERE approval.approval_id = NEW.mapping_approval_id
                            AND approval.engagement_id = NEW.engagement_id
                            AND approval.mapping_id = NEW.mapping_id
                            AND approval.mapping_version = NEW.mapping_version
                            AND approval.mapping_hash = NEW.mapping_hash
                            AND approval.approval_hash = NEW.mapping_approval_hash
                     )
                 )
             )
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'validation attempt lacks exact immutable authority'
                 );
             END;

         CREATE TRIGGER IF NOT EXISTS engagement_validation_attempts_transition_guard
             BEFORE UPDATE ON engagement_validation_attempts
             WHEN NOT (
                 NEW.attempt_id IS OLD.attempt_id
                 AND NEW.engagement_id IS OLD.engagement_id
                 AND NEW.idempotency_key IS OLD.idempotency_key
                 AND NEW.request_hash IS OLD.request_hash
                 AND NEW.canonical_request_json IS OLD.canonical_request_json
                 AND NEW.job_id IS OLD.job_id
                 AND NEW.mapping_id IS OLD.mapping_id
                 AND NEW.mapping_version IS OLD.mapping_version
                 AND NEW.mapping_hash IS OLD.mapping_hash
                 AND NEW.policy_hash IS OLD.policy_hash
                 AND NEW.created_at IS OLD.created_at
                 AND NEW.authority_schema IS OLD.authority_schema
                 AND NEW.expected_engagement_generation
                     IS OLD.expected_engagement_generation
                 AND NEW.planned_run_id IS OLD.planned_run_id
                 AND NEW.source_hash IS OLD.source_hash
                 AND NEW.profile_id IS OLD.profile_id
                 AND NEW.profile_version IS OLD.profile_version
                 AND NEW.profile_hash IS OLD.profile_hash
                 AND NEW.profile_approval_id IS OLD.profile_approval_id
                 AND NEW.profile_approval_hash IS OLD.profile_approval_hash
                 AND NEW.mapping_mode IS OLD.mapping_mode
                 AND NEW.mapping_approval_id IS OLD.mapping_approval_id
                 AND NEW.mapping_approval_hash IS OLD.mapping_approval_hash
                 AND NEW.canonical_authority_json IS OLD.canonical_authority_json
                 AND NEW.authority_hash IS OLD.authority_hash
                 AND (
                     (
                         OLD.authority_schema = 'EXACT_V13'
                         AND OLD.attempt_state = 'RESERVED'
                         AND OLD.run_id IS NULL
                         AND (
                             (
                                 NEW.attempt_state = 'COMPLETE'
                                 AND NEW.run_id IS OLD.planned_run_id
                                 AND NEW.error_code IS NULL
                             )
                             OR
                             (
                                 NEW.attempt_state = 'FAILED'
                                 AND NEW.run_id IS NULL
                                 AND length(CAST(NEW.error_code AS BLOB))
                                     BETWEEN 1 AND 256
                             )
                         )
                     )
                     OR
                     (
                         OLD.authority_schema = 'LEGACY_V9'
                         AND OLD.attempt_state = 'LEGACY'
                         AND NEW.attempt_state = 'LEGACY'
                         AND OLD.run_id IS NULL
                         AND NEW.run_id IS NOT NULL
                         AND NEW.error_code IS OLD.error_code
                     )
                 )
             )
             BEGIN
                 SELECT RAISE(
                     ABORT,
                     'validation attempt authority/result transition is invalid'
                 );
             END;

         CREATE TRIGGER IF NOT EXISTS engagement_validation_attempts_no_delete
             BEFORE DELETE ON engagement_validation_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'validation attempts are immutable');
             END;",
    )?;
    let mut validation_attempt_issues = Vec::new();
    crate::engagement::verify_validation_attempt_integrity(
        &transaction,
        &mut validation_attempt_issues,
    )?;
    if !validation_attempt_issues.is_empty() {
        return Err(KernelError::Reconciliation(format!(
            "validation-attempt authority migration replay failed: {}",
            validation_attempt_issues.join("; ")
        )));
    }
    transaction.pragma_update(None, "user_version", 13)?;
    transaction.commit()?;
    Ok(())
}

/// Add normalized mapping authority and immutable local remediation-pack
/// evidence. Data-bearing exported values are deliberately absent from every
/// table in this migration; only the verified archive contains them.
#[allow(clippy::too_many_lines)]
fn migrate_v13_to_v14(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "PRAGMA defer_foreign_keys = ON;
         CREATE TABLE engagement_mapping_authorities (
             authority_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(authority_id AS BLOB)) BETWEEN 1 AND 256
             ),
             authority_kind TEXT NOT NULL CHECK (
                 authority_kind IN ('OPERATOR_APPROVAL', 'CONTROLLED_PREPARATION')
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             mapping_id TEXT NOT NULL CHECK (
                 length(CAST(mapping_id AS BLOB)) BETWEEN 1 AND 128
             ),
             mapping_version TEXT NOT NULL CHECK (
                 length(CAST(mapping_version AS BLOB)) BETWEEN 1 AND 32
             ),
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             mapping_approval_id TEXT
                 REFERENCES engagement_mapping_approvals(approval_id) ON DELETE RESTRICT,
             mapping_approval_hash TEXT CHECK (
                 mapping_approval_hash IS NULL OR length(mapping_approval_hash) = 64
             ),
             preparation_id TEXT
                 REFERENCES engagement_mapping_preparations(preparation_id) ON DELETE RESTRICT,
             mapping_validation_attempt_id TEXT
                 REFERENCES mapping_validation_attempts(id) ON DELETE RESTRICT,
             canonical_authority_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_authority_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             authority_hash TEXT NOT NULL UNIQUE CHECK (length(authority_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (
                 engagement_id, job_id, source_hash,
                 mapping_id, mapping_version, mapping_hash
             ),
             UNIQUE (authority_id, authority_hash),
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(mapping_id, mapping_version, mapping_hash)
                 ON DELETE RESTRICT,
             CHECK (
                 (authority_kind = 'OPERATOR_APPROVAL'
                     AND mapping_approval_id IS NOT NULL
                     AND mapping_approval_hash IS NOT NULL
                     AND preparation_id IS NULL
                     AND mapping_validation_attempt_id IS NULL)
                 OR
                 (authority_kind = 'CONTROLLED_PREPARATION'
                     AND mapping_approval_id IS NULL
                     AND mapping_approval_hash IS NULL
                     AND preparation_id IS NOT NULL
                     AND mapping_validation_attempt_id IS NOT NULL)
             )
         );
         CREATE INDEX engagement_mapping_authorities_history
             ON engagement_mapping_authorities(
                 engagement_id, job_id, created_at, authority_id
             );
         CREATE TRIGGER engagement_mapping_authorities_exact_insert_guard
             BEFORE INSERT ON engagement_mapping_authorities
             WHEN NOT EXISTS (
                 SELECT 1 FROM jobs AS job
                 JOIN mapping_versions AS mapping
                   ON mapping.mapping_id = NEW.mapping_id
                  AND mapping.mapping_version = NEW.mapping_version
                  AND mapping.mapping_hash = NEW.mapping_hash
                 WHERE job.id = NEW.job_id
                   AND job.state = 'COMPLETE'
                   AND job.source_hash = NEW.source_hash
                   AND (
                     (NEW.authority_kind = 'OPERATOR_APPROVAL'
                       AND EXISTS (
                         SELECT 1 FROM engagement_mapping_approvals AS approval
                         WHERE approval.approval_id = NEW.mapping_approval_id
                           AND approval.approval_hash = NEW.mapping_approval_hash
                           AND approval.engagement_id = NEW.engagement_id
                           AND approval.mapping_id = NEW.mapping_id
                           AND approval.mapping_version = NEW.mapping_version
                           AND approval.mapping_hash = NEW.mapping_hash
                       ))
                     OR
                     (NEW.authority_kind = 'CONTROLLED_PREPARATION'
                       AND EXISTS (
                         SELECT 1
                         FROM engagement_mapping_preparations AS preparation
                         JOIN mapping_versions AS prepared_mapping
                           ON prepared_mapping.mapping_id = preparation.mapping_id
                          AND prepared_mapping.mapping_version = preparation.mapping_version
                          AND prepared_mapping.mapping_hash = preparation.mapping_hash
                         JOIN mapping_validation_attempts AS validation
                           ON validation.id = NEW.mapping_validation_attempt_id
                          AND validation.id = prepared_mapping.attempt_id
                          AND validation.state = 'VALID'
                          AND validation.mapping_id = NEW.mapping_id
                          AND validation.mapping_version = NEW.mapping_version
                          AND validation.mapping_hash = NEW.mapping_hash
                         WHERE preparation.preparation_id = NEW.preparation_id
                           AND preparation.engagement_id = NEW.engagement_id
                           AND preparation.mapping_id = NEW.mapping_id
                           AND preparation.mapping_version = NEW.mapping_version
                           AND preparation.mapping_hash = NEW.mapping_hash
                       ))
                   )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'mapping authority lacks exact supporting evidence');
             END;
         CREATE TRIGGER engagement_mapping_authorities_no_update
             BEFORE UPDATE ON engagement_mapping_authorities
             BEGIN SELECT RAISE(ABORT, 'mapping authorities are immutable'); END;
         CREATE TRIGGER engagement_mapping_authorities_no_delete
             BEFORE DELETE ON engagement_mapping_authorities
             BEGIN SELECT RAISE(ABORT, 'mapping authorities are immutable'); END;

         CREATE TABLE engagement_remediation_pack_attempts (
             attempt_id TEXT PRIMARY KEY NOT NULL CHECK (
                 length(CAST(attempt_id AS BLOB)) BETWEEN 1 AND 256
             ),
             planned_pack_id TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(planned_pack_id AS BLOB)) BETWEEN 1 AND 256
             ),
             engagement_id TEXT NOT NULL
                 REFERENCES engagement_records(engagement_id) ON DELETE RESTRICT,
             idempotency_key TEXT NOT NULL CHECK (
                 length(CAST(idempotency_key AS BLOB)) BETWEEN 16 AND 128
             ),
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 2097152
             ),
             expected_generation INTEGER NOT NULL CHECK (expected_generation >= 0),
             workspace_id TEXT NOT NULL,
             workspace_descriptor_hash TEXT NOT NULL CHECK (
                 length(workspace_descriptor_hash) = 64
             ),
             job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64),
             profiling_run_id TEXT NOT NULL
                 REFERENCES profiling_runs(id) ON DELETE RESTRICT,
             profiling_result_hash TEXT NOT NULL CHECK (
                 length(profiling_result_hash) = 64
             ),
             profile_id TEXT NOT NULL,
             profile_version TEXT NOT NULL,
             profile_hash TEXT NOT NULL CHECK (length(profile_hash) = 64),
             profile_approval_id TEXT NOT NULL
                 REFERENCES profile_approvals(approval_id) ON DELETE RESTRICT,
             profile_approval_hash TEXT NOT NULL CHECK (
                 length(profile_approval_hash) = 64
             ),
             mapping_id TEXT NOT NULL,
             mapping_version TEXT NOT NULL,
             mapping_hash TEXT NOT NULL CHECK (length(mapping_hash) = 64),
             mapping_authority_id TEXT NOT NULL,
             mapping_authority_hash TEXT NOT NULL CHECK (
                 length(mapping_authority_hash) = 64
             ),
             validation_run_id TEXT NOT NULL
                 REFERENCES validation_runs(id) ON DELETE RESTRICT,
             validation_result_hash TEXT NOT NULL CHECK (
                 length(validation_result_hash) = 64
             ),
             options_hash TEXT NOT NULL CHECK (length(options_hash) = 64),
             actor TEXT NOT NULL CHECK (
                 length(CAST(actor AS BLOB)) BETWEEN 1 AND 256
             ),
             reason TEXT NOT NULL CHECK (
                 length(CAST(reason AS BLOB)) BETWEEN 1 AND 4096
             ),
             local_export_acknowledgement TEXT NOT NULL CHECK (
                 local_export_acknowledgement =
                    'migration-stream/local-remediation-data-export/v1'
             ),
             include_offending_values INTEGER NOT NULL CHECK (
                 include_offending_values IN (0, 1)
             ),
             offending_values_acknowledgement TEXT CHECK (
                 offending_values_acknowledgement IS NULL
                 OR offending_values_acknowledgement =
                    'migration-stream/local-remediation-offending-values-export/v1'
             ),
             canonical_authority_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_authority_json AS BLOB)) BETWEEN 2 AND 2097152
             ),
             authority_hash TEXT NOT NULL UNIQUE CHECK (length(authority_hash) = 64),
             created_at TEXT NOT NULL CHECK (
                 length(CAST(created_at AS BLOB)) BETWEEN 1 AND 64
             ),
             UNIQUE (engagement_id, idempotency_key),
             UNIQUE (attempt_id, authority_hash),
             FOREIGN KEY (profile_id, profile_version, profile_hash)
                 REFERENCES profile_versions(profile_id, profile_version, profile_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (mapping_id, mapping_version, mapping_hash)
                 REFERENCES mapping_versions(mapping_id, mapping_version, mapping_hash)
                 ON DELETE RESTRICT,
             FOREIGN KEY (mapping_authority_id, mapping_authority_hash)
                 REFERENCES engagement_mapping_authorities(authority_id, authority_hash)
                 ON DELETE RESTRICT,
             CHECK (
                 (include_offending_values = 0
                    AND offending_values_acknowledgement IS NULL)
                 OR
                 (include_offending_values = 1
                    AND offending_values_acknowledgement =
                      'migration-stream/local-remediation-offending-values-export/v1')
             )
         );
         CREATE TRIGGER engagement_remediation_pack_attempts_no_update
             BEFORE UPDATE ON engagement_remediation_pack_attempts
             BEGIN SELECT RAISE(ABORT, 'remediation pack attempts are immutable'); END;
         CREATE TRIGGER engagement_remediation_pack_attempts_no_delete
             BEFORE DELETE ON engagement_remediation_pack_attempts
             BEGIN SELECT RAISE(ABORT, 'remediation pack attempts are immutable'); END;
         CREATE TRIGGER engagement_remediation_pack_attempts_exact_insert_guard
             BEFORE INSERT ON engagement_remediation_pack_attempts
             WHEN NOT EXISTS (
                 SELECT 1
                 FROM engagement_active_context AS active
                 JOIN jobs AS job ON job.id = NEW.job_id
                 JOIN profiling_runs AS profiling
                   ON profiling.id = NEW.profiling_run_id
                 JOIN profile_approvals AS profile_approval
                   ON profile_approval.approval_id = NEW.profile_approval_id
                 JOIN engagement_mapping_authorities AS mapping_authority
                   ON mapping_authority.authority_id = NEW.mapping_authority_id
                 JOIN validation_runs AS validation
                   ON validation.id = NEW.validation_run_id
                 WHERE active.engagement_id = NEW.engagement_id
                   AND active.generation = NEW.expected_generation
                   AND active.active_job_id = NEW.job_id
                   AND active.active_profiling_run_id = NEW.profiling_run_id
                   AND active.selected_profile_id = NEW.profile_id
                   AND active.selected_profile_version = NEW.profile_version
                   AND active.selected_profile_hash = NEW.profile_hash
                   AND active.active_mapping_id = NEW.mapping_id
                   AND active.active_mapping_version = NEW.mapping_version
                   AND active.active_mapping_hash = NEW.mapping_hash
                   AND active.active_validation_run_id = NEW.validation_run_id
                   AND active.validation_stage = 'COMPLETE'
                   AND job.state = 'COMPLETE' AND job.source_hash = NEW.source_hash
                   AND profiling.job_id = NEW.job_id
                   AND profiling.state = 'COMPLETE'
                   AND profiling.result_hash = NEW.profiling_result_hash
                   AND profile_approval.profile_id = NEW.profile_id
                   AND profile_approval.profile_version = NEW.profile_version
                   AND profile_approval.profile_hash = NEW.profile_hash
                   AND profile_approval.approval_hash = NEW.profile_approval_hash
                   AND mapping_authority.authority_hash = NEW.mapping_authority_hash
                   AND mapping_authority.engagement_id = NEW.engagement_id
                   AND mapping_authority.job_id = NEW.job_id
                   AND mapping_authority.source_hash = NEW.source_hash
                   AND mapping_authority.mapping_id = NEW.mapping_id
                   AND mapping_authority.mapping_version = NEW.mapping_version
                   AND mapping_authority.mapping_hash = NEW.mapping_hash
                   AND validation.state = 'COMPLETE'
                   AND validation.job_id = NEW.job_id
                   AND validation.source_hash = NEW.source_hash
                   AND validation.profile_id = NEW.profile_id
                   AND validation.profile_version = NEW.profile_version
                   AND validation.profile_hash = NEW.profile_hash
                   AND validation.mapping_id = NEW.mapping_id
                   AND validation.mapping_version = NEW.mapping_version
                   AND validation.mapping_hash = NEW.mapping_hash
                   AND validation.result_hash = NEW.validation_result_hash
                   AND EXISTS (
                     SELECT 1 FROM engagement_acceptance_attempts AS acceptance
                     JOIN engagement_source_preflights AS preflight
                       ON preflight.preflight_id = acceptance.preflight_id
                     WHERE acceptance.engagement_id = NEW.engagement_id
                       AND acceptance.job_id = NEW.job_id
                       AND acceptance.profiling_run_id = NEW.profiling_run_id
                       AND acceptance.accepted_source_hash = NEW.source_hash
                       AND acceptance.accepted_manifest_hash = NEW.manifest_hash
                       AND preflight.manifest_hash = NEW.manifest_hash
                   )
             )
             BEGIN SELECT RAISE(ABORT, 'remediation attempt lacks exact active authority'); END;

         CREATE TABLE engagement_remediation_pack_selections (
             attempt_id TEXT NOT NULL
                 REFERENCES engagement_remediation_pack_attempts(attempt_id)
                 ON DELETE RESTRICT,
             entity_id TEXT NOT NULL CHECK (
                 length(CAST(entity_id AS BLOB)) BETWEEN 1 AND 256
             ),
             source_file TEXT NOT NULL CHECK (
                 length(CAST(source_file AS BLOB)) BETWEEN 1 AND 1024
             ),
             source_column_ordinal INTEGER NOT NULL CHECK (
                 source_column_ordinal >= 0 AND source_column_ordinal < 4096
             ),
             source_column TEXT NOT NULL CHECK (
                 length(CAST(source_column AS BLOB)) BETWEEN 1 AND 1024
             ),
             canonical_selection_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_selection_json AS BLOB)) BETWEEN 2 AND 8192
             ),
             selection_hash TEXT NOT NULL CHECK (length(selection_hash) = 64),
             PRIMARY KEY (attempt_id, entity_id, source_file, source_column_ordinal),
             UNIQUE (attempt_id, source_file, source_column_ordinal),
             UNIQUE (attempt_id, selection_hash)
         );
         CREATE TRIGGER engagement_remediation_pack_selections_no_update
             BEFORE UPDATE ON engagement_remediation_pack_selections
             BEGIN SELECT RAISE(ABORT, 'remediation key selections are immutable'); END;
         CREATE TRIGGER engagement_remediation_pack_selections_no_delete
             BEFORE DELETE ON engagement_remediation_pack_selections
             BEGIN SELECT RAISE(ABORT, 'remediation key selections are immutable'); END;

         CREATE TABLE engagement_remediation_pack_results (
             attempt_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES engagement_remediation_pack_attempts(attempt_id)
                 ON DELETE RESTRICT,
             outcome TEXT NOT NULL CHECK (outcome IN ('COMPLETE', 'FAILED')),
             pack_id TEXT UNIQUE,
             result_hash TEXT NOT NULL UNIQUE CHECK (length(result_hash) = 64),
             error_code TEXT,
             completed_at TEXT NOT NULL CHECK (
                 length(CAST(completed_at AS BLOB)) BETWEEN 1 AND 64
             ),
             CHECK (
                 (outcome = 'COMPLETE' AND pack_id IS NOT NULL AND error_code IS NULL)
                 OR
                 (outcome = 'FAILED' AND pack_id IS NULL
                    AND length(CAST(error_code AS BLOB)) BETWEEN 1 AND 256)
             )
         );
         CREATE TRIGGER engagement_remediation_pack_results_no_update
             BEFORE UPDATE ON engagement_remediation_pack_results
             BEGIN SELECT RAISE(ABORT, 'remediation pack results are immutable'); END;
         CREATE TRIGGER engagement_remediation_pack_results_no_delete
             BEFORE DELETE ON engagement_remediation_pack_results
             BEGIN SELECT RAISE(ABORT, 'remediation pack results are immutable'); END;

         CREATE TABLE engagement_remediation_packs (
             pack_id TEXT PRIMARY KEY NOT NULL,
             attempt_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_remediation_pack_attempts(attempt_id)
                 ON DELETE RESTRICT,
             engagement_id TEXT NOT NULL,
             validation_run_id TEXT NOT NULL,
             source_hash TEXT NOT NULL CHECK (length(source_hash) = 64),
             selected_columns_json TEXT NOT NULL CHECK (
                 length(CAST(selected_columns_json AS BLOB)) BETWEEN 2 AND 1048576
             ),
             selected_columns_hash TEXT NOT NULL CHECK (
                 length(selected_columns_hash) = 64
             ),
             include_offending_values INTEGER NOT NULL CHECK (
                 include_offending_values IN (0, 1)
             ),
             actor TEXT NOT NULL,
             reason TEXT NOT NULL,
             issue_count INTEGER NOT NULL CHECK (issue_count >= 0),
             membership_hash TEXT NOT NULL CHECK (length(membership_hash) = 64),
             member_count INTEGER NOT NULL CHECK (member_count = 3),
             archive_relative_path TEXT NOT NULL UNIQUE CHECK (
                 length(CAST(archive_relative_path AS BLOB)) BETWEEN 1 AND 1024
             ),
             archive_size INTEGER NOT NULL CHECK (archive_size > 0),
             archive_sha256 TEXT NOT NULL UNIQUE CHECK (length(archive_sha256) = 64),
             manifest_hash TEXT NOT NULL CHECK (length(manifest_hash) = 64),
             canonical_record_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_record_json AS BLOB)) BETWEEN 2 AND 2097152
             ),
             record_hash TEXT NOT NULL UNIQUE CHECK (length(record_hash) = 64),
             completed_at TEXT NOT NULL,
             UNIQUE (pack_id, record_hash)
         );
         CREATE INDEX engagement_remediation_packs_history
             ON engagement_remediation_packs(engagement_id, completed_at, pack_id);
         CREATE TRIGGER engagement_remediation_packs_no_update
             BEFORE UPDATE ON engagement_remediation_packs
             BEGIN SELECT RAISE(ABORT, 'remediation packs are immutable'); END;
         CREATE TRIGGER engagement_remediation_packs_no_delete
             BEFORE DELETE ON engagement_remediation_packs
             BEGIN SELECT RAISE(ABORT, 'remediation packs are immutable'); END;
         CREATE TRIGGER engagement_remediation_packs_exact_insert_guard
             BEFORE INSERT ON engagement_remediation_packs
             WHEN NOT EXISTS (
                 SELECT 1 FROM engagement_remediation_pack_attempts AS attempt
                 WHERE attempt.attempt_id = NEW.attempt_id
                   AND attempt.planned_pack_id = NEW.pack_id
                   AND attempt.engagement_id = NEW.engagement_id
                   AND attempt.validation_run_id = NEW.validation_run_id
                   AND attempt.source_hash = NEW.source_hash
                   AND attempt.include_offending_values = NEW.include_offending_values
                   AND attempt.actor = NEW.actor AND attempt.reason = NEW.reason
             )
             BEGIN SELECT RAISE(ABORT, 'remediation pack differs from planned authority'); END;

         CREATE TABLE engagement_remediation_pack_members (
             pack_id TEXT NOT NULL
                 REFERENCES engagement_remediation_packs(pack_id) ON DELETE RESTRICT,
             filename TEXT NOT NULL CHECK (
                 filename IN ('manifest.json', 'remediation-issues.json', 'remediation-issues.csv')
             ),
             media_type TEXT NOT NULL,
             row_count INTEGER CHECK (row_count IS NULL OR row_count >= 0),
             byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
             sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
             PRIMARY KEY (pack_id, filename)
         );
         CREATE TRIGGER engagement_remediation_pack_members_no_update
             BEFORE UPDATE ON engagement_remediation_pack_members
             BEGIN SELECT RAISE(ABORT, 'remediation pack members are immutable'); END;
         CREATE TRIGGER engagement_remediation_pack_members_no_delete
             BEFORE DELETE ON engagement_remediation_pack_members
             BEGIN SELECT RAISE(ABORT, 'remediation pack members are immutable'); END;

         CREATE TABLE engagement_remediation_issue_members (
             pack_id TEXT NOT NULL
                 REFERENCES engagement_remediation_packs(pack_id) ON DELETE RESTRICT,
             issue_id TEXT NOT NULL,
             issue_hash TEXT NOT NULL CHECK (length(issue_hash) = 64),
             severity TEXT NOT NULL CHECK (
                 severity IN ('ERROR', 'WARNING', 'INFORMATION')
             ),
             rule_id TEXT NOT NULL,
             issue_code TEXT NOT NULL,
             display_message TEXT NOT NULL,
             entity_id TEXT NOT NULL,
             field_id TEXT NOT NULL,
             source_file TEXT NOT NULL,
             source_ordinal INTEGER NOT NULL CHECK (source_ordinal > 0),
             line_start INTEGER NOT NULL CHECK (line_start > 0),
             line_end INTEGER NOT NULL CHECK (line_end >= line_start),
             offending_lineage_kind TEXT NOT NULL CHECK (
                 offending_lineage_kind IN ('EXACT_SOURCE_COLUMN', 'NO_SINGLE_SOURCE_COLUMN')
             ),
             offending_source_column_ordinal INTEGER CHECK (
                 offending_source_column_ordinal IS NULL
                 OR offending_source_column_ordinal >= 0
             ),
             offending_source_column TEXT,
             canonical_member_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_member_json AS BLOB)) BETWEEN 2 AND 65536
             ),
             member_hash TEXT NOT NULL CHECK (length(member_hash) = 64),
             PRIMARY KEY (pack_id, issue_id),
             UNIQUE (pack_id, member_hash),
             CHECK (
                 (offending_lineage_kind = 'EXACT_SOURCE_COLUMN'
                    AND offending_source_column_ordinal IS NOT NULL
                    AND offending_source_column IS NOT NULL)
                 OR
                 (offending_lineage_kind = 'NO_SINGLE_SOURCE_COLUMN'
                    AND offending_source_column_ordinal IS NULL
                    AND offending_source_column IS NULL)
             )
         );
         CREATE INDEX engagement_remediation_issue_members_history
             ON engagement_remediation_issue_members(
                 pack_id, source_file, source_ordinal, issue_id
             );
         CREATE TRIGGER engagement_remediation_issue_members_no_update
             BEFORE UPDATE ON engagement_remediation_issue_members
             BEGIN SELECT RAISE(ABORT, 'remediation issue membership is immutable'); END;
         CREATE TRIGGER engagement_remediation_issue_members_no_delete
             BEFORE DELETE ON engagement_remediation_issue_members
             BEGIN SELECT RAISE(ABORT, 'remediation issue membership is immutable'); END;
         CREATE TRIGGER engagement_remediation_issue_members_exact_insert_guard
             BEFORE INSERT ON engagement_remediation_issue_members
             WHEN NOT EXISTS (
                 SELECT 1
                 FROM engagement_remediation_packs AS pack
                 JOIN engagement_remediation_pack_attempts AS attempt
                   ON attempt.attempt_id = pack.attempt_id
                 JOIN validation_issues AS issue
                   ON issue.run_id = attempt.validation_run_id
                  AND issue.issue_id = NEW.issue_id
                 WHERE pack.pack_id = NEW.pack_id
                   AND issue.issue_hash = NEW.issue_hash
                   AND issue.severity = NEW.severity
                   AND issue.rule_id = NEW.rule_id
                   AND issue.issue_code = NEW.issue_code
                   AND issue.display_message = NEW.display_message
                   AND issue.entity_id = NEW.entity_id
                   AND issue.field_id = NEW.field_id
                   AND issue.source_file = NEW.source_file
                   AND issue.source_ordinal = NEW.source_ordinal
                   AND issue.line_start = NEW.line_start
                   AND issue.line_end = NEW.line_end
             )
             BEGIN SELECT RAISE(ABORT, 'remediation issue membership differs from validation'); END;",
    )?;

    // Normalize every legacy tour's exact attributable mapping approval into
    // the new authority family before new writes begin using it.
    let mut legacy_authorities = {
        let mut statement = transaction.prepare(
            "SELECT tour.engagement_id, tour.job_id, tour.source_hash,
                    tour.mapping_id, tour.mapping_version, tour.mapping_hash,
                    tour.mapping_approval_id, tour.mapping_approval_hash,
                    approval.approved_at
             FROM engagement_tours AS tour
             JOIN engagement_mapping_approvals AS approval
               ON approval.approval_id = tour.mapping_approval_id
              AND approval.approval_hash = tour.mapping_approval_hash
              AND approval.engagement_id = tour.engagement_id
              AND approval.mapping_id = tour.mapping_id
              AND approval.mapping_version = tour.mapping_version
              AND approval.mapping_hash = tour.mapping_hash
             ORDER BY approval.approved_at, tour.tour_id",
        )?;
        statement
            .query_map([], |row| {
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
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let pending_reuse_authorities = {
        let mut statement = transaction.prepare(
            "SELECT reuse.engagement_id, reuse.candidate_job_id,
                    reuse.candidate_source_hash, reuse.mapping_id,
                    reuse.mapping_version, reuse.mapping_hash,
                    reuse.mapping_approval_id, reuse.mapping_approval_hash,
                    approval.approved_at
             FROM engagement_mapping_reuse_authorities AS reuse
             JOIN engagement_mapping_approvals AS approval
               ON approval.approval_id = reuse.mapping_approval_id
              AND approval.approval_hash = reuse.mapping_approval_hash
              AND approval.engagement_id = reuse.engagement_id
              AND approval.mapping_id = reuse.mapping_id
              AND approval.mapping_version = reuse.mapping_version
              AND approval.mapping_hash = reuse.mapping_hash
             ORDER BY approval.approved_at, reuse.source_reuse_id",
        )?;
        statement
            .query_map([], |row| {
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
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    legacy_authorities.extend(pending_reuse_authorities);
    let mut seen_authorities = std::collections::BTreeSet::new();
    for row in legacy_authorities {
        if !seen_authorities.insert((
            row.0.clone(),
            row.1.clone(),
            row.2.clone(),
            row.3.clone(),
            row.4.clone(),
            row.5.clone(),
        )) {
            continue;
        }
        let seed = serde_json::json!({
            "schemaVersion": "migration-stream/engagement/mapping-authority/v1",
            "authorityKind": "OPERATOR_APPROVAL",
            "engagementId": row.0,
            "jobId": row.1,
            "sourceHash": row.2,
            "mappingId": row.3,
            "mappingVersion": row.4,
            "mappingHash": row.5,
            "mappingApprovalId": row.6,
            "mappingApprovalHash": row.7,
            "preparationId": null,
            "mappingValidationAttemptId": null,
            "createdAt": row.8,
        });
        let seed_hash = crate::canonical::canonical_hash(&seed)?;
        let authority_id = format!("mapping-authority-{seed_hash}");
        let value = serde_json::json!({
            "schemaVersion": "migration-stream/engagement/mapping-authority/v1",
            "authorityId": authority_id,
            "authorityKind": "OPERATOR_APPROVAL",
            "engagementId": row.0,
            "jobId": row.1,
            "sourceHash": row.2,
            "mappingId": row.3,
            "mappingVersion": row.4,
            "mappingHash": row.5,
            "mappingApprovalId": row.6,
            "mappingApprovalHash": row.7,
            "preparationId": null,
            "mappingValidationAttemptId": null,
            "createdAt": row.8,
        });
        let canonical = crate::canonical::canonical_json_string(&value)?;
        let authority_hash = crate::canonical::canonical_hash(&value)?;
        transaction.execute(
            "INSERT OR IGNORE INTO engagement_mapping_authorities (
                 authority_id, authority_kind, engagement_id, job_id, source_hash,
                 mapping_id, mapping_version, mapping_hash,
                 mapping_approval_id, mapping_approval_hash,
                 preparation_id, mapping_validation_attempt_id,
                 canonical_authority_json, authority_hash, created_at
             ) VALUES (?1, 'OPERATOR_APPROVAL', ?2, ?3, ?4, ?5, ?6, ?7,
                       ?8, ?9, NULL, NULL, ?10, ?11, ?12)",
            params![
                authority_id,
                row.0,
                row.1,
                row.2,
                row.3,
                row.4,
                row.5,
                row.6,
                row.7,
                canonical,
                authority_hash,
                row.8,
            ],
        )?;
    }
    rebuild_mapping_authority_table_v14(&transaction, "engagement_mapping_reuse_authorities")?;
    rebuild_mapping_authority_table_v14(&transaction, "engagement_tours")?;
    transaction.execute_batch(
        "CREATE TRIGGER engagement_tours_mapping_authority_insert_guard
             BEFORE INSERT ON engagement_tours
             WHEN NEW.mapping_authority_id IS NULL
               OR NEW.mapping_authority_hash IS NULL
               OR NOT EXISTS (
                 SELECT 1 FROM engagement_mapping_authorities a
                 WHERE a.authority_id = NEW.mapping_authority_id
                   AND a.authority_hash = NEW.mapping_authority_hash
                   AND a.engagement_id = NEW.engagement_id
                   AND a.job_id = NEW.job_id
                   AND a.source_hash = NEW.source_hash
                   AND a.mapping_id = NEW.mapping_id
                   AND a.mapping_version = NEW.mapping_version
                   AND a.mapping_hash = NEW.mapping_hash
               )
             BEGIN SELECT RAISE(ABORT, 'tour mapping authority is absent or inconsistent'); END;
         CREATE TRIGGER engagement_mapping_reuse_authority_insert_guard
             BEFORE INSERT ON engagement_mapping_reuse_authorities
             WHEN NEW.mapping_authority_id IS NULL
               OR NEW.mapping_authority_hash IS NULL
               OR NOT EXISTS (
                 SELECT 1 FROM engagement_mapping_authorities a
                 WHERE a.authority_id = NEW.mapping_authority_id
                   AND a.authority_hash = NEW.mapping_authority_hash
                   AND a.engagement_id = NEW.engagement_id
                   AND a.job_id = NEW.candidate_job_id
                   AND a.source_hash = NEW.candidate_source_hash
                   AND a.mapping_id = NEW.mapping_id
                   AND a.mapping_version = NEW.mapping_version
                   AND a.mapping_hash = NEW.mapping_hash
               )
             BEGIN SELECT RAISE(ABORT, 'mapping reuse authority is absent or inconsistent'); END;
         PRAGMA user_version = 14;",
    )?;
    let mut foreign_keys = transaction.prepare("PRAGMA foreign_key_check")?;
    let first_violation = foreign_keys
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .next()
        .transpose()?;
    if let Some((table, rowid, parent, foreign_key)) = first_violation {
        return Err(KernelError::Reconciliation(format!(
            "v14 migration left a foreign-key violation in {table} row {rowid:?} against {parent} key {foreign_key}"
        )));
    }
    drop(foreign_keys);
    transaction.commit()?;
    Ok(())
}

/// Rebuild the two mutually-linked v11 tour tables without fabricating a
/// mapping approval for controlled mappings. The source table, all dependent
/// tables, and the replacement coexist in one deferred-FK transaction; any
/// mismatch rolls the whole v14 migration back to the original schema.
#[allow(clippy::too_many_lines)]
fn rebuild_mapping_authority_table_v14(transaction: &Transaction<'_>, table: &str) -> Result<()> {
    let (marker, authority_columns, authority_lookup) = match table {
        "engagement_tours" => (
            "validation_run_id TEXT NOT NULL UNIQUE",
            "mapping_authority_id TEXT REFERENCES engagement_mapping_authorities(authority_id) ON DELETE RESTRICT,
             mapping_authority_hash TEXT CHECK (
                 (mapping_authority_hash IS NULL OR length(mapping_authority_hash) = 64)
                 AND ((mapping_approval_id IS NULL) = (mapping_approval_hash IS NULL))
                 AND ((mapping_authority_id IS NULL) = (mapping_authority_hash IS NULL))
             ),",
            "authority.engagement_id = source.engagement_id
               AND authority.job_id = source.job_id
               AND authority.source_hash = source.source_hash
               AND authority.mapping_id = source.mapping_id
               AND authority.mapping_version = source.mapping_version
               AND authority.mapping_hash = source.mapping_hash",
        ),
        "engagement_mapping_reuse_authorities" => (
            "algorithm_version TEXT NOT NULL CHECK (",
            "mapping_authority_id TEXT REFERENCES engagement_mapping_authorities(authority_id) ON DELETE RESTRICT,
             mapping_authority_hash TEXT CHECK (
                 (mapping_authority_hash IS NULL OR length(mapping_authority_hash) = 64)
                 AND ((mapping_approval_id IS NULL) = (mapping_approval_hash IS NULL))
                 AND ((mapping_authority_id IS NULL) = (mapping_authority_hash IS NULL))
             ),",
            "authority.engagement_id = source.engagement_id
               AND authority.job_id = source.candidate_job_id
               AND authority.source_hash = source.candidate_source_hash
               AND authority.mapping_id = source.mapping_id
               AND authority.mapping_version = source.mapping_version
               AND authority.mapping_hash = source.mapping_hash",
        ),
        _ => {
            return Err(KernelError::Reconciliation(format!(
                "v14 mapping-authority rebuild does not support table {table:?}"
            )));
        }
    };
    let original_sql: String = transaction.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    let temporary = format!("{table}_v14_replacement");
    let create_prefix = format!("CREATE TABLE {table}");
    if !original_sql.starts_with(&create_prefix) {
        return Err(KernelError::Reconciliation(format!(
            "v14 mapping-authority rebuild found unexpected {table} CREATE statement"
        )));
    }
    let mut replacement_sql =
        original_sql.replacen(&create_prefix, &format!("CREATE TABLE {temporary}"), 1);
    let approval_id_before = replacement_sql.clone();
    replacement_sql = replacement_sql.replacen(
        "mapping_approval_id TEXT NOT NULL",
        "mapping_approval_id TEXT",
        1,
    );
    if replacement_sql == approval_id_before {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild could not make mapping approval ID nullable"
        )));
    }
    let approval_hash_before = replacement_sql.clone();
    replacement_sql = replacement_sql.replacen(
        "mapping_approval_hash TEXT NOT NULL CHECK (",
        "mapping_approval_hash TEXT CHECK (",
        1,
    );
    if replacement_sql == approval_hash_before {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild could not make mapping approval hash nullable"
        )));
    }
    let marker_before = replacement_sql.clone();
    replacement_sql = replacement_sql.replacen(
        marker,
        &format!("{authority_columns}\n             {marker}"),
        1,
    );
    if replacement_sql == marker_before {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild could not add normalized mapping authority columns"
        )));
    }
    // SQLite reparses every trigger in the schema while the replacement is
    // renamed. That includes the deliberately staged Object Workspace guard,
    // whose referenced final-binding table does not exist until establishment
    // completes. Temporarily remove every SQL-backed trigger, plus indexes
    // owned by this table, and restore their exact SQL before this transaction
    // can commit.
    let ancillary = {
        let mut statement = transaction.prepare(
            "SELECT type, name, sql FROM sqlite_schema
             WHERE sql IS NOT NULL
               AND (
                 (type = 'index' AND tbl_name = ?1)
                 OR
                 type = 'trigger'
               )
             ORDER BY CASE type WHEN 'index' THEN 0 ELSE 1 END, name",
        )?;
        statement
            .query_map([table], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    transaction.execute_batch(&replacement_sql)?;
    let columns = {
        let pragma = format!("PRAGMA table_info('{table}')");
        let mut statement = transaction.prepare(&pragma)?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if columns
        .iter()
        .any(|column| column == "mapping_authority_id")
        || columns
            .iter()
            .any(|column| column == "mapping_authority_hash")
    {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} source unexpectedly already has normalized authority columns"
        )));
    }
    let quoted_columns = columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let copy_sql = format!(
        "INSERT INTO {temporary} ({quoted_columns}, mapping_authority_id, mapping_authority_hash)
         SELECT {quoted_columns},
                (SELECT authority_id FROM engagement_mapping_authorities AS authority
                 WHERE {authority_lookup}),
                (SELECT authority_hash FROM engagement_mapping_authorities AS authority
                 WHERE {authority_lookup})
         FROM {table} AS source"
    );
    transaction.execute(&copy_sql, [])?;
    let old_count: i64 =
        transaction.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
    let new_count: i64 =
        transaction.query_row(&format!("SELECT COUNT(*) FROM {temporary}"), [], |row| {
            row.get(0)
        })?;
    if old_count != new_count {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild changed row count from {old_count} to {new_count}"
        )));
    }
    let missing_authorities: i64 = transaction.query_row(
        &format!(
            "SELECT COUNT(*) FROM {temporary}
             WHERE mapping_authority_id IS NULL OR mapping_authority_hash IS NULL"
        ),
        [],
        |row| row.get(0),
    )?;
    if missing_authorities != 0 {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild could not bind {missing_authorities} rows to exact mapping authority"
        )));
    }
    for (kind, name, _) in &ancillary {
        let quoted_name = format!("\"{}\"", name.replace('"', "\"\""));
        let drop_statement = match kind.as_str() {
            "index" => format!("DROP INDEX {quoted_name}"),
            "trigger" => format!("DROP TRIGGER {quoted_name}"),
            _ => {
                return Err(KernelError::Reconciliation(format!(
                    "v14 {table} rebuild found unsupported ancillary object {kind} {name}"
                )));
            }
        };
        transaction.execute_batch(&drop_statement)?;
    }
    transaction.execute_batch(&format!(
        "DROP TABLE {table}; ALTER TABLE {temporary} RENAME TO {table};"
    ))?;
    for (_, name, sql) in &ancillary {
        transaction.execute_batch(sql).map_err(|error| {
            KernelError::Reconciliation(format!(
                "v14 {table} rebuild could not restore {name}: {error}"
            ))
        })?;
    }
    let restored_triggers = {
        let mut statement = transaction.prepare(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'trigger' AND sql IS NOT NULL ORDER BY name",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let expected_triggers = ancillary
        .iter()
        .filter(|(kind, _, _)| kind == "trigger")
        .map(|(_, name, sql)| (name.clone(), sql.clone()))
        .collect::<Vec<_>>();
    if restored_triggers != expected_triggers {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild did not restore the exact trigger name/SQL set"
        )));
    }
    let restored_indexes = {
        let mut statement = transaction.prepare(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL ORDER BY name",
        )?;
        statement
            .query_map([table], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let expected_indexes = ancillary
        .iter()
        .filter(|(kind, _, _)| kind == "index")
        .map(|(_, name, sql)| (name.clone(), sql.clone()))
        .collect::<Vec<_>>();
    if restored_indexes != expected_indexes {
        return Err(KernelError::Reconciliation(format!(
            "v14 {table} rebuild did not restore the exact index name/SQL set"
        )));
    }
    Ok(())
}

pub(crate) fn open_read_only(database_path: &Path) -> Result<Connection> {
    let workspace = database_path.parent().ok_or_else(|| {
        KernelError::IntegrityFailed(format!(
            "read-only database path has no workspace parent: {}",
            database_path.display()
        ))
    })?;
    #[cfg(unix)]
    let workspace_metadata =
        fs::symlink_metadata(workspace).map_err(|error| KernelError::io(workspace, error))?;
    #[cfg(unix)]
    if !workspace_metadata.file_type().is_dir()
        || read_metadata_is_reparse_point(&workspace_metadata)
    {
        return Err(KernelError::IntegrityFailed(format!(
            "read-only workspace must be a real directory, not a link: {}",
            workspace.display()
        )));
    }
    #[cfg(unix)]
    let database_metadata = fs::symlink_metadata(database_path)
        .map_err(|error| KernelError::io(database_path, error))?;
    #[cfg(unix)]
    if !database_metadata.file_type().is_file()
        || read_metadata_is_reparse_point(&database_metadata)
    {
        return Err(KernelError::IntegrityFailed(format!(
            "read-only database must be a regular non-link file: {}",
            database_path.display()
        )));
    }
    #[cfg(windows)]
    let workspace_identity = retain_read_path_identity(workspace, true)?;
    #[cfg(windows)]
    let database_identity = retain_read_path_identity(database_path, false)?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| KernelError::io(workspace, error))?;
    let canonical_database =
        canonical_workspace.join(database_path.file_name().ok_or_else(|| {
            KernelError::IntegrityFailed("read-only database path has no filename".to_owned())
        })?);
    let connection = Connection::open_with_flags(
        &canonical_database,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let workspace_after_open = fs::symlink_metadata(&canonical_workspace)
        .map_err(|error| KernelError::io(&canonical_workspace, error))?;
    let database_after_open = fs::symlink_metadata(&canonical_database)
        .map_err(|error| KernelError::io(&canonical_database, error))?;
    #[cfg(unix)]
    let workspace_identity_matches =
        same_read_file_identity(&workspace_metadata, &workspace_after_open);
    #[cfg(windows)]
    let workspace_identity_matches =
        crate::durability::path_names_open_directory(&canonical_workspace, &workspace_identity)
            .map_err(|error| KernelError::io(&canonical_workspace, error))?;
    if !workspace_after_open.file_type().is_dir()
        || read_metadata_is_reparse_point(&workspace_after_open)
        || !workspace_identity_matches
    {
        return Err(KernelError::IntegrityFailed(format!(
            "read-only workspace identity changed while opening: {}",
            workspace.display()
        )));
    }
    #[cfg(unix)]
    let database_identity_matches =
        same_read_file_identity(&database_metadata, &database_after_open);
    #[cfg(windows)]
    let database_identity_matches =
        crate::durability::path_names_open_file(&canonical_database, &database_identity)
            .map_err(|error| KernelError::io(&canonical_database, error))?;
    if !database_after_open.file_type().is_file()
        || read_metadata_is_reparse_point(&database_after_open)
        || !database_identity_matches
    {
        return Err(KernelError::IntegrityFailed(format!(
            "read-only database identity changed while opening: {}",
            database_path.display()
        )));
    }
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}

#[cfg(unix)]
fn same_read_file_identity(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.dev() == after.dev() && before.ino() == after.ino()
}

#[cfg(windows)]
fn retain_read_path_identity(path: &Path, directory: bool) -> Result<File> {
    let file = if directory {
        crate::durability::open_directory_nofollow(path)
    } else {
        crate::durability::open_file_nofollow(path)
    }
    .map_err(|error| KernelError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    let name_matches = if directory {
        crate::durability::path_names_open_directory(path, &file)
    } else {
        crate::durability::path_names_open_file(path, &file)
    }
    .map_err(|error| KernelError::io(path, error))?;
    let expected_kind = if directory {
        metadata.file_type().is_dir()
    } else {
        metadata.file_type().is_file()
    };
    if !expected_kind || read_metadata_is_reparse_point(&metadata) || !name_matches {
        return Err(KernelError::IntegrityFailed(format!(
            "read-only path must retain one real non-link identity: {}",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(windows)]
fn read_metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    crate::durability::is_reparse_point(metadata)
}

#[cfg(not(windows))]
const fn read_metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn insert_event(
    transaction: &Transaction<'_>,
    job_id: &str,
    state: JobState,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO job_events (job_id, state, error_code, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![job_id, state.as_str(), error_code, detail, occurred_at],
    )?;
    Ok(())
}

fn insert_profiling_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO profiling_events (run_id, state, error_code, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, state, error_code, detail, occurred_at],
    )?;
    Ok(())
}

fn require_one_job(changed: usize, job_id: &str, action: &str) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(KernelError::Reconciliation(format!(
            "could not {action} for job {job_id}; changed {changed} rows"
        )))
    }
}

#[derive(Default)]
pub(crate) struct NamespaceCleanupReport {
    pub(crate) removed_live_partials: usize,
    pub(crate) quarantined_partials: usize,
    pub(crate) deleted_tombstones: usize,
    cleanup_pending: Vec<CleanupPendingInternal>,
}

impl NamespaceCleanupReport {
    pub(crate) fn ensure_no_pending(&self) -> Result<()> {
        if self.cleanup_pending.is_empty() {
            return Ok(());
        }
        Err(KernelError::Reconciliation(format!(
            "{} inert tombstone cleanup operation(s) remain pending: {}",
            self.cleanup_pending.len(),
            self.cleanup_pending
                .iter()
                .map(|pending| pending.diagnostic.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )))
    }
}

struct CleanupPendingInternal {
    cleanup_id: String,
    namespace: CleanupNamespace,
    attempts: u64,
    diagnostic: String,
}

impl CleanupPendingInternal {
    fn into_public(self) -> CleanupPending {
        CleanupPending {
            tombstone: format!(
                "{}/{}.{}",
                self.namespace.as_str(),
                self.cleanup_id,
                TOMBSTONE_EXTENSION
            ),
            cleanup_id: self.cleanup_id,
            namespace: self.namespace.as_str().to_owned(),
            attempts: self.attempts,
            diagnostic: self.diagnostic,
        }
    }
}

/// Recover only internally named partial artifacts in one controlled namespace.
pub(crate) fn recover_namespace_partials(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
) -> Result<NamespaceCleanupReport> {
    let directory = writer.root.join(namespace.as_str());
    match fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NamespaceCleanupReport::default());
        }
        Err(error) => return Err(KernelError::io(&directory, error)),
    }
    validate_controlled_namespace(&writer.root, &directory)?;
    recover_namespace_partials_platform(writer, namespace, &directory)
}

#[cfg(unix)]
fn recover_namespace_partials_platform(
    _writer: &WorkspaceWriter,
    _namespace: CleanupNamespace,
    directory: &Path,
) -> Result<NamespaceCleanupReport> {
    let mut report = NamespaceCleanupReport::default();
    for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(directory, error))?;
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str());
        if extension == Some(TOMBSTONE_EXTENSION) {
            return Err(KernelError::Reconciliation(format!(
                "Windows tombstone {} requires reconciliation on its originating platform",
                path.display()
            )));
        }
        if extension != Some("part") {
            continue;
        }
        validated_internal_id(&path, "part")?;
        let file_type = entry
            .file_type()
            .map_err(|error| KernelError::io(&path, error))?;
        if !file_type.is_file() {
            return Err(KernelError::Reconciliation(format!(
                "live partial {} is not a regular file",
                path.display()
            )));
        }
        fs::remove_file(&path).map_err(|error| KernelError::io(&path, error))?;
        report.removed_live_partials += 1;
    }
    File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| KernelError::io(directory, error))?;
    Ok(report)
}

#[cfg(windows)]
fn recover_namespace_partials_platform(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    directory: &Path,
) -> Result<NamespaceCleanupReport> {
    let (live_partials, tombstones) = discover_windows_cleanup_artifacts(directory)?;
    for attempt_id in live_partials.keys() {
        if load_cleanup_by_attempt(&writer.connection, namespace, attempt_id)?.is_none() {
            plan_cleanup(&writer.connection, namespace, attempt_id)?;
        }
    }
    let records = load_cleanup_records(&writer.connection, namespace)?;
    for cleanup_id in tombstones.keys() {
        if !records
            .iter()
            .any(|record| &record.cleanup_id == cleanup_id)
        {
            return Err(KernelError::Reconciliation(format!(
                "untracked inert tombstone {cleanup_id}.{} exists in {}",
                TOMBSTONE_EXTENSION,
                directory.display()
            )));
        }
    }

    let mut report = NamespaceCleanupReport::default();
    for record in records {
        reconcile_windows_cleanup_record(
            writer,
            namespace,
            directory,
            &record,
            live_partials.get(&record.attempt_id).map(PathBuf::as_path),
            tombstones.contains_key(&record.cleanup_id),
            &mut report,
        )?;
    }
    Ok(report)
}

#[cfg(not(any(unix, windows)))]
fn recover_namespace_partials_platform(
    _writer: &WorkspaceWriter,
    _namespace: CleanupNamespace,
    directory: &Path,
) -> Result<NamespaceCleanupReport> {
    Err(KernelError::io(
        directory,
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "partial recovery is not implemented for this platform",
        ),
    ))
}

fn validate_controlled_namespace(workspace: &Path, directory: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|error| KernelError::io(directory, error))?;
    if !metadata.file_type().is_dir() {
        return Err(KernelError::Reconciliation(format!(
            "controlled namespace {} is not a real directory",
            directory.display()
        )));
    }
    #[cfg(windows)]
    if crate::durability::is_reparse_point(&metadata) {
        return Err(KernelError::Reconciliation(format!(
            "controlled namespace {} is a reparse point",
            directory.display()
        )));
    }
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| KernelError::io(workspace, error))?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| KernelError::io(directory, error))?;
    if canonical_directory.parent() != Some(canonical_workspace.as_path()) {
        return Err(KernelError::Reconciliation(format!(
            "controlled namespace {} resolves outside its workspace root",
            directory.display()
        )));
    }
    Ok(())
}

fn validated_internal_id(path: &Path, expected_extension: &str) -> Result<String> {
    if path.extension().and_then(|value| value.to_str()) != Some(expected_extension) {
        return Err(KernelError::Reconciliation(format!(
            "controlled artifact {} has the wrong extension",
            path.display()
        )));
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            KernelError::Reconciliation(format!(
                "controlled artifact {} has a non-Unicode internal identifier",
                path.display()
            ))
        })?;
    if !is_canonical_uuid(stem) {
        return Err(KernelError::Reconciliation(format!(
            "controlled artifact {} does not use a canonical internal UUID",
            path.display()
        )));
    }
    Ok(stem.to_owned())
}

fn is_canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|parsed| parsed.to_string() == value)
}

#[cfg(windows)]
fn noncanonical_cleanup_id_error(column: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(KernelError::Reconciliation(format!(
            "partial cleanup contains noncanonical internal UUID {value:?}"
        ))),
    )
}

#[cfg(windows)]
fn canonical_cleanup_id(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<String> {
    let value: String = row.get(column)?;
    if !is_canonical_uuid(&value) {
        return Err(noncanonical_cleanup_id_error(column, &value));
    }
    Ok(value)
}

/*
 * Keep the Windows cleanup state private. It is persistence evidence for
 * recovery only and cannot confer completion or authority.
 */
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialCleanupState {
    Planned,
    Quarantined,
    CleanupPending,
    Cleaned,
    Conflict,
}

#[cfg(windows)]
impl PartialCleanupState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "PLANNED" => Ok(Self::Planned),
            "QUARANTINED" => Ok(Self::Quarantined),
            "CLEANUP_PENDING" => Ok(Self::CleanupPending),
            "CLEANED" => Ok(Self::Cleaned),
            "CONFLICT" => Ok(Self::Conflict),
            other => Err(KernelError::Reconciliation(format!(
                "partial cleanup has unknown state {other:?}"
            ))),
        }
    }
}

#[cfg(windows)]
struct PartialCleanupRecord {
    cleanup_id: String,
    attempt_id: String,
    state: PartialCleanupState,
    cleanup_attempts: u64,
}

#[cfg(windows)]
fn discover_windows_cleanup_artifacts(
    directory: &Path,
) -> Result<(BTreeMap<String, PathBuf>, BTreeMap<String, PathBuf>)> {
    let mut live_partials = BTreeMap::new();
    let mut tombstones = BTreeMap::new();
    for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(directory, error))?;
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str());
        let Some(extension) = extension else {
            continue;
        };
        let canonical_extension = if extension.eq_ignore_ascii_case("part") {
            "part"
        } else if extension.eq_ignore_ascii_case(TOMBSTONE_EXTENSION) {
            TOMBSTONE_EXTENSION
        } else {
            continue;
        };
        if extension != canonical_extension {
            return Err(KernelError::Reconciliation(format!(
                "controlled cleanup artifact {} does not use the canonical lowercase extension",
                path.display()
            )));
        }
        let file_type = entry
            .file_type()
            .map_err(|error| KernelError::io(&path, error))?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if !file_type.is_file()
            || !metadata.file_type().is_file()
            || crate::durability::is_reparse_point(&metadata)
        {
            return Err(KernelError::Reconciliation(format!(
                "controlled cleanup artifact {} is not a regular non-reparse file",
                path.display()
            )));
        }
        let id = validated_internal_id(&path, canonical_extension)?;
        if canonical_extension == "part" {
            live_partials.insert(id, path);
        } else {
            tombstones.insert(id, path);
        }
    }
    Ok((live_partials, tombstones))
}

#[cfg(windows)]
fn plan_cleanup(
    connection: &Connection,
    namespace: CleanupNamespace,
    attempt_id: &str,
) -> Result<PartialCleanupRecord> {
    if !is_canonical_uuid(attempt_id) {
        return Err(KernelError::Reconciliation(
            "cleanup attempt ID is not a canonical internal UUID".to_owned(),
        ));
    }
    if namespace == CleanupNamespace::Packages {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ?1)",
            [attempt_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(KernelError::Reconciliation(format!(
                "live package partial {attempt_id}.part has no durable job"
            )));
        }
    }
    let cleanup_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    connection.execute(
        "INSERT INTO partial_cleanups (
             cleanup_id, namespace, attempt_id, job_id, state,
             cleanup_attempts, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 'PLANNED', 0, ?5, ?5)",
        params![
            cleanup_id,
            namespace.as_str(),
            attempt_id,
            namespace.job_id(attempt_id),
            now,
        ],
    )?;
    Ok(PartialCleanupRecord {
        cleanup_id,
        attempt_id: attempt_id.to_owned(),
        state: PartialCleanupState::Planned,
        cleanup_attempts: 0,
    })
}

#[cfg(windows)]
fn load_cleanup_by_attempt(
    connection: &Connection,
    namespace: CleanupNamespace,
    attempt_id: &str,
) -> Result<Option<PartialCleanupRecord>> {
    connection
        .query_row(
            "SELECT cleanup_id, attempt_id, state, cleanup_attempts
             FROM partial_cleanups WHERE namespace = ?1 AND attempt_id = ?2",
            params![namespace.as_str(), attempt_id],
            cleanup_record_from_row,
        )
        .optional()
        .map_err(Into::into)
}

#[cfg(windows)]
fn load_cleanup_records(
    connection: &Connection,
    namespace: CleanupNamespace,
) -> Result<Vec<PartialCleanupRecord>> {
    let mut statement = connection.prepare(
        "SELECT cleanup_id, attempt_id, state, cleanup_attempts
         FROM partial_cleanups WHERE namespace = ?1 ORDER BY cleanup_id",
    )?;
    let rows = statement.query_map([namespace.as_str()], cleanup_record_from_row)?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

#[cfg(windows)]
fn cleanup_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PartialCleanupRecord> {
    let state: String = row.get(2)?;
    let state = PartialCleanupState::parse(&state).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let attempts: i64 = row.get(3)?;
    let cleanup_attempts = u64::try_from(attempts).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    Ok(PartialCleanupRecord {
        cleanup_id: canonical_cleanup_id(row, 0)?,
        attempt_id: canonical_cleanup_id(row, 1)?,
        state,
        cleanup_attempts,
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn reconcile_windows_cleanup_record(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    directory: &Path,
    record: &PartialCleanupRecord,
    live: Option<&Path>,
    has_tombstone: bool,
    report: &mut NamespaceCleanupReport,
) -> Result<()> {
    let tombstone = directory.join(format!("{}.{TOMBSTONE_EXTENSION}", record.cleanup_id));
    let has_live = live.is_some();
    if cleanup_record_is_already_complete_or_conflicted(
        writer,
        namespace,
        record,
        has_live,
        has_tombstone,
    )? {
        return Ok(());
    }

    if record.state == PartialCleanupState::Planned && has_live {
        quarantine_windows_live_partial(
            writer,
            directory,
            record,
            live.expect("PLANNED live artifact was checked"),
            &tombstone,
        )?;
        report.removed_live_partials += 1;
        report.quarantined_partials += 1;
    } else if record.state == PartialCleanupState::Planned && has_tombstone {
        if let Err(error) = validate_windows_tombstone(&tombstone) {
            let detail = format!("pre-existing tombstone validation failed: {error}");
            return cleanup_conflict(&writer.connection, record, &detail);
        }
        mark_cleanup_quarantined(&writer.connection, &record.cleanup_id)?;
    } else if matches!(
        record.state,
        PartialCleanupState::Quarantined | PartialCleanupState::CleanupPending
    ) && !has_tombstone
    {
        mark_cleanup_cleaned(&writer.connection, &record.cleanup_id, false)?;
        return Ok(());
    }

    match delete_windows_tombstone(&tombstone) {
        Ok(()) => {
            mark_cleanup_cleaned(&writer.connection, &record.cleanup_id, true)?;
            report.deleted_tombstones += 1;
        }
        Err(error) => {
            let attempts = record.cleanup_attempts.saturating_add(1);
            let diagnostic = format!(
                "cleanup {} retained inert tombstone after deletion failure: {error}",
                record.cleanup_id
            );
            mark_cleanup_pending(&writer.connection, &record.cleanup_id, &error, &diagnostic)?;
            report.cleanup_pending.push(CleanupPendingInternal {
                cleanup_id: record.cleanup_id.clone(),
                namespace,
                attempts,
                diagnostic,
            });
        }
    }
    Ok(())
}

#[cfg(windows)]
fn cleanup_record_is_already_complete_or_conflicted(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    record: &PartialCleanupRecord,
    has_live: bool,
    has_tombstone: bool,
) -> Result<bool> {
    if namespace == CleanupNamespace::Packages {
        let job_exists: bool = writer.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ?1)",
            [&record.attempt_id],
            |row| row.get(0),
        )?;
        if !job_exists {
            return cleanup_conflict(
                &writer.connection,
                record,
                "package cleanup no longer has its bound durable job",
            );
        }
    }
    match record.state {
        PartialCleanupState::Conflict => {
            return Err(KernelError::Reconciliation(format!(
                "partial cleanup {} is in explicit CONFLICT state",
                record.cleanup_id
            )));
        }
        PartialCleanupState::Cleaned => {
            if has_live || has_tombstone {
                return cleanup_conflict(
                    &writer.connection,
                    record,
                    "a CLEANED cleanup artifact reappeared",
                );
            }
            return Ok(true);
        }
        PartialCleanupState::Planned if has_live && has_tombstone => {
            return cleanup_conflict(
                &writer.connection,
                record,
                "live partial and planned tombstone exist simultaneously",
            );
        }
        PartialCleanupState::Planned if !has_live && !has_tombstone => {
            return cleanup_conflict(
                &writer.connection,
                record,
                "PLANNED cleanup has neither its live partial nor tombstone",
            );
        }
        PartialCleanupState::Quarantined | PartialCleanupState::CleanupPending if has_live => {
            return cleanup_conflict(
                &writer.connection,
                record,
                "live partial exists after quarantine",
            );
        }
        _ => {}
    }
    Ok(false)
}

#[cfg(windows)]
fn quarantine_windows_live_partial(
    writer: &WorkspaceWriter,
    directory: &Path,
    record: &PartialCleanupRecord,
    live: &Path,
    tombstone: &Path,
) -> Result<()> {
    validate_controlled_namespace(&writer.root, directory)?;
    match fs::symlink_metadata(tombstone) {
        Ok(_) => {
            return cleanup_conflict(
                &writer.connection,
                record,
                "tombstone destination collision",
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(KernelError::io(tombstone, error)),
    }
    if let Err(error) = crate::durability::quarantine_write_through(live, tombstone) {
        match fs::symlink_metadata(tombstone) {
            Ok(_) => {
                return cleanup_conflict(
                    &writer.connection,
                    record,
                    "write-through move reported an error and destination now exists",
                );
            }
            Err(inspect_error) if inspect_error.kind() == std::io::ErrorKind::NotFound => {
                return Err(KernelError::io(live, error));
            }
            Err(inspect_error) => {
                let detail = format!(
                    "write-through move failed and tombstone presence could not be determined: {inspect_error}"
                );
                return cleanup_conflict(&writer.connection, record, &detail);
            }
        }
    }
    if let Err(error) = validate_controlled_namespace(&writer.root, directory) {
        let detail = format!(
            "controlled namespace validation failed after write-through quarantine: {error}"
        );
        return cleanup_conflict(&writer.connection, record, &detail);
    }
    match fs::symlink_metadata(live) {
        Ok(_) => {
            return cleanup_conflict(
                &writer.connection,
                record,
                "live partial remained after successful write-through quarantine",
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            let detail = format!(
                "live partial absence could not be proven after write-through quarantine: {error}"
            );
            return cleanup_conflict(&writer.connection, record, &detail);
        }
    }
    if let Err(error) = validate_windows_tombstone(tombstone) {
        let detail = format!("post-quarantine tombstone validation failed: {error}");
        return cleanup_conflict(&writer.connection, record, &detail);
    }
    mark_cleanup_quarantined(&writer.connection, &record.cleanup_id)?;
    Ok(())
}

#[cfg(windows)]
fn validate_windows_tombstone(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() || crate::durability::is_reparse_point(&metadata) {
        return Err(KernelError::Reconciliation(format!(
            "inert tombstone {} is not a regular non-reparse file",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn mark_cleanup_quarantined(connection: &Connection, cleanup_id: &str) -> Result<()> {
    let now = now_rfc3339();
    connection.execute(
        "UPDATE partial_cleanups
         SET state = 'QUARANTINED', quarantined_at = COALESCE(quarantined_at, ?1),
             updated_at = ?1, last_error_code = NULL, last_error_message = NULL
         WHERE cleanup_id = ?2 AND state IN ('PLANNED', 'QUARANTINED', 'CLEANUP_PENDING')",
        params![now, cleanup_id],
    )?;
    Ok(())
}

#[cfg(windows)]
fn mark_cleanup_cleaned(
    connection: &Connection,
    cleanup_id: &str,
    attempted_delete: bool,
) -> Result<()> {
    let now = now_rfc3339();
    connection.execute(
        "UPDATE partial_cleanups
         SET state = 'CLEANED', cleaned_at = ?1, updated_at = ?1,
             cleanup_attempts = cleanup_attempts + ?2,
             last_error_code = NULL, last_error_message = NULL
         WHERE cleanup_id = ?3 AND state IN ('QUARANTINED', 'CLEANUP_PENDING')",
        params![now, i64::from(attempted_delete), cleanup_id],
    )?;
    Ok(())
}

#[cfg(windows)]
fn mark_cleanup_pending(
    connection: &Connection,
    cleanup_id: &str,
    error: &std::io::Error,
    diagnostic: &str,
) -> Result<()> {
    let now = now_rfc3339();
    connection.execute(
        "UPDATE partial_cleanups
         SET state = 'CLEANUP_PENDING', cleanup_attempts = cleanup_attempts + 1,
             updated_at = ?1, last_error_code = ?2, last_error_message = ?3
         WHERE cleanup_id = ?4 AND state IN ('QUARANTINED', 'CLEANUP_PENDING')",
        params![now, format!("{:?}", error.kind()), diagnostic, cleanup_id],
    )?;
    Ok(())
}

#[cfg(windows)]
fn cleanup_conflict<T>(
    connection: &Connection,
    record: &PartialCleanupRecord,
    detail: &str,
) -> Result<T> {
    let now = now_rfc3339();
    connection.execute(
        "UPDATE partial_cleanups
         SET state = 'CONFLICT', updated_at = ?1,
             last_error_code = 'RECOVERY_CONFLICT', last_error_message = ?2
         WHERE cleanup_id = ?3",
        params![now, detail, record.cleanup_id],
    )?;
    Err(KernelError::Reconciliation(format!(
        "partial cleanup {} conflict: {detail}",
        record.cleanup_id
    )))
}

#[cfg(windows)]
fn delete_windows_tombstone(path: &Path) -> std::io::Result<()> {
    if tombstone_delete_failure_injected() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected tombstone deletion failure",
        ));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() || crate::durability::is_reparse_point(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "tombstone is not a regular non-reparse file",
        ));
    }
    if metadata.permissions().readonly() {
        let mut permissions = metadata.permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "this Windows-only path clears the read-only attribute on an inert owned tombstone"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn tombstone_delete_failure_injected() -> bool {
    #[cfg(test)]
    {
        TOMBSTONE_DELETE_FAILURE.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[cfg(all(test, windows))]
thread_local! {
    static TOMBSTONE_DELETE_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, windows))]
fn inject_tombstone_delete_failure(enabled: bool) {
    TOMBSTONE_DELETE_FAILURE.with(|injected| injected.set(enabled));
}

fn empty_status(workspace: &Path) -> WorkspaceStatus {
    WorkspaceStatus {
        workspace: workspace.to_path_buf(),
        active_jobs: 0,
        complete_jobs: 0,
        failed_jobs: 0,
        interrupted_jobs: 0,
        jobs: Vec::new(),
    }
}

fn nonnegative(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        KernelError::Reconciliation(format!("database contains negative {label}: {value}"))
    })
}

fn optional_nonnegative(value: Option<i64>, label: &str) -> Result<Option<u64>> {
    value.map(|value| nonnegative(value, label)).transpose()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::fs::OpenOptions;

    #[test]
    fn workspace_uses_required_sqlite_durability_settings() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let journal: String = writer
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode");
        let synchronous: i64 = writer
            .connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .expect("synchronous");
        let foreign_keys: i64 = writer
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign keys");
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        let redundant_index: Option<String> = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND name = 'source_records_job'",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("query redundant index");
        let unique_index: String = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND tbl_name = 'source_records'
                   AND sql IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("automatic uniqueness index");
        assert_eq!(redundant_index, None);
        assert_eq!(unique_index, "sqlite_autoindex_source_records_1");
    }

    #[test]
    fn advisory_lock_classification_preserves_writer_exclusion() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let first = WorkspaceWriter::open(temporary.path()).expect("first writer");
        let second = WorkspaceWriter::open(temporary.path());
        match second {
            Err(error) => {
                match &error {
                    KernelError::WorkspaceLocked(path) => assert_eq!(
                        path,
                        &fs::canonicalize(temporary.path())
                            .expect("canonical temporary workspace path")
                    ),
                    other => panic!("unexpected lock error: {other}"),
                }
                assert_eq!(error.code(), "WORKSPACE_LOCKED");
            }
            Ok(_) => panic!("second writer acquired an excluded lock"),
        }
        let one: i64 = first
            .connection
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("first writer remains usable");
        assert_eq!(one, 1);
        drop(first);
        let _third =
            WorkspaceWriter::open(temporary.path()).expect("lock released for next writer");
    }

    #[test]
    fn recovery_marks_nonterminal_jobs_and_removes_parts() {
        const JOB_ID: &str = "11111111-1111-4111-8111-111111111111";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let partial = {
            let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
            create_job(
                &writer.connection,
                &NewJob {
                    id: JOB_ID,
                    source_name: "source.zip",
                    source_path: "/source.zip",
                    source_size: Some(10),
                    policy_json: "{}",
                },
            )
            .expect("create job");
            let package_dir = temporary.path().join(PACKAGES_DIRECTORY);
            fs::create_dir_all(&package_dir).expect("packages");
            let partial = package_dir.join(format!("{JOB_ID}.part"));
            File::create(&partial).expect("part");
            partial
        };

        let report = recover_workspace(temporary.path()).expect("recover");
        assert_eq!(report.recovered_jobs, 1);
        assert_eq!(report.removed_partial_files, 1);
        assert_eq!(report.cleanup_pending, [] as [CleanupPending; 0]);
        assert!(!partial.exists());
        #[cfg(windows)]
        {
            assert_eq!(report.quarantined_partial_files, 1);
            assert_eq!(report.deleted_tombstones, 1);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(report.quarantined_partial_files, 0);
            assert_eq!(report.deleted_tombstones, 0);
        }
        let repeated = recover_workspace(temporary.path()).expect("repeat recovery");
        assert_eq!(repeated.recovered_jobs, 0);
        assert_eq!(repeated.removed_partial_files, 0);
        assert_eq!(repeated.quarantined_partial_files, 0);
        assert_eq!(repeated.deleted_tombstones, 0);
        assert_eq!(repeated.cleanup_pending, [] as [CleanupPending; 0]);
        let status = workspace_status(temporary.path()).expect("status");
        assert_eq!(status.jobs[0].state, JobState::Interrupted);
        assert_eq!(
            status.jobs[0].error_code.as_deref(),
            Some("INTERRUPTED_RECOVERED")
        );
        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("open recovered workspace");
        let recovered_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM job_events
                 WHERE job_id = ?1 AND state = 'INTERRUPTED'
                   AND error_code = 'INTERRUPTED_RECOVERED'",
                [JOB_ID],
                |row| row.get(0),
            )
            .expect("count recovery events");
        assert_eq!(recovered_events, 1);
        let integrity = integrity_check(temporary.path()).expect("integrity replay");
        assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
        #[cfg(windows)]
        assert_eq!(integrity.checked_partial_cleanups, 1);
        #[cfg(not(windows))]
        assert_eq!(integrity.checked_partial_cleanups, 0);
    }

    #[test]
    fn recovery_rejects_an_unsafe_partial_identifier_without_deriving_a_tombstone() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let packages = temporary.path().join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages");
        let unsafe_partial = packages.join("untrusted-upload-name.part");
        fs::write(&unsafe_partial, b"partial").expect("unsafe partial");
        drop(writer);

        let error = recover_workspace(temporary.path()).expect_err("unsafe name must fail closed");
        assert!(matches!(error, KernelError::Reconciliation(_)));
        assert_eq!(
            fs::read(&unsafe_partial).expect("unsafe partial remains"),
            b"partial"
        );
        assert_eq!(
            fs::read_dir(&packages)
                .expect("packages")
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str())
                        == Some(TOMBSTONE_EXTENSION)
                })
                .count(),
            0
        );
    }

    #[test]
    fn integrity_replay_detects_an_untracked_tombstone() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let packages = temporary.path().join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages");
        fs::write(
            packages.join("22222222-2222-4222-8222-222222222222.tombstone"),
            b"inert but untracked",
        )
        .expect("untracked tombstone");
        drop(writer);

        let integrity = integrity_check(temporary.path()).expect("integrity replay");
        assert!(!integrity.ok);
        assert!(
            integrity
                .issues
                .iter()
                .any(|issue| issue.contains("untracked tombstone artifact"))
        );
    }

    #[cfg(windows)]
    fn create_windows_job(writer: &WorkspaceWriter, job_id: &str) {
        create_job(
            &writer.connection,
            &NewJob {
                id: job_id,
                source_name: "source.zip",
                source_path: "C:\\source.zip",
                source_size: Some(10),
                policy_json: "{}",
            },
        )
        .expect("create Windows recovery job");
    }

    #[cfg(windows)]
    fn create_windows_live_partial(
        writer: &WorkspaceWriter,
        job_id: &str,
        payload: &[u8],
    ) -> PathBuf {
        create_windows_job(writer, job_id);
        let packages = writer.root.join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages");
        let partial = packages.join(format!("{job_id}.part"));
        fs::write(&partial, payload).expect("live partial");
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&partial)
            .and_then(|file| file.sync_all())
            .expect("synchronize live partial");
        partial
    }

    #[cfg(windows)]
    fn recovered_job_event_count(connection: &Connection, job_id: &str) -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM job_events
                 WHERE job_id = ?1 AND state = 'INTERRUPTED'
                   AND error_code = 'INTERRUPTED_RECOVERED'",
                [job_id],
                |row| row.get(0),
            )
            .expect("count recovered job events")
    }

    #[cfg(windows)]
    struct TombstoneDeleteFailureGuard;

    #[cfg(windows)]
    impl TombstoneDeleteFailureGuard {
        fn enable() -> Self {
            inject_tombstone_delete_failure(true);
            Self
        }
    }

    #[cfg(windows)]
    impl Drop for TombstoneDeleteFailureGuard {
        fn drop(&mut self) {
            inject_tombstone_delete_failure(false);
        }
    }

    #[cfg(windows)]
    fn assert_windows_pending_cleanup_warnings(report: &IntegrityReport) {
        assert_eq!(
            report.warnings.len(),
            2,
            "one legacy-adoption warning and one cleanup warning are expected: {:?}",
            report.warnings
        );
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| {
                    warning.as_str()
                        == "legacy workspace has no object workspace descriptor and requires explicit adoption"
                })
                .count(),
            1
        );
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| {
                    warning.starts_with("partial cleanup ")
                        && warning
                            .contains("retains an inert tombstone after 1 deletion attempt(s)")
                        && warning.contains("injected tombstone deletion failure")
                })
                .count(),
            1
        );
    }

    #[cfg(windows)]
    fn assert_windows_resolved_cleanup_warnings(report: &IntegrityReport) {
        assert_eq!(
            report.warnings.len(),
            1,
            "only the independent legacy-adoption warning is expected after cleanup: {:?}",
            report.warnings
        );
        assert_eq!(
            report.warnings[0],
            "legacy workspace has no object workspace descriptor and requires explicit adoption"
        );
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.starts_with("partial cleanup "))
                .count(),
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_recovery_reconciles_a_crash_after_write_through_quarantine() {
        const JOB_ID: &str = "33333333-3333-4333-8333-333333333333";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let live = create_windows_live_partial(&writer, JOB_ID, b"interrupted partial");
        let cleanup = plan_cleanup(&writer.connection, CleanupNamespace::Packages, JOB_ID)
            .expect("persist cleanup plan");
        let tombstone = live
            .parent()
            .expect("live parent")
            .join(format!("{}.tombstone", cleanup.cleanup_id));
        crate::durability::quarantine_write_through(&live, &tombstone)
            .expect("write-through quarantine");
        assert!(!live.exists());
        assert!(tombstone.exists());
        drop(writer);

        let report = recover_workspace(temporary.path()).expect("recover post-move crash");
        assert_eq!(report.recovered_jobs, 1);
        assert_eq!(report.removed_partial_files, 0);
        assert_eq!(report.quarantined_partial_files, 0);
        assert_eq!(report.deleted_tombstones, 1);
        assert_eq!(report.cleanup_pending, [] as [CleanupPending; 0]);
        assert!(!live.exists());
        assert!(!tombstone.exists());

        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("open recovered workspace");
        let cleanup_state: (String, i64) = connection
            .query_row(
                "SELECT state, cleanup_attempts FROM partial_cleanups
                 WHERE cleanup_id = ?1",
                [&cleanup.cleanup_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("cleanup state");
        assert_eq!(cleanup_state, ("CLEANED".to_owned(), 1));
        let complete_jobs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
                [],
                |row| row.get(0),
            )
            .expect("complete jobs");
        assert_eq!(complete_jobs, 0);
        assert_eq!(recovered_job_event_count(&connection, JOB_ID), 1);
        let integrity = integrity_check(temporary.path()).expect("integrity replay");
        assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    }

    #[cfg(windows)]
    #[test]
    fn windows_tombstone_deletion_failure_is_a_warning_and_retry_is_idempotent() {
        const JOB_ID: &str = "44444444-4444-4444-8444-444444444444";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let live = create_windows_live_partial(&writer, JOB_ID, b"interrupted partial");
        drop(writer);

        let failure_guard = TombstoneDeleteFailureGuard::enable();
        let first = recover_workspace(temporary.path()).expect("recover with deferred deletion");
        assert_eq!(first.recovered_jobs, 1);
        assert_eq!(first.removed_partial_files, 1);
        assert_eq!(first.quarantined_partial_files, 1);
        assert_eq!(first.deleted_tombstones, 0);
        assert_eq!(first.cleanup_pending.len(), 1);
        assert_eq!(first.cleanup_pending[0].namespace, "packages");
        assert_eq!(first.cleanup_pending[0].attempts, 1);
        assert!(!live.exists());
        let tombstone = temporary.path().join(&first.cleanup_pending[0].tombstone);
        assert!(tombstone.exists());

        let pending_integrity =
            integrity_check(temporary.path()).expect("pending integrity replay");
        assert!(
            pending_integrity.ok,
            "pending integrity issues: {:?}",
            pending_integrity.issues
        );
        assert_eq!(pending_integrity.checked_partial_cleanups, 1);
        assert_windows_pending_cleanup_warnings(&pending_integrity);
        drop(failure_guard);

        let second = recover_workspace(temporary.path()).expect("retry deferred deletion");
        assert_eq!(second.recovered_jobs, 0);
        assert_eq!(second.removed_partial_files, 0);
        assert_eq!(second.quarantined_partial_files, 0);
        assert_eq!(second.deleted_tombstones, 1);
        assert_eq!(second.cleanup_pending, [] as [CleanupPending; 0]);
        assert!(!tombstone.exists());
        let third = recover_workspace(temporary.path()).expect("idempotent recovery");
        assert_eq!(third.recovered_jobs, 0);
        assert_eq!(third.removed_partial_files, 0);
        assert_eq!(third.quarantined_partial_files, 0);
        assert_eq!(third.deleted_tombstones, 0);
        assert_eq!(third.cleanup_pending, [] as [CleanupPending; 0]);

        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("open recovered workspace");
        let cleanup_state: (String, i64) = connection
            .query_row(
                "SELECT state, cleanup_attempts FROM partial_cleanups",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("cleanup state");
        assert_eq!(cleanup_state, ("CLEANED".to_owned(), 2));
        assert_eq!(recovered_job_event_count(&connection, JOB_ID), 1);
        let final_integrity = integrity_check(temporary.path()).expect("final integrity replay");
        assert!(
            final_integrity.ok,
            "integrity issues: {:?}",
            final_integrity.issues
        );
        assert_windows_resolved_cleanup_warnings(&final_integrity);
    }

    #[cfg(windows)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn windows_inert_tombstone_is_excluded_from_governed_practitioner_consumers() {
        use crate::{
            intake::ingest_package,
            mapping::validate_mapping,
            profile_registry::{
                ProfileAuthority, RegistryLimits, approve_profile, inspect_profile,
                register_profile,
            },
            profiling::{ProfilingPolicy, run_profile},
            read_model::{CleanupFilters, IssueFilters, PageRequest, ReadService},
            validation::{ValidationPolicy, run_validation},
        };

        const INTERRUPTED_JOB_ID: &str = "77777777-7777-4777-8777-777777777777";
        const PROFILE_ID: &str = "synthetic.customer.readiness";
        const PROFILE_VERSION: &str = "1.0.0";
        const MAPPING_ID: &str = "synthetic.customer.valid";
        const MAPPING_VERSION: &str = "1.0.0";
        const SOURCE_FILE: &str = "customer_accounts.csv";

        let temporary = tempfile::tempdir().expect("temporary governed workspace");
        let workspace = temporary.path().join("workspace");
        let project = Path::new(env!("CARGO_MANIFEST_DIR"));
        let profile_schema = project.join("schemas/migration-object-profile.schema.json");
        let mapping_schema = project.join("schemas/source-mapping-version.schema.json");
        let profile_document = project.join("fixtures/wp003/synthetic-profile.reviewed.json");
        let validation_policy_path = project.join("fixtures/wp003/validation-policy.json");

        let intake_policy = IntakePolicy {
            minimum_free_space_bytes: 0,
            transaction_batch_rows: 2,
            ..IntakePolicy::default()
        };
        let accepted = ingest_package(
            &project.join("fixtures/packages/valid.zip"),
            &workspace,
            &intake_policy,
        )
        .expect("ingest accepted synthetic source");
        let profiling_policy =
            ProfilingPolicy::from_path(&project.join("config/profiling-policy.json"))
                .expect("load profiling policy");
        let profiling = run_profile(&workspace, &accepted.job_id, &profiling_policy)
            .expect("profile accepted synthetic source");

        let registered = register_profile(
            &workspace,
            &profile_schema,
            &profile_document,
            &RegistryLimits::default(),
        )
        .expect("register reviewed synthetic profile");
        let approved = approve_profile(
            &workspace,
            PROFILE_ID,
            PROFILE_VERSION,
            &registered.profile_hash,
            "WP-003 Windows inert-tombstone regression",
            "Authorize this exact synthetic profile for local mapping and validation only",
            true,
            &RegistryLimits::default(),
        )
        .expect("approve exact synthetic profile");
        assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);

        let mapping_template =
            fs::read_to_string(project.join("fixtures/wp003/valid-mapping.template.json"))
                .expect("read mapping template");
        assert_eq!(
            mapping_template.matches("__PROFILE_SHA256__").count(),
            1,
            "mapping template has one profile-hash placeholder"
        );
        let mapping_path = temporary.path().join("valid-mapping.json");
        fs::write(
            &mapping_path,
            mapping_template.replace("__PROFILE_SHA256__", &approved.profile_hash),
        )
        .expect("write exact synthetic mapping");
        let mapping = validate_mapping(
            &workspace,
            &accepted.job_id,
            &mapping_schema,
            &profile_schema,
            &mapping_path,
        )
        .expect("validate exact synthetic mapping");
        let validation_policy = ValidationPolicy::from_path(&validation_policy_path)
            .expect("load bounded validation policy");
        let baseline_validation = run_validation(
            &workspace,
            &accepted.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &profile_schema,
            &validation_policy,
        )
        .expect("run baseline governed validation");
        let profile_before = inspect_profile(&workspace, PROFILE_ID, PROFILE_VERSION)
            .expect("inspect profile before interrupted cleanup");
        let reads = ReadService::new(&workspace);
        let job_before = reads
            .job_summary(&accepted.job_id)
            .expect("read accepted job before interrupted cleanup");

        let live = {
            let writer = WorkspaceWriter::open(&workspace).expect("open governed workspace");
            create_windows_live_partial(
                &writer,
                INTERRUPTED_JOB_ID,
                b"separate interrupted synthetic partial",
            )
        };
        let failure_guard = TombstoneDeleteFailureGuard::enable();
        let recovery = recover_workspace(&workspace).expect("quarantine interrupted partial");
        assert_eq!(recovery.recovered_jobs, 1);
        assert_eq!(recovery.removed_partial_files, 1);
        assert_eq!(recovery.quarantined_partial_files, 1);
        assert_eq!(recovery.deleted_tombstones, 0);
        assert_eq!(recovery.cleanup_pending.len(), 1);
        assert!(
            !live.exists(),
            "live partial must leave the intake namespace"
        );
        let tombstone = workspace.join(&recovery.cleanup_pending[0].tombstone);
        assert!(
            tombstone.exists(),
            "inert tombstone must survive injected deletion failure"
        );

        let mapping_replay = validate_mapping(
            &workspace,
            &accepted.job_id,
            &mapping_schema,
            &profile_schema,
            &mapping_path,
        )
        .expect("inert tombstone cannot block or replace accepted mapping authority");
        assert!(mapping_replay.reused_immutable_version);
        assert_eq!(mapping_replay.mapping_hash, mapping.mapping_hash);
        let validation_replay = run_validation(
            &workspace,
            &accepted.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &profile_schema,
            &validation_policy,
        )
        .expect("inert tombstone cannot enter accepted validation authority");
        assert_eq!(
            validation_replay.result_hash, baseline_validation.result_hash,
            "equivalent governed validation remains deterministic"
        );
        let profile_after = inspect_profile(&workspace, PROFILE_ID, PROFILE_VERSION)
            .expect("inspect profile after interrupted cleanup");
        assert_eq!(profile_after, profile_before);

        let page = PageRequest::default();
        let job = reads
            .job_summary(&accepted.job_id)
            .expect("read accepted job with surviving inert tombstone");
        assert_eq!(job.data.state, "COMPLETE");
        assert_eq!(job.data.counts, job_before.data.counts);
        assert_eq!(job.data.reconciliation, job_before.data.reconciliation);
        assert_eq!(job.data.accepted_source, job_before.data.accepted_source);
        assert_eq!(
            job.data.warnings,
            [] as [crate::read_model::OperationalWarning; 0]
        );

        let files = reads
            .file_manifest(&accepted.job_id, &page)
            .expect("read accepted files with surviving inert tombstone");
        assert_eq!(files.items.len(), 1);
        assert_eq!(files.items[0].path, SOURCE_FILE);
        assert_eq!(files.items[0].row_count, 4);
        let profiling_view = reads
            .profiling_summary(&profiling.run_id, false, &page)
            .expect("read profiling with surviving inert tombstone");
        assert_eq!(profiling_view.scope.job_id, accepted.job_id);
        assert_ne!(
            profiling_view.items,
            [] as [crate::read_model::ProfilingSummaryItem; 0]
        );
        let profile_view = reads
            .profile_view(PROFILE_ID, PROFILE_VERSION, &approved.profile_hash)
            .expect("read approved profile with surviving inert tombstone");
        assert_eq!(profile_view.data.lifecycle_state, "APPROVED");
        assert_eq!(profile_view.data.profile_hash, approved.profile_hash);
        let mapping_view = reads
            .mapping_coverage(MAPPING_ID, MAPPING_VERSION, &mapping.mapping_hash, &page)
            .expect("read mapping coverage with surviving inert tombstone");
        assert_eq!(mapping_view.mapping.validated_job_id, accepted.job_id);
        assert_eq!(mapping_view.mapping.source_hash, accepted.source_hash);
        assert_eq!(mapping_view.mapping.mapping_hash, mapping.mapping_hash);
        let validation_view = reads
            .validation_summary(&validation_replay.run_id)
            .expect("read validation summary with surviving inert tombstone");
        assert_eq!(validation_view.data.state, "COMPLETE");
        assert_eq!(validation_view.data.source_hash, accepted.source_hash);
        assert_eq!(
            validation_view.data.result_hash.as_deref(),
            Some(validation_replay.result_hash.as_str())
        );
        let issues = reads
            .issue_list(&validation_replay.run_id, &IssueFilters::default(), &page)
            .expect("read issues with surviving inert tombstone");
        assert_eq!(issues.items.len(), 5);
        assert!(
            issues
                .items
                .iter()
                .all(|issue| issue.source_file == SOURCE_FILE)
        );
        let row = reads
            .row_detail(&validation_replay.run_id, SOURCE_FILE, 4)
            .expect("read exact accepted row with surviving inert tombstone");
        assert_eq!(row.data.job_id, accepted.job_id);
        assert_eq!(row.data.source_hash, accepted.source_hash);
        assert_eq!(row.data.source_file, SOURCE_FILE);
        assert_eq!(row.data.source_ordinal, 4);
        let reconciliation = reads
            .reconciliation_summary(&accepted.job_id, Some(&validation_replay.run_id), &page)
            .expect("reconcile accepted source with surviving inert tombstone");
        assert!(reconciliation.aggregate.closed);
        assert_eq!(reconciliation.aggregate.source_records, 4);
        assert_eq!(reconciliation.aggregate.stored_records, 4);
        assert_eq!(reconciliation.aggregate.pending_records, 0);

        let cleanup = reads
            .operational_cleanup(&CleanupFilters::default(), &page)
            .expect("read bounded cleanup warning");
        assert_eq!(cleanup.items.len(), 1);
        assert_eq!(cleanup.items[0].job_id.as_deref(), Some(INTERRUPTED_JOB_ID));
        assert_eq!(cleanup.items[0].namespace, "packages");
        assert_eq!(cleanup.items[0].state, "CLEANUP_PENDING");
        assert_eq!(cleanup.items[0].cleanup_attempts, 1);
        assert!(cleanup.items[0].diagnostic_present);

        let pending_integrity = integrity_check(&workspace).expect("replay pending integrity");
        assert!(
            pending_integrity.ok,
            "pending cleanup must remain integrity-safe: {:?}",
            pending_integrity.issues
        );
        assert_windows_pending_cleanup_warnings(&pending_integrity);
        assert_eq!(pending_integrity.checked_partial_cleanups, 1);

        drop(failure_guard);
        let retry = recover_workspace(&workspace).expect("retry deferred tombstone deletion");
        assert_eq!(retry.recovered_jobs, 0);
        assert_eq!(retry.deleted_tombstones, 1);
        assert_eq!(retry.cleanup_pending, [] as [CleanupPending; 0]);
        assert!(!tombstone.exists());
        let repeated = recover_workspace(&workspace).expect("repeat idempotent recovery");
        assert_eq!(repeated.recovered_jobs, 0);
        assert_eq!(repeated.removed_partial_files, 0);
        assert_eq!(repeated.quarantined_partial_files, 0);
        assert_eq!(repeated.deleted_tombstones, 0);
        assert_eq!(repeated.cleanup_pending, [] as [CleanupPending; 0]);

        let cleaned = reads
            .operational_cleanup(&CleanupFilters::default(), &page)
            .expect("read cleaned tombstone lifecycle");
        assert_eq!(cleaned.items.len(), 1);
        assert_eq!(cleaned.items[0].state, "CLEANED");
        assert_eq!(cleaned.items[0].cleanup_attempts, 2);
        let final_integrity = integrity_check(&workspace).expect("replay final integrity");
        assert!(
            final_integrity.ok,
            "integrity issues: {:?}",
            final_integrity.issues
        );
        assert_windows_resolved_cleanup_warnings(&final_integrity);
    }

    #[cfg(windows)]
    #[test]
    fn windows_live_and_planned_tombstone_are_an_explicit_no_replace_conflict() {
        const JOB_ID: &str = "55555555-5555-4555-8555-555555555555";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let live = create_windows_live_partial(&writer, JOB_ID, b"live bytes");
        let cleanup = plan_cleanup(&writer.connection, CleanupNamespace::Packages, JOB_ID)
            .expect("persist cleanup plan");
        let tombstone = live
            .parent()
            .expect("live parent")
            .join(format!("{}.tombstone", cleanup.cleanup_id));
        fs::write(&tombstone, b"existing tombstone bytes").expect("existing tombstone");
        drop(writer);

        let error = recover_workspace(temporary.path()).expect_err("simultaneous artifacts fail");
        assert!(error.to_string().contains("simultaneously"));
        assert_eq!(fs::read(&live).expect("live remains"), b"live bytes");
        assert_eq!(
            fs::read(&tombstone).expect("tombstone remains"),
            b"existing tombstone bytes"
        );
        let repeated = recover_workspace(temporary.path()).expect_err("conflict remains explicit");
        assert!(repeated.to_string().contains("CONFLICT"));

        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("open conflicted workspace");
        let cleanup_state: (String, Option<String>) = connection
            .query_row(
                "SELECT state, last_error_code FROM partial_cleanups
                 WHERE cleanup_id = ?1",
                [&cleanup.cleanup_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("cleanup conflict");
        assert_eq!(
            cleanup_state,
            ("CONFLICT".to_owned(), Some("RECOVERY_CONFLICT".to_owned()))
        );
        assert_eq!(recovered_job_event_count(&connection, JOB_ID), 1);
        let complete_jobs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
                [],
                |row| row.get(0),
            )
            .expect("complete jobs");
        assert_eq!(complete_jobs, 0);
        let integrity = integrity_check(temporary.path()).expect("integrity replay");
        assert!(!integrity.ok);
        assert!(
            integrity
                .issues
                .iter()
                .any(|issue| issue.contains("CONFLICT"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_recovery_rejects_a_reparse_partial_without_touching_its_target() {
        use std::os::windows::fs::symlink_file;

        const JOB_ID: &str = "66666666-6666-4666-8666-666666666666";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        create_windows_job(&writer, JOB_ID);
        let packages = temporary.path().join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages");
        let outside = temporary.path().join("outside-partial");
        fs::write(&outside, b"outside bytes").expect("outside target");
        let live = packages.join(format!("{JOB_ID}.part"));
        symlink_file(&outside, &live).expect("partial reparse point");
        drop(writer);

        let error = recover_workspace(temporary.path()).expect_err("reparse partial must fail");
        assert!(error.to_string().contains("non-reparse"));
        assert_eq!(
            fs::read(&outside).expect("outside target remains"),
            b"outside bytes"
        );
        assert_eq!(
            workspace_status(temporary.path())
                .expect("status")
                .complete_jobs,
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_recovery_rejects_a_reparse_namespace_without_touching_its_target() {
        use std::os::windows::fs::symlink_dir;

        const JOB_ID: &str = "77777777-7777-4777-8777-777777777777";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        create_windows_job(&writer, JOB_ID);
        drop(writer);
        let outside = temporary.path().join("outside-packages");
        fs::create_dir_all(&outside).expect("outside namespace");
        let outside_partial = outside.join(format!("{JOB_ID}.part"));
        fs::write(&outside_partial, b"outside bytes").expect("outside partial");
        symlink_dir(&outside, temporary.path().join(PACKAGES_DIRECTORY))
            .expect("package namespace reparse point");

        let error = recover_workspace(temporary.path()).expect_err("reparse namespace must fail");
        assert!(
            error.to_string().contains("reparse point")
                || error.to_string().contains("not a real directory")
        );
        assert_eq!(
            fs::read(&outside_partial).expect("outside partial remains"),
            b"outside bytes"
        );
        assert_eq!(
            workspace_status(temporary.path())
                .expect("status")
                .complete_jobs,
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cleanup_never_ignores_a_case_variant_reserved_extension() {
        const JOB_ID: &str = "88888888-8888-4888-8888-888888888888";
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        create_windows_job(&writer, JOB_ID);
        let packages = temporary.path().join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages");
        let variant = packages.join(format!("{JOB_ID}.PART"));
        fs::write(&variant, b"case variant").expect("case-variant partial");
        drop(writer);

        let error = recover_workspace(temporary.path()).expect_err("variant must fail explicitly");
        assert!(error.to_string().contains("canonical lowercase extension"));
        assert_eq!(
            fs::read(&variant).expect("variant remains"),
            b"case variant"
        );
        assert_eq!(
            workspace_status(temporary.path())
                .expect("status")
                .complete_jobs,
            0
        );
    }

    fn remove_compared_validation_schema_for_v10_fixture(connection: &Connection) {
        connection
            .execute_batch(
                "DROP TABLE engagement_remediation_issue_members;
                 DROP TABLE engagement_remediation_pack_members;
                 DROP TABLE engagement_remediation_pack_results;
                 DROP TABLE engagement_remediation_packs;
                 DROP TABLE engagement_remediation_pack_selections;
                 DROP TABLE engagement_remediation_pack_attempts;
                 DROP TABLE engagement_validation_comparison_items;
                 DROP TABLE engagement_validation_comparisons;
                 DROP TABLE engagement_tour_finding_members;
                 DROP TABLE engagement_tour_finding_sets;
                 DROP TABLE engagement_tour_heads;
                 DROP TABLE engagement_tours;
                 DROP TRIGGER validation_runs_source_reuse_no_update;
                 DROP TRIGGER validation_runs_source_reuse_insert_guard;
                 DROP INDEX validation_runs_source_reuse;
                 ALTER TABLE validation_runs DROP COLUMN source_reuse_hash;
                 ALTER TABLE validation_runs DROP COLUMN source_reuse_id;
                 DROP TABLE engagement_mapping_reuse_authorities;
                 DROP TABLE engagement_mapping_authorities;
                 DROP TABLE engagement_comparison_namespaces;
                 DROP TABLE engagement_tour_command_attempts;
                 DROP TABLE engagement_tour_attempts;
                 PRAGMA user_version = 10;",
            )
            .expect("construct exact v10 schema fixture");
    }

    fn remove_mapping_workspace_schema_for_v9_fixture(connection: &Connection) {
        remove_compared_validation_schema_for_v10_fixture(connection);
        connection
            .execute_batch(
                "DROP TABLE engagement_mapping_approvals;
                 DROP TABLE engagement_mapping_workspace_attempts;
                 DROP TABLE engagement_mapping_draft_revisions;
                 DROP TABLE engagement_mapping_draft_heads;
                 PRAGMA user_version = 9;",
            )
            .expect("construct exact v9 schema fixture");
    }

    fn remove_wp006_schema_for_v6_fixture(connection: &Connection) {
        remove_mapping_workspace_schema_for_v9_fixture(connection);
        connection
            .execute_batch(
                "DROP TABLE migration_result_actions;
                 DROP TABLE migration_result_candidates;
                 DROP TABLE reconciliation_row_lineage_references;
                 DROP TABLE reconciliation_row_classifications;
                 DROP TABLE reconciliation_category_counts;
                 DROP TABLE reconciliation_snapshots;
                 DROP TABLE reconciliation_attempt_events;
                 DROP TABLE reconciliation_attempts;
                 DROP TABLE reconciliation_policy_activations;
                 DROP TABLE reconciliation_policy_documents;
                 PRAGMA user_version = 6;",
            )
            .expect("construct exact v6 schema fixture");
    }

    fn schema_object_exists(connection: &Connection, kind: &str, name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2
                 )",
                params![kind, name],
                |row| row.get::<_, i64>(0),
            )
            .expect("query schema object")
            == 1
    }

    #[test]
    fn remediation_schema_authority_tamper_fails_integrity_replay() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        writer
            .connection
            .execute_batch(
                "DROP TRIGGER engagement_remediation_packs_no_update;
                 CREATE TRIGGER engagement_remediation_packs_no_update
                     BEFORE UPDATE ON engagement_remediation_packs
                     BEGIN SELECT 1; END;",
            )
            .expect("replace one remediation schema authority offline");
        let mut issues = Vec::new();
        crate::engagement::verify_remediation_integrity(
            temporary.path(),
            &writer.connection,
            &mut issues,
        )
        .expect("run remediation integrity replay");
        assert!(
            issues.iter().any(|issue| {
                issue.contains("v14 remediation/mapping schema authority")
                    && issue.contains("engagement_remediation_packs_no_update")
            }),
            "altered schema authority must fail exact replay: {issues:?}"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn fresh_workspace_has_bounded_compared_validation_schema_and_guards() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);

        for table in [
            "engagement_tour_heads",
            "engagement_tour_attempts",
            "engagement_tour_command_attempts",
            "engagement_tours",
            "engagement_mapping_reuse_authorities",
            "engagement_comparison_namespaces",
            "engagement_tour_finding_sets",
            "engagement_tour_finding_members",
            "engagement_validation_comparisons",
            "engagement_validation_comparison_items",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "table", table),
                "missing compared-validation table {table}"
            );
        }
        for index in [
            "engagement_tour_attempts_history",
            "engagement_tour_command_attempts_history",
            "engagement_mapping_reuse_authorities_history",
            "validation_runs_source_reuse",
            "engagement_tours_history",
            "engagement_tour_finding_members_subject",
            "engagement_validation_comparisons_history",
            "engagement_validation_comparison_items_category",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "index", index),
                "missing compared-validation index {index}"
            );
        }
        for trigger in [
            "engagement_tour_attempts_no_update",
            "engagement_tour_attempts_no_delete",
            "engagement_tour_command_attempts_no_update",
            "engagement_tour_command_attempts_no_delete",
            "engagement_comparison_namespaces_no_update",
            "engagement_comparison_namespaces_no_delete",
            "engagement_mapping_reuse_authorities_no_update",
            "engagement_mapping_reuse_authorities_no_delete",
            "validation_runs_source_reuse_insert_guard",
            "validation_runs_source_reuse_no_update",
            "engagement_tours_no_update",
            "engagement_tours_no_delete",
            "engagement_tour_heads_guarded_update",
            "engagement_tour_heads_no_delete",
            "engagement_tour_finding_sets_no_update",
            "engagement_tour_finding_sets_no_delete",
            "engagement_tour_finding_members_no_update",
            "engagement_tour_finding_members_no_delete",
            "engagement_validation_comparisons_no_update",
            "engagement_validation_comparisons_no_delete",
            "engagement_validation_comparison_items_no_update",
            "engagement_validation_comparison_items_no_delete",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "trigger", trigger),
                "missing compared-validation guard {trigger}"
            );
        }

        let validation_columns = {
            let mut statement = writer
                .connection
                .prepare("PRAGMA table_info(validation_runs)")
                .expect("validation column query");
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("validation columns")
                .collect::<rusqlite::Result<HashSet<_>>>()
                .expect("collect validation columns")
        };
        assert!(validation_columns.contains("source_reuse_id"));
        assert!(validation_columns.contains("source_reuse_hash"));

        let hash_a = "a".repeat(64);
        let hash_b = "b".repeat(64);
        let hash_c = "c".repeat(64);
        let hash_d = "d".repeat(64);
        writer
            .connection
            .execute(
                "INSERT INTO engagement_records (
                     singleton, engagement_id, schema_version, display_name,
                     idempotency_key, request_hash, canonical_request_json,
                     created_at
                 ) VALUES (
                     1, 'compared-validation-engagement',
                     'migration-stream/engagement/record/v1',
                     'Compared validation migration fixture',
                     'compared-validation-create-key', ?1, '{}',
                     '2026-07-29T00:00:00Z'
                 )",
                [&hash_a],
            )
            .expect("insert compared-validation engagement");
        writer
            .connection
            .execute(
                "INSERT INTO engagement_active_context (
                     engagement_id, updated_at
                 ) VALUES (
                     'compared-validation-engagement',
                     '2026-07-29T00:00:00Z'
                 )",
                [],
            )
            .expect("insert compared-validation active context");

        let short_secret = writer
            .connection
            .execute(
                "INSERT INTO engagement_comparison_namespaces (
                     namespace_id, engagement_id, algorithm_version,
                     secret, secret_hash, created_at
                 ) VALUES (
                     'compared-validation-namespace',
                     'compared-validation-engagement',
                     'migration-stream/compared-validation-fingerprint/v1',
                     zeroblob(31), ?1, '2026-07-29T00:00:01Z'
                 )",
                [&hash_b],
            )
            .expect_err("comparison secret must be exactly bounded");
        assert!(short_secret.to_string().contains("CHECK constraint failed"));
        writer
            .connection
            .execute(
                "INSERT INTO engagement_comparison_namespaces (
                     namespace_id, engagement_id, algorithm_version,
                     secret, secret_hash, created_at
                 ) VALUES (
                     'compared-validation-namespace',
                     'compared-validation-engagement',
                     'migration-stream/compared-validation-fingerprint/v1',
                     zeroblob(32), ?1, '2026-07-29T00:00:01Z'
                 )",
                [&hash_b],
            )
            .expect("insert bounded private comparison namespace");
        let namespace_mutation = writer
            .connection
            .execute(
                "UPDATE engagement_comparison_namespaces
                 SET secret_hash = ?1
                 WHERE namespace_id = 'compared-validation-namespace'",
                [&hash_c],
            )
            .expect_err("comparison namespace must be immutable");
        assert!(namespace_mutation.to_string().contains("immutable"));

        writer
            .connection
            .execute(
                "INSERT INTO engagement_tour_attempts (
                     tour_attempt_id, engagement_id, planned_tour_id,
                     tour_number, predecessor_tour_id,
                     started_by, reason, canonical_attempt_json,
                     attempt_hash, created_at
                 ) VALUES (
                     'tour-attempt-1', 'compared-validation-engagement',
                     'planned-tour-1', 1, NULL,
                     'migration tester', 'Start compared validation.',
                     '{}', ?1, '2026-07-29T00:00:02Z'
                 )",
                [&hash_c],
            )
            .expect("insert immutable tour attempt");
        writer
            .connection
            .execute(
                "INSERT INTO engagement_tour_attempts (
                     tour_attempt_id, engagement_id, planned_tour_id,
                     tour_number, predecessor_tour_id,
                     started_by, reason, canonical_attempt_json,
                     attempt_hash, created_at
                 ) VALUES (
                     'tour-attempt-1-retry',
                     'compared-validation-engagement',
                     'planned-tour-1-retry', 1, NULL,
                     'migration tester', 'Retry a failed planned tour.',
                     '{}', ?1, '2026-07-29T00:00:03Z'
                 )",
                [&hash_d],
            )
            .expect("failed planned tours must not block a bounded retry");
        let tour_attempt_mutation = writer
            .connection
            .execute(
                "UPDATE engagement_tour_attempts
                 SET reason = 'mutated'
                 WHERE tour_attempt_id = 'tour-attempt-1'",
                [],
            )
            .expect_err("tour attempt must be immutable");
        assert!(tour_attempt_mutation.to_string().contains("immutable"));

        writer
            .connection
            .execute(
                "INSERT INTO engagement_tour_command_attempts (
                     attempt_id, engagement_id, tour_attempt_id,
                     command_kind, idempotency_key,
                     request_hash, canonical_request_json,
                     expected_generation, result_generation,
                     result_reference, result_hash, created_at
                 ) VALUES (
                     'start-tour-command-1',
                     'compared-validation-engagement', NULL,
                     'START_TOUR', 'start-tour-idempotency-0001',
                     ?1, '{}', 0, 1, 'tour-attempt-1', ?2,
                     '2026-07-29T00:00:02Z'
                 )",
                params![hash_a, hash_d],
            )
            .expect("insert immutable start command");
        let invalid_command_scope = writer
            .connection
            .execute(
                "INSERT INTO engagement_tour_command_attempts (
                     attempt_id, engagement_id, tour_attempt_id,
                     command_kind, idempotency_key,
                     request_hash, canonical_request_json,
                     expected_generation, result_generation,
                     result_reference, result_hash, created_at
                 ) VALUES (
                     'accept-source-command-invalid',
                     'compared-validation-engagement', NULL,
                     'ACCEPT_SOURCE', 'accept-source-idempotency-1',
                     ?1, '{}', 1, 2, 'source-reuse-1', ?2,
                     '2026-07-29T00:00:03Z'
                 )",
                params![hash_a, hash_d],
            )
            .expect_err("non-start command requires an exact tour attempt");
        assert!(
            invalid_command_scope
                .to_string()
                .contains("CHECK constraint failed")
        );

        writer
            .connection
            .execute(
                "INSERT INTO engagement_tour_heads (
                     engagement_id, generation, active_tour_id,
                     pending_tour_attempt_id, updated_at
                 ) VALUES (
                     'compared-validation-engagement', 1, NULL,
                     'tour-attempt-1', '2026-07-29T00:00:02Z'
                 )",
                [],
            )
            .expect("insert tour CAS head");
        let skipped_head_generation = writer
            .connection
            .execute(
                "UPDATE engagement_tour_heads
                 SET generation = 3, updated_at = '2026-07-29T00:00:04Z'
                 WHERE engagement_id = 'compared-validation-engagement'",
                [],
            )
            .expect_err("tour head generation cannot skip");
        assert!(
            skipped_head_generation
                .to_string()
                .contains("transition is invalid")
        );

        let one_sided_source_reuse = writer
            .connection
            .execute(
                "INSERT INTO validation_runs (
                     id, identity_hash, job_id, source_hash,
                     profile_id, profile_version, profile_hash,
                     mapping_id, mapping_version, mapping_hash,
                     rule_set_hash, engine_version, policy_json, policy_hash,
                     state, created_at, updated_at,
                     source_reuse_id, source_reuse_hash
                 ) VALUES (
                     'invalid-source-reuse-run', ?1, 'missing-job', ?1,
                     'missing-profile', '1.0.0', ?1,
                     'missing-mapping', '1.0.0', ?1,
                     ?1, 'migration-stream-validation/v1', '{}', ?1,
                     'RUNNING', '2026-07-29T00:00:05Z',
                     '2026-07-29T00:00:05Z', NULL, ?2
                 )",
                params![hash_a, hash_b],
            )
            .expect_err("validation source-reuse columns must be paired");
        assert!(
            one_sided_source_reuse
                .to_string()
                .contains("source-reuse authority")
        );

        let member_schema: String = writer
            .connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'table'
                   AND name = 'engagement_tour_finding_members'",
                [],
                |row| row.get(0),
            )
            .expect("finding-member schema");
        for required in [
            "occurrence_index",
            "subject_kind",
            "subject_algorithm",
            "UNIQUE_SCOPE_HMAC_SHA256_V1",
            "RELATIONSHIP_ENTITY_KEY_HMAC_SHA256_V1",
            "RELATIONSHIP_PROJECTED_ROW_HMAC_SHA256_V1",
            "display_message",
            "relationship_role",
            "relationship_cardinality",
            "relationship_key_pair_hash",
        ] {
            assert!(
                member_schema.contains(required),
                "finding-member schema lacks {required}"
            );
        }

        for table in [
            "engagement_tour_finding_sets",
            "engagement_validation_comparisons",
        ] {
            let schema: String = writer
                .connection
                .query_row(
                    "SELECT sql FROM sqlite_schema
                     WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("validation authority schema");
            for required in ["validation_engine_version", "validation_policy_hash"] {
                assert!(schema.contains(required), "{table} schema lacks {required}");
            }
        }
    }

    #[test]
    fn exact_v10_upgrade_preserves_data_and_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer =
                WorkspaceWriter::open(temporary.path()).expect("create current v11 schema");
            writer
                .connection
                .execute(
                    "INSERT INTO engagement_records (
                         singleton, engagement_id, schema_version, display_name,
                         idempotency_key, request_hash, canonical_request_json,
                         created_at
                     ) VALUES (
                         1, 'v10-upgrade-engagement',
                         'migration-stream/engagement/record/v1',
                         'Preserved v10 engagement',
                         'v10-upgrade-create-key', ?1, '{}',
                         '2026-07-29T00:00:00Z'
                     )",
                    ["a".repeat(64)],
                )
                .expect("insert preserved engagement");
            remove_compared_validation_schema_for_v10_fixture(&writer.connection);
        }

        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("upgrade exact v10");
            let version: i64 = writer
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("schema version");
            assert_eq!(version, SCHEMA_VERSION);
            let preserved: String = writer
                .connection
                .query_row(
                    "SELECT display_name FROM engagement_records
                     WHERE engagement_id = 'v10-upgrade-engagement'",
                    [],
                    |row| row.get(0),
                )
                .expect("preserved v10 engagement");
            assert_eq!(preserved, "Preserved v10 engagement");
            assert!(schema_object_exists(
                &writer.connection,
                "table",
                "engagement_validation_comparisons"
            ));
        }

        let before_reopen = {
            let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
                .expect("inspect first v11 upgrade");
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE name LIKE 'engagement_tour_%'
                        OR name LIKE 'engagement_validation_comparison%'
                        OR name LIKE 'engagement_mapping_reuse%'
                        OR name LIKE 'engagement_comparison_namespace%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count compared-validation objects")
        };
        let writer = WorkspaceWriter::open(temporary.path()).expect("idempotent v11 reopen");
        let after_reopen: i64 = writer
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE name LIKE 'engagement_tour_%'
                    OR name LIKE 'engagement_validation_comparison%'
                    OR name LIKE 'engagement_mapping_reuse%'
                    OR name LIKE 'engagement_comparison_namespace%'",
                [],
                |row| row.get(0),
            )
            .expect("recount compared-validation objects");
        assert_eq!(after_reopen, before_reopen);
    }

    #[test]
    fn failed_v10_upgrade_rolls_back_all_compared_validation_objects() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer =
                WorkspaceWriter::open(temporary.path()).expect("create current v11 schema");
            remove_compared_validation_schema_for_v10_fixture(&writer.connection);
            writer
                .connection
                .execute_batch("CREATE TABLE engagement_tours (sentinel TEXT);")
                .expect("create late v11 migration collision");
        }

        let Err(error) = WorkspaceWriter::open(temporary.path()) else {
            panic!("v11 migration must roll back");
        };
        assert!(error.to_string().contains("already exists"));
        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("inspect rolled-back v10 database");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 10);
        assert!(schema_object_exists(
            &connection,
            "table",
            "engagement_tours"
        ));
        assert!(!schema_object_exists(
            &connection,
            "table",
            "engagement_tour_attempts"
        ));
        let source_reuse_column_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('validation_runs')
                 WHERE name IN ('source_reuse_id', 'source_reuse_hash')",
                [],
                |row| row.get(0),
            )
            .expect("rolled-back validation columns");
        assert_eq!(source_reuse_column_count, 0);
    }

    fn insert_mapping_workspace_authority_fixture(connection: &Connection) {
        let source_hash = "a".repeat(64);
        let profile_hash = "b".repeat(64);
        let schema_hash = "c".repeat(64);
        let validation_hash = "d".repeat(64);
        let attempt_hash = "e".repeat(64);
        let mapping_hash = "f".repeat(64);
        connection
            .execute(
                "INSERT INTO engagement_records (
                     singleton, engagement_id, schema_version, display_name,
                     idempotency_key, request_hash, canonical_request_json,
                     created_at
                 ) VALUES (
                     1, 'mapping-workspace-engagement',
                     'migration-stream/engagement/record/v1',
                     'Mapping workspace migration fixture',
                     'mapping-workspace-create-key', ?1, '{}',
                     '2026-07-28T00:00:00Z'
                 )",
                [&source_hash],
            )
            .expect("insert mapping-workspace engagement");
        connection
            .execute(
                "INSERT INTO engagement_active_context (
                     engagement_id, updated_at
                 ) VALUES (
                     'mapping-workspace-engagement',
                     '2026-07-28T00:00:00Z'
                 )",
                [],
            )
            .expect("insert mapping-workspace engagement context");
        create_job(
            connection,
            &NewJob {
                id: "mapping-workspace-job",
                source_name: "synthetic.zip",
                source_path: "/synthetic.zip",
                source_size: Some(10),
                policy_json: "{}",
            },
        )
        .expect("insert mapping-workspace source job");
        connection
            .execute(
                "INSERT INTO profile_versions (
                     profile_id, profile_version, profile_hash, schema_hash,
                     document_json, declared_status, validation_hash,
                     registered_at
                 ) VALUES (
                     'synthetic.sales-document.readiness', '1.0.0', ?1, ?2,
                     '{}', 'REVIEWED_DRAFT', ?3,
                     '2026-07-28T00:00:01Z'
                 )",
                params![profile_hash, schema_hash, validation_hash],
            )
            .expect("insert mapping-workspace profile");
        connection
            .execute(
                "INSERT INTO mapping_validation_attempts (
                     id, input_hash, state, mapping_id, mapping_version,
                     mapping_hash, errors_json, created_at, attempt_hash
                 ) VALUES (
                     'mapping-workspace-validation-attempt', ?1, 'VALID',
                     'sales-document.mapping', '1.0.0', ?2, '[]',
                     '2026-07-28T00:00:02Z', ?3
                 )",
                params![source_hash, mapping_hash, attempt_hash],
            )
            .expect("insert mapping-workspace validation attempt");
        connection
            .execute(
                "INSERT INTO mapping_versions (
                     mapping_id, mapping_version, mapping_hash, attempt_id,
                     profile_id, profile_version, profile_hash, source_hash,
                     validated_job_id, document_json, coverage_json,
                     coverage_hash, created_at
                 ) VALUES (
                     'sales-document.mapping', '1.0.0', ?1,
                     'mapping-workspace-validation-attempt',
                     'synthetic.sales-document.readiness', '1.0.0', ?2, ?3,
                     'mapping-workspace-job', '{}', '{}', ?4,
                     '2026-07-28T00:00:02Z'
                 )",
                params![mapping_hash, profile_hash, source_hash, validation_hash],
            )
            .expect("insert mapping-workspace mapping version");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn fresh_workspace_has_bounded_mapping_workspace_schema_and_guards() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);

        for table in [
            "engagement_mapping_draft_heads",
            "engagement_mapping_draft_revisions",
            "engagement_mapping_workspace_attempts",
            "engagement_mapping_approvals",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "table", table),
                "missing mapping-workspace table {table}"
            );
        }
        for index in [
            "engagement_mapping_draft_heads_history",
            "engagement_mapping_workspace_attempts_history",
            "engagement_mapping_approvals_mapping",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "index", index),
                "missing mapping-workspace index {index}"
            );
        }
        for trigger in [
            "engagement_mapping_draft_heads_guarded_update",
            "engagement_mapping_draft_heads_no_delete",
            "engagement_mapping_draft_revisions_no_update",
            "engagement_mapping_draft_revisions_no_delete",
            "engagement_mapping_workspace_attempts_no_update",
            "engagement_mapping_workspace_attempts_no_delete",
            "engagement_mapping_approvals_no_update",
            "engagement_mapping_approvals_no_delete",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "trigger", trigger),
                "missing mapping-workspace guard {trigger}"
            );
        }

        insert_mapping_workspace_authority_fixture(&writer.connection);
        let source_hash = "a".repeat(64);
        let profile_hash = "b".repeat(64);
        let first_revision_hash = "1".repeat(64);
        let second_revision_hash = "2".repeat(64);
        let mapping_hash = "f".repeat(64);
        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_draft_heads (
                     draft_id, engagement_id, source_job_id, source_hash,
                     object_pack_id, object_pack_version,
                     profile_id, profile_version, profile_hash,
                     generation, state, head_revision_hash,
                     created_at, updated_at
                 ) VALUES (
                     'sales-document-draft', 'mapping-workspace-engagement',
                     'mapping-workspace-job', ?1,
                     'synthetic.sales-document.readiness', '1.0.0',
                     'synthetic.sales-document.readiness', '1.0.0', ?2,
                     1, 'EDITABLE', ?3,
                     '2026-07-28T00:00:03Z', '2026-07-28T00:00:03Z'
                 )",
                params![source_hash, profile_hash, first_revision_hash],
            )
            .expect("insert editable mapping draft head");
        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_draft_revisions (
                     draft_id, generation, revision_hash,
                     parent_revision_hash, canonical_draft_json,
                     actor, created_at
                 ) VALUES (
                     'sales-document-draft', 1, ?1, NULL, '{}',
                     'migration tester', '2026-07-28T00:00:03Z'
                 )",
                [&first_revision_hash],
            )
            .expect("insert first immutable draft revision");
        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_workspace_attempts (
                     attempt_id, engagement_id, draft_id, command_kind,
                     idempotency_key, request_hash, canonical_request_json,
                     expected_generation, expected_state,
                     result_generation, result_state,
                     result_reference, result_hash, created_at
                 ) VALUES (
                     'save-draft-attempt-1', 'mapping-workspace-engagement',
                     'sales-document-draft', 'SAVE_DRAFT',
                     'save-draft-idempotency-0001', ?1, '{}',
                     0, 'ABSENT', 1, 'EDITABLE', ?2, ?2,
                     '2026-07-28T00:00:03Z'
                 )",
                params![source_hash, first_revision_hash],
            )
            .expect("insert immutable save attempt");

        let immutable_revision = writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_revisions
                 SET actor = 'mutated'
                 WHERE draft_id = 'sales-document-draft' AND generation = 1",
                [],
            )
            .expect_err("draft revisions must be immutable");
        assert!(immutable_revision.to_string().contains("immutable"));
        let skipped_generation = writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_heads
                 SET generation = 3, head_revision_hash = ?1,
                     updated_at = '2026-07-28T00:00:04Z'
                 WHERE draft_id = 'sales-document-draft'",
                [&second_revision_hash],
            )
            .expect_err("draft generation cannot skip");
        assert!(
            skipped_generation
                .to_string()
                .contains("transition is invalid")
        );

        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_draft_revisions (
                     draft_id, generation, revision_hash,
                     parent_revision_hash, canonical_draft_json,
                     actor, created_at
                 ) VALUES (
                     'sales-document-draft', 2, ?1, ?2, '{\"generation\":2}',
                     'migration tester', '2026-07-28T00:00:04Z'
                 )",
                params![second_revision_hash, first_revision_hash],
            )
            .expect("insert successor draft revision");
        writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_heads
                 SET generation = 2, head_revision_hash = ?1,
                     updated_at = '2026-07-28T00:00:04Z'
                 WHERE draft_id = 'sales-document-draft'
                   AND generation = 1 AND state = 'EDITABLE'",
                [&second_revision_hash],
            )
            .expect("advance draft head by one CAS generation");
        writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_heads
                 SET state = 'SUBMITTED',
                     submitted_mapping_id = 'sales-document.mapping',
                     submitted_mapping_version = '1.0.0',
                     submitted_mapping_hash = ?1,
                     submitted_at = '2026-07-28T00:00:05Z',
                     updated_at = '2026-07-28T00:00:05Z'
                 WHERE draft_id = 'sales-document-draft'
                   AND generation = 2 AND state = 'EDITABLE'",
                [&mapping_hash],
            )
            .expect("freeze submitted mapping draft");
        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_workspace_attempts (
                     attempt_id, engagement_id, draft_id, command_kind,
                     idempotency_key, request_hash, canonical_request_json,
                     expected_generation, expected_state,
                     result_generation, result_state,
                     result_reference, result_hash, created_at
                 ) VALUES (
                     'approve-mapping-attempt-1',
                     'mapping-workspace-engagement', 'sales-document-draft',
                     'APPROVE_MAPPING', 'approve-mapping-idempotency-0001',
                     ?1, '{}', 2, 'SUBMITTED', 2, 'APPROVED',
                     'sales-document-approval', ?2,
                     '2026-07-28T00:00:06Z'
                 )",
                params![source_hash, mapping_hash],
            )
            .expect("insert immutable approval attempt");
        let approval_hash = "9".repeat(64);
        writer
            .connection
            .execute(
                "INSERT INTO engagement_mapping_approvals (
                     approval_id, attempt_id, engagement_id, draft_id,
                     draft_generation, draft_revision_hash,
                     mapping_id, mapping_version, mapping_hash,
                     approver, reason, acknowledgement,
                     canonical_approval_json, approval_hash, approved_at
                 ) VALUES (
                     'sales-document-approval', 'approve-mapping-attempt-1',
                     'mapping-workspace-engagement', 'sales-document-draft',
                     2, ?1, 'sales-document.mapping', '1.0.0', ?2,
                     'migration tester', 'Approve exact immutable mapping.',
                     'migration-stream/local-mapping-version-approval/v1',
                     '{}', ?3, '2026-07-28T00:00:06Z'
                 )",
                params![second_revision_hash, mapping_hash, approval_hash],
            )
            .expect("insert exact immutable mapping approval");
        writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_heads
                 SET state = 'APPROVED',
                     approved_at = '2026-07-28T00:00:06Z',
                     updated_at = '2026-07-28T00:00:06Z'
                 WHERE draft_id = 'sales-document-draft'
                   AND generation = 2 AND state = 'SUBMITTED'",
                [],
            )
            .expect("approve submitted mapping head");

        let immutable_approval = writer
            .connection
            .execute(
                "UPDATE engagement_mapping_approvals
                 SET reason = 'mutated'
                 WHERE approval_id = 'sales-document-approval'",
                [],
            )
            .expect_err("mapping approval must be immutable");
        assert!(immutable_approval.to_string().contains("immutable"));
        let terminal_head = writer
            .connection
            .execute(
                "UPDATE engagement_mapping_draft_heads
                 SET updated_at = '2026-07-28T00:00:07Z'
                 WHERE draft_id = 'sales-document-draft'",
                [],
            )
            .expect_err("approved draft head must be terminal");
        assert!(terminal_head.to_string().contains("transition is invalid"));
    }

    #[test]
    fn exact_v9_upgrade_preserves_data_and_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path())
                .expect("create current mapping-workspace schema");
            insert_mapping_workspace_authority_fixture(&writer.connection);
            remove_mapping_workspace_schema_for_v9_fixture(&writer.connection);
        }

        {
            let writer =
                WorkspaceWriter::open(temporary.path()).expect("upgrade exact v9 workspace");
            let version: i64 = writer
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("schema version");
            assert_eq!(version, SCHEMA_VERSION);
            let preserved: String = writer
                .connection
                .query_row(
                    "SELECT display_name FROM engagement_records
                     WHERE engagement_id = 'mapping-workspace-engagement'",
                    [],
                    |row| row.get(0),
                )
                .expect("preserved v9 engagement");
            assert_eq!(preserved, "Mapping workspace migration fixture");
            assert!(schema_object_exists(
                &writer.connection,
                "table",
                "engagement_mapping_draft_heads"
            ));
        }

        let before_reopen = {
            let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
                .expect("inspect first v10 upgrade");
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE name LIKE 'engagement_mapping_%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count mapping-workspace objects")
        };
        let writer = WorkspaceWriter::open(temporary.path()).expect("idempotent v10 reopen");
        let after_reopen: i64 = writer
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE name LIKE 'engagement_mapping_%'",
                [],
                |row| row.get(0),
            )
            .expect("recount mapping-workspace objects");
        assert_eq!(after_reopen, before_reopen);
    }

    #[test]
    fn failed_v9_upgrade_rolls_back_every_mapping_workspace_object() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path())
                .expect("create current mapping-workspace schema");
            remove_mapping_workspace_schema_for_v9_fixture(&writer.connection);
            writer
                .connection
                .execute_batch("CREATE TABLE engagement_mapping_approvals (sentinel TEXT);")
                .expect("create late v10 migration collision");
        }

        let Err(error) = WorkspaceWriter::open(temporary.path()) else {
            panic!("v10 migration must roll back");
        };
        assert!(error.to_string().contains("already exists"));
        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("inspect rolled-back v9 database");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 9);
        assert!(schema_object_exists(
            &connection,
            "table",
            "engagement_mapping_approvals"
        ));
        assert!(!schema_object_exists(
            &connection,
            "table",
            "engagement_mapping_draft_heads"
        ));
    }

    #[test]
    fn fresh_workspace_has_complete_wp006_schema_and_bounded_indexes() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);

        for table in [
            "reconciliation_policy_documents",
            "reconciliation_policy_activations",
            "reconciliation_attempts",
            "reconciliation_attempt_events",
            "reconciliation_snapshots",
            "reconciliation_category_counts",
            "reconciliation_row_classifications",
            "reconciliation_row_lineage_references",
            "migration_result_candidates",
            "migration_result_actions",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "table", table),
                "missing WP-006 table {table}"
            );
        }

        for index in [
            "reconciliation_policy_activation_idempotency",
            "reconciliation_policy_activation_history",
            "reconciliation_attempts_idempotency",
            "reconciliation_attempts_job_history",
            "reconciliation_attempt_events_history",
            "reconciliation_snapshots_job_history",
            "reconciliation_rows_category_page",
            "reconciliation_rows_entity_page",
            "reconciliation_row_lineage_filter",
            "migration_result_candidates_idempotency",
            "migration_result_candidates_generation",
            "migration_result_candidates_job_history",
            "migration_result_actions_idempotency",
            "migration_result_actions_generation",
            "migration_result_actions_predecessor",
            "migration_result_actions_job_history",
            "migration_result_actions_candidate_history",
        ] {
            assert!(
                schema_object_exists(&writer.connection, "index", index),
                "missing WP-006 index {index}"
            );
        }
        let immutable_trigger_count: i64 = writer
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'trigger'
                   AND (name LIKE 'reconciliation_%_no_update'
                        OR name LIKE 'reconciliation_%_no_delete'
                        OR name LIKE 'migration_result_%_no_update'
                        OR name LIKE 'migration_result_%_no_delete')",
                [],
                |row| row.get(0),
            )
            .expect("count WP-006 immutability triggers");
        assert_eq!(immutable_trigger_count, 20);

        let snapshot_columns = writer
            .connection
            .prepare("PRAGMA table_info(reconciliation_snapshots)")
            .expect("snapshot columns")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("snapshot column rows")
            .collect::<std::result::Result<HashSet<_>, _>>()
            .expect("collect snapshot columns");
        for required in [
            "accepted_row_set_hash",
            "active_decision_set_hash",
            "active_overlay_set_hash",
            "effective_value_set_hash",
            "qualifying_reevaluation_set_hash",
            "category_counts_hash",
            "row_set_hash",
            "snapshot_hash",
        ] {
            assert!(
                snapshot_columns.contains(required),
                "missing snapshot authority column {required}"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn exact_v6_upgrade_preserves_data_and_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("create current workspace");
            create_job(
                &writer.connection,
                &NewJob {
                    id: "preserved-v6-job",
                    source_name: "source.zip",
                    source_path: "/source.zip",
                    source_size: Some(10),
                    policy_json: "{}",
                },
            )
            .expect("create preserved job");
            remove_wp006_schema_for_v6_fixture(&writer.connection);
        }

        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("upgrade exact v6 schema");
            let version: i64 = writer
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("schema version");
            assert_eq!(version, SCHEMA_VERSION);
            let preserved: String = writer
                .connection
                .query_row(
                    "SELECT id FROM jobs WHERE id = 'preserved-v6-job'",
                    [],
                    |row| row.get(0),
                )
                .expect("preserved v6 row");
            assert_eq!(preserved, "preserved-v6-job");

            let policy_hash = "a".repeat(64);
            let provenance_hash = "b".repeat(64);
            writer
                .connection
                .execute(
                    "INSERT INTO reconciliation_policy_documents (
                         policy_identity, policy_version, schema_version,
                         canonicalization_version, engine_version,
                         canonical_policy_json, policy_hash,
                         provenance_json, provenance_hash,
                         registered_by, registration_reason, registered_at
                     ) VALUES (
                         'migration-stream/reconciliation-approval-policy/v1',
                         '1', 1, 'canonical-json/v1', 'reconciliation-engine/v1',
                         '{}', ?1, '{}', ?2, 'fixture-owner',
                         'migration fixture', '2026-07-22T00:00:00.000Z'
                     )",
                    params![policy_hash, provenance_hash],
                )
                .expect("insert v7 policy row");
            let activation_hash = "c".repeat(64);
            let request_hash = "d".repeat(64);
            writer
                .connection
                .execute(
                    "INSERT INTO reconciliation_policy_activations (
                         activation_id, schema_version,
                         policy_identity, policy_version, policy_hash,
                         predecessor_activation_id, predecessor_generation,
                         predecessor_activation_hash, observed_generation, generation,
                         actor, reason, idempotency_key, request_hash,
                         canonical_activation_json, activation_hash, activated_at
                     ) VALUES (
                         'activation-v1', 1,
                         'migration-stream/reconciliation-approval-policy/v1',
                         '1', ?1, NULL, 0, NULL, 0, 1,
                         'fixture-owner', 'activate fixture policy',
                         'activation-idempotency', ?2, '{}', ?3,
                         '2026-07-22T00:00:01.000Z'
                     )",
                    params![policy_hash, request_hash, activation_hash],
                )
                .expect("insert v7 activation row");
            let immutable_error = writer
                .connection
                .execute(
                    "UPDATE reconciliation_policy_activations
                     SET reason = 'mutated' WHERE activation_id = 'activation-v1'",
                    [],
                )
                .expect_err("activation history must be append-only");
            assert!(immutable_error.to_string().contains("immutable"));
        }

        let before_reopen = {
            let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
                .expect("inspect first upgrade");
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE name LIKE 'reconciliation_%'
                        OR name LIKE 'migration_result_%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count WP-006 objects")
        };
        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("idempotent reopen");
            let after_reopen: i64 = writer
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE name LIKE 'reconciliation_%'
                        OR name LIKE 'migration_result_%'",
                    [],
                    |row| row.get(0),
                )
                .expect("recount WP-006 objects");
            let activation_count: i64 = writer
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM reconciliation_policy_activations",
                    [],
                    |row| row.get(0),
                )
                .expect("preserved activation");
            assert_eq!(after_reopen, before_reopen);
            assert_eq!(activation_count, 1);
        }
    }

    #[test]
    fn newer_schema_is_rejected_without_mutation() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let database_path = temporary.path().join(DATABASE_FILENAME);
        Connection::open(&database_path)
            .expect("create newer database")
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("mark newer database");

        let Err(error) = WorkspaceWriter::open(temporary.path()) else {
            panic!("newer schema must fail");
        };
        assert!(
            error
                .to_string()
                .contains(&format!("is newer than supported version {SCHEMA_VERSION}"))
        );
        let connection = Connection::open(&database_path).expect("reopen newer database");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("newer version remains");
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert!(!schema_object_exists(
            &connection,
            "table",
            "reconciliation_policy_documents"
        ));
    }

    #[test]
    fn explicit_v10_adoption_migrates_reopens_and_passes_global_integrity() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer =
                WorkspaceWriter::open(temporary.path()).expect("create current legacy workspace");
            remove_compared_validation_schema_for_v10_fixture(&writer.connection);
        }
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("legacy workspace service");
        let status = service.inspect().expect("inspect legacy v10 workspace");
        assert_eq!(
            status.state,
            crate::object_workspace::ObjectWorkspaceState::AdoptRequired
        );
        let command = crate::object_workspace::EstablishObjectWorkspaceCommand {
            schema_version: crate::object_workspace::ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA
                .to_owned(),
            expected_state_hash: status.state_hash,
            display_name: "Adopted v10 workspace".to_owned(),
            migration_object_name_or_scope: "Synthetic migration object".to_owned(),
            creator_attribution: "Migration test operator".to_owned(),
            idempotency_key: "adopt-v10-workspace-0001".to_owned(),
        };
        let adopted = service.adopt(&command).expect("adopt and migrate v10");
        assert_eq!(adopted.established_by, "ADOPTED");

        let reopened = crate::object_workspace::ObjectWorkspaceService::new(service.root())
            .expect("reopen adopted service");
        assert_eq!(
            reopened.require_active().expect("active adopted identity"),
            adopted
        );
        let lock = WorkspaceLock::acquire(reopened.root()).expect("lock adopted workspace");
        let writer = lock.open_writer().expect("open strict adopted writer");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("adopted schema version");
        assert_eq!(version, SCHEMA_VERSION);
        drop(writer);
        drop(lock);
        let report = integrity_check(reopened.root()).expect("adopted global integrity");
        assert!(report.ok, "{:?}", report.issues);
        assert_eq!(report.checked_object_workspaces, 1);
    }

    #[test]
    fn active_newer_schema_is_rejected_before_writer_side_effects() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        crate::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Future-schema boundary",
            "Synthetic migration object",
            "Migration test operator",
        )
        .expect("initialize active workspace");
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("active workspace service");
        let database_path = service.root().join(DATABASE_FILENAME);
        let connection = Connection::open(&database_path).expect("open active database");
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("mark active database newer");
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint future marker");
        drop(connection);
        let database_before = fs::read(&database_path).expect("future database bytes");
        let wal_path = service.root().join(format!("{DATABASE_FILENAME}-wal"));
        let wal_before = fs::read(&wal_path).ok();

        let lock = WorkspaceLock::acquire(service.root()).expect("acquire active workspace lock");
        let Err(error) = lock.open_writer() else {
            panic!("active newer schema must fail before writer configuration");
        };
        assert!(
            error
                .to_string()
                .contains(&format!("is newer than supported version {SCHEMA_VERSION}"))
        );
        drop(lock);
        assert_eq!(
            fs::read(&database_path).expect("database after rejected writer"),
            database_before
        );
        assert_eq!(fs::read(&wal_path).ok(), wal_before);
        let connection = open_read_only(&database_path).expect("reopen future database");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("future version remains");
        assert_eq!(version, SCHEMA_VERSION + 1);
    }

    #[cfg(unix)]
    #[test]
    fn commit_hook_vetoes_replaced_lock_name_without_committing_rows() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        crate::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Writer lock boundary",
            "Synthetic migration object",
            "Migration test operator",
        )
        .expect("initialize active workspace");
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("active workspace service");
        let lock = WorkspaceLock::acquire(service.root()).expect("acquire workspace lock");
        let writer = lock.open_writer().expect("open strict writer");
        let transaction = writer
            .connection
            .unchecked_transaction()
            .expect("begin guarded transaction");
        transaction
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'writer-lock-veto-job', 'SPOOLING', 'synthetic.zip',
                     '/synthetic.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("insert uncommitted row");
        let lock_path = service.root().join(LOCK_FILENAME);
        let displaced_lock = service.root().join(".migration-stream.lock.displaced");
        fs::rename(&lock_path, &displaced_lock).expect("displace active lock name");
        File::create(&lock_path).expect("install replacement lock name");

        assert!(
            transaction.commit().is_err(),
            "commit hook must veto a transaction after lock identity replacement"
        );
        drop(writer);
        drop(lock);
        fs::remove_file(&lock_path).expect("remove replacement lock");
        fs::rename(&displaced_lock, &lock_path).expect("restore original lock");
        let connection =
            open_read_only(&service.root().join(DATABASE_FILENAME)).expect("reopen database");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE id = 'writer-lock-veto-job'",
                [],
                |row| row.get(0),
            )
            .expect("count vetoed rows");
        assert_eq!(count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn commit_hook_vetoes_database_hardlink_alias_without_committing_rows() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        crate::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Writer database alias boundary",
            "Synthetic migration object",
            "Migration test operator",
        )
        .expect("initialize active workspace");
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("active workspace service");
        let lock = WorkspaceLock::acquire(service.root()).expect("acquire workspace lock");
        let writer = lock.open_writer().expect("open strict writer");
        let transaction = writer
            .connection
            .unchecked_transaction()
            .expect("begin guarded transaction");
        transaction
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'writer-database-alias-veto-job', 'SPOOLING',
                     'synthetic.zip', '/synthetic.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("insert uncommitted row");
        let database_path = service.root().join(DATABASE_FILENAME);
        let alias_path = service.root().join("state.sqlite3.untrusted-alias");
        fs::hard_link(&database_path, &alias_path).expect("add database hard-link alias");

        assert!(
            transaction.commit().is_err(),
            "commit hook must veto a transaction after a database alias appears"
        );
        fs::remove_file(&alias_path).expect("remove database alias");
        drop(writer);
        drop(lock);
        let connection = open_read_only(&database_path).expect("reopen database");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs
                 WHERE id = 'writer-database-alias-veto-job'",
                [],
                |row| row.get(0),
            )
            .expect("count vetoed rows");
        assert_eq!(count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn supplemental_commit_guard_vetoes_new_hardlink_alias_without_committing_rows() {
        use std::os::unix::fs::MetadataExt as _;

        let temporary = tempfile::tempdir().expect("temporary workspace");
        crate::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Supplemental writer boundary",
            "Synthetic migration object",
            "Migration test operator",
        )
        .expect("initialize active workspace");
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("active workspace service");
        let lock = WorkspaceLock::acquire(service.root()).expect("acquire workspace lock");
        let mut writer = lock.open_writer().expect("open strict writer");
        let retained = service.root().join("guarded-retained-package.zip");
        fs::write(&retained, b"synthetic retained evidence").expect("write guarded evidence");
        let retained_handle = File::open(&retained).expect("retain guarded evidence");
        writer
            .install_supplemental_commit_guard(move || {
                if retained_handle
                    .metadata()
                    .map_err(|error| {
                        KernelError::io(Path::new("guarded retained evidence"), error)
                    })?
                    .nlink()
                    == 1
                {
                    Ok(())
                } else {
                    Err(KernelError::IntegrityFailed(
                        "guarded retained evidence acquired a hard-link alias".to_owned(),
                    ))
                }
            })
            .expect("install supplemental guard");
        let transaction = writer
            .connection
            .unchecked_transaction()
            .expect("begin guarded transaction");
        transaction
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'writer-hardlink-veto-job', 'SPOOLING', 'synthetic.zip',
                     '/synthetic.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("insert uncommitted row");
        let external_alias = service.root().join("guarded-retained-package-alias.zip");
        fs::hard_link(&retained, &external_alias).expect("add adversarial alias before commit");

        assert!(
            transaction.commit().is_err(),
            "supplemental commit guard must veto a new hard-link alias"
        );
        fs::remove_file(&external_alias).expect("remove adversarial alias");
        drop(writer);
        drop(lock);
        let connection =
            open_read_only(&service.root().join(DATABASE_FILENAME)).expect("reopen database");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE id = 'writer-hardlink-veto-job'",
                [],
                |row| row.get(0),
            )
            .expect("count vetoed rows");
        assert_eq!(count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn commit_hook_vetoes_same_byte_descriptor_replacement() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        crate::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Writer descriptor boundary",
            "Synthetic migration object",
            "Migration test operator",
        )
        .expect("initialize active workspace");
        let service = crate::object_workspace::ObjectWorkspaceService::new(temporary.path())
            .expect("active workspace service");
        let lock = WorkspaceLock::acquire(service.root()).expect("acquire workspace lock");
        let writer = lock.open_writer().expect("open strict writer");
        let transaction = writer
            .connection
            .unchecked_transaction()
            .expect("begin guarded transaction");
        transaction
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'writer-descriptor-veto-job', 'SPOOLING', 'synthetic.zip',
                     '/synthetic.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("insert uncommitted row");
        let descriptor_path = service
            .root()
            .join(crate::object_workspace::OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
        let displaced_descriptor = service.root().join("workspace.json.displaced");
        let descriptor_bytes = fs::read(&descriptor_path).expect("read descriptor");
        fs::rename(&descriptor_path, &displaced_descriptor).expect("displace descriptor");
        fs::write(&descriptor_path, &descriptor_bytes).expect("install same-byte replacement");

        assert!(
            transaction.commit().is_err(),
            "commit hook must veto a transaction after descriptor identity replacement"
        );
        drop(writer);
        drop(lock);
        fs::remove_file(&descriptor_path).expect("remove replacement descriptor");
        fs::rename(&displaced_descriptor, &descriptor_path).expect("restore descriptor");
        let connection =
            open_read_only(&service.root().join(DATABASE_FILENAME)).expect("reopen database");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE id = 'writer-descriptor-veto-job'",
                [],
                |row| row.get(0),
            )
            .expect("count vetoed rows");
        assert_eq!(count, 0);
        let report = integrity_check(service.root()).expect("restored workspace integrity");
        assert!(report.ok, "{:?}", report.issues);
    }

    #[test]
    fn failed_v6_upgrade_rolls_back_every_wp006_object() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("create current workspace");
            create_job(
                &writer.connection,
                &NewJob {
                    id: "rollback-preserved-job",
                    source_name: "source.zip",
                    source_path: "/source.zip",
                    source_size: Some(10),
                    policy_json: "{}",
                },
            )
            .expect("create preserved rollback row");
            remove_wp006_schema_for_v6_fixture(&writer.connection);
            writer
                .connection
                .execute_batch("CREATE TABLE migration_result_candidates (sentinel TEXT);")
                .expect("create late migration collision");
        }

        let Err(error) = WorkspaceWriter::open(temporary.path()) else {
            panic!("migration must roll back");
        };
        assert!(error.to_string().contains("already exists"));
        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("inspect rolled-back database");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 6);
        assert!(schema_object_exists(
            &connection,
            "table",
            "migration_result_candidates"
        ));
        assert!(!schema_object_exists(
            &connection,
            "table",
            "reconciliation_policy_documents"
        ));
        let preserved: String = connection
            .query_row(
                "SELECT id FROM jobs WHERE id = 'rollback-preserved-job'",
                [],
                |row| row.get(0),
            )
            .expect("v6 row survives failed migration");
        assert_eq!(preserved, "rollback-preserved-job");
    }

    #[test]
    fn migration_from_v1_preserves_existing_jobs() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let database_path = temporary.path().join(DATABASE_FILENAME);
        {
            let connection = Connection::open(&database_path).expect("open v1 database");
            connection
                .execute_batch(
                    "CREATE TABLE jobs (
                         id TEXT PRIMARY KEY NOT NULL,
                         state TEXT NOT NULL,
                         source_name TEXT NOT NULL,
                         source_path TEXT NOT NULL,
                         source_hash TEXT,
                         source_size INTEGER,
                         stored_archive TEXT,
                         policy_json TEXT NOT NULL,
                         manifest_entries INTEGER NOT NULL DEFAULT 0,
                         csv_files INTEGER NOT NULL DEFAULT 0,
                         expanded_bytes INTEGER NOT NULL DEFAULT 0,
                         source_records INTEGER NOT NULL DEFAULT 0,
                         stored_records INTEGER NOT NULL DEFAULT 0,
                         pending_records INTEGER NOT NULL DEFAULT 0,
                         error_code TEXT,
                         error_message TEXT,
                         created_at TEXT NOT NULL,
                         updated_at TEXT NOT NULL,
                         completed_at TEXT,
                         elapsed_ms INTEGER,
                         peak_rss_bytes INTEGER,
                         declared_buffer_bytes INTEGER
                     );
                     INSERT INTO jobs (
                         id, state, source_name, source_path, policy_json,
                         created_at, updated_at
                     ) VALUES (
                         'preserved-job', 'SPOOLING', 'source.zip', '/source.zip',
                         '{}', '2026-07-21T00:00:00.000Z',
                         '2026-07-21T00:00:00.000Z'
                     );
                     PRAGMA user_version = 1;",
                )
                .expect("create v1 database");
        }

        let writer = WorkspaceWriter::open(temporary.path()).expect("migrate workspace");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);
        let preserved: String = writer
            .connection
            .query_row("SELECT id FROM jobs", [], |row| row.get(0))
            .expect("preserved job");
        assert_eq!(preserved, "preserved-job");
        let profiling_table: String = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'profiling_runs'",
                [],
                |row| row.get(0),
            )
            .expect("profiling table");
        assert_eq!(profiling_table, "profiling_runs");
        let cleanup_table: String = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name = 'partial_cleanups'",
                [],
                |row| row.get(0),
            )
            .expect("partial cleanup table");
        assert_eq!(cleanup_table, "partial_cleanups");
    }

    #[test]
    fn migration_from_v2_removes_only_the_redundant_source_record_index() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("create workspace");
            remove_wp006_schema_for_v6_fixture(&writer.connection);
            writer
                .connection
                .execute_batch(
                    "CREATE INDEX source_records_job
                         ON source_records(job_id, file_path, source_ordinal);
                     DROP TABLE correction_effectivations;
                     DROP TABLE issue_decisions;
                     DROP TABLE reevaluation_issues;
                     DROP TABLE reevaluation_events;
                     DROP TABLE reevaluation_runs;
                     DROP TABLE correction_overlays;
                     DROP TABLE decision_attempt_events;
                     DROP TABLE decision_attempts;
                     DROP INDEX validation_issues_wp005_scope;
                     DROP INDEX validation_runs_wp005_scope;
                     DROP TABLE validation_issues;
                     DROP TABLE validation_events;
                     DROP TABLE validation_runs;
                     DROP TABLE mapping_versions;
                     DROP TABLE mapping_validation_attempts;
                     DROP TABLE profile_lifecycle_events;
                     DROP TABLE profile_approvals;
                     DROP TABLE profile_versions;
                     DROP TABLE partial_cleanups;
                     PRAGMA user_version = 2;",
                )
                .expect("simulate v2 index");
        }

        let writer = WorkspaceWriter::open(temporary.path()).expect("migrate v2 workspace");
        let version: i64 = writer
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        let redundant_index: Option<String> = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND name = 'source_records_job'",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("query redundant index");
        let unique_index: String = writer
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND tbl_name = 'source_records'
                   AND sql IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("automatic uniqueness index");
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(redundant_index, None);
        assert_eq!(unique_index, "sqlite_autoindex_source_records_1");
    }

    #[test]
    fn recovery_marks_running_profiling_runs_interrupted() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        {
            let writer = WorkspaceWriter::open(temporary.path()).expect("open workspace");
            create_job(
                &writer.connection,
                &NewJob {
                    id: "profile-job",
                    source_name: "source.zip",
                    source_path: "/source.zip",
                    source_size: Some(10),
                    policy_json: "{}",
                },
            )
            .expect("create job");
            writer
                .connection
                .execute(
                    "UPDATE jobs
                     SET state = 'FAILED', error_code = 'TEST_SETUP',
                         error_message = 'terminal parent for recovery test',
                         completed_at = updated_at
                     WHERE id = 'profile-job'",
                    [],
                )
                .expect("make parent terminal");
            let hash = "a".repeat(64);
            writer
                .connection
                .execute(
                    "INSERT INTO profiling_runs (
                         id, identity_hash, job_id, source_hash, profiler_version,
                         policy_json, policy_hash, state, created_at, updated_at
                     ) VALUES (
                         'profile-run', ?1, 'profile-job', ?1, 'wp002-v1', '{}',
                         ?1, 'RUNNING', '2026-07-21T00:00:00.000Z',
                         '2026-07-21T00:00:00.000Z'
                     )",
                    [&hash],
                )
                .expect("create running profile");
            writer
                .connection
                .execute(
                    "INSERT INTO profiling_events (
                         run_id, state, occurred_at
                     ) VALUES (
                         'profile-run', 'RUNNING', '2026-07-21T00:00:00.000Z'
                     )",
                    [],
                )
                .expect("create profile event");
        }

        let report = recover_workspace(temporary.path()).expect("recover workspace");
        assert_eq!(report.recovered_jobs, 0);
        assert_eq!(report.recovered_profiling_runs, 1);
        let connection = open_read_only(&temporary.path().join(DATABASE_FILENAME))
            .expect("open recovered database");
        let (state, code): (String, Option<String>) = connection
            .query_row(
                "SELECT state, error_code FROM profiling_runs WHERE id = 'profile-run'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("recovered profile");
        assert_eq!(state, "INTERRUPTED");
        assert_eq!(code.as_deref(), Some("INTERRUPTED_RECOVERED"));
        let integrity = integrity_check(temporary.path()).expect("integrity");
        assert!(!integrity.ok);
        assert!(
            integrity
                .issues
                .iter()
                .any(|issue| issue.contains("matching completed intake evidence"))
        );
        assert_eq!(integrity.checked_profiling_runs, 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn integrity_verifies_complete_profiling_hash_and_child_counts() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/packages/valid.zip");
        let intake =
            crate::intake::ingest_package(&source, temporary.path(), &IntakePolicy::default())
                .expect("ingest profile source");
        let profiling_policy = crate::profiling::ProfilingPolicy::from_path(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("config/profiling-policy.json"),
        )
        .expect("load profiling policy");
        let profile =
            crate::profiling::run_profile(temporary.path(), &intake.job_id, &profiling_policy)
                .expect("profile completed intake");

        let integrity = integrity_check(temporary.path()).expect("integrity");
        assert!(integrity.ok, "{:?}", integrity.issues);
        assert_eq!(integrity.checked_profiling_runs, 1);

        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("open database for tamper");
        let original_samples: String = connection
            .query_row(
                "SELECT samples_json FROM profiling_columns
                 WHERE run_id = ?1 ORDER BY path, column_ordinal LIMIT 1",
                [&profile.run_id],
                |row| row.get(0),
            )
            .expect("read original samples");
        connection
            .execute(
                "UPDATE profiling_columns SET samples_json = '[]'
                 WHERE run_id = ?1 AND column_ordinal = 0",
                [&profile.run_id],
            )
            .expect("tamper profile child evidence");
        drop(connection);
        let integrity = integrity_check(temporary.path()).expect("child-tampered integrity");
        assert!(!integrity.ok);
        assert!(
            integrity
                .issues
                .iter()
                .any(|issue| issue.contains("differs from canonical result evidence"))
        );

        let connection = Connection::open(temporary.path().join(DATABASE_FILENAME))
            .expect("reopen database for result tamper");
        connection
            .execute(
                "UPDATE profiling_columns SET samples_json = ?1
                 WHERE run_id = ?2 AND column_ordinal = 0",
                params![original_samples, profile.run_id],
            )
            .expect("restore profile child evidence");
        connection
            .execute(
                "UPDATE profiling_runs SET result_json = '{\"tampered\":true}' WHERE id = ?1",
                [&profile.run_id],
            )
            .expect("tamper profile result");
        drop(connection);
        let integrity = integrity_check(temporary.path()).expect("result-tampered integrity");
        assert!(!integrity.ok);
        assert!(
            integrity
                .issues
                .iter()
                .any(|issue| issue.contains("result hash mismatch"))
        );
    }
}

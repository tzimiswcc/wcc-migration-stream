//! Safe, object-neutral ZIP/CSV package intake.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use csv::StringRecord;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    KernelError, Result,
    durability::{path_uses_tombstone_namespace, publish_new},
    policy::IntakePolicy,
    state::{
        CleanupNamespace, DATABASE_FILENAME, JobExecutionMetrics, JobState, NewJob,
        PACKAGES_DIRECTORY, WorkspaceWriter, create_job, fail_job, interrupt_job,
        record_job_metrics, record_spooled_archive, recover_locked, recover_namespace_partials,
        sqlite_integer, transition_job,
    },
};

const CRC_BUFFER_BYTES: usize = 8 * 1024;
const MAXIMUM_EVIDENCE_ISSUES: usize = 100;

/// Optional cooperative stop signals for a single ingestion attempt.
///
/// Cancellation is observed only at bounded spool checks and transaction-safe
/// CSV batch boundaries. `cancel_after_batches` is deterministic test and
/// benchmark instrumentation; a value of zero cancels before the first batch.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct IngestControl {
    pub cancel_file: Option<PathBuf>,
    pub cancel_after_batches: Option<u64>,
}

impl IngestControl {
    fn check_external(&self) -> Result<()> {
        let Some(path) = &self.cancel_file else {
            return Ok(());
        };
        match fs::symlink_metadata(path) {
            Ok(_) => Err(KernelError::Cancelled(
                "cancellation signal file was observed".to_owned(),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(KernelError::io(path, error)),
        }
    }

    fn check_batch_boundary(&self, committed_batches: u64) -> Result<()> {
        self.check_external()?;
        if self
            .cancel_after_batches
            .is_some_and(|configured| committed_batches >= configured)
        {
            return Err(KernelError::Cancelled(format!(
                "configured stop reached after {committed_batches} committed batches"
            )));
        }
        Ok(())
    }
}

/// One validated central-directory entry, sorted by normalized path.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArchiveEntryManifest {
    pub path: String,
    pub is_directory: bool,
    pub is_csv: bool,
    pub compressed_size: u64,
    pub expanded_size: u64,
    pub compression_ratio: f64,
    pub crc32: u32,
}

/// Fully validated archive manifest produced before any row is ingested.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArchiveManifest {
    pub entries: Vec<ArchiveEntryManifest>,
    pub csv_paths: Vec<String>,
    pub total_compressed_bytes: u64,
    pub total_expanded_bytes: u64,
    pub maximum_observed_compression_ratio: f64,
}

/// Durable intake completion report suitable for deterministic JSON output.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IngestReport {
    pub job_id: String,
    pub source_name: String,
    pub source_hash: String,
    pub source_size: u64,
    pub stored_archive: PathBuf,
    pub manifest: ArchiveManifest,
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
    pub declared_buffer_bytes: u64,
    #[serde(default)]
    pub peak_database_bytes: Option<u64>,
    #[serde(default)]
    pub peak_wal_bytes: Option<u64>,
    #[serde(default)]
    pub peak_shm_bytes: Option<u64>,
}

/// Recomputed immutable-package evidence for workspace integrity checks.
pub(crate) struct PackageEvidenceVerification {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) readonly: bool,
    pub(crate) issues: Vec<String>,
}

/// Recomputed file-level evidence for a non-complete stored package.
pub(crate) struct PackageFileVerification {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) readonly: bool,
}

struct PendingReport {
    source_hash: String,
    source_size: u64,
    stored_archive: PathBuf,
    manifest: ArchiveManifest,
    source_records: u64,
    stored_records: u64,
    pending_records: u64,
    committed_batches: u64,
}

struct Metrics {
    started: Instant,
    peak_rss_bytes: Option<u64>,
    declared_buffer_bytes: u64,
    database_path: PathBuf,
    peak_database_bytes: Option<u64>,
    peak_wal_bytes: Option<u64>,
    peak_shm_bytes: Option<u64>,
}

impl Metrics {
    fn new(policy: &IntakePolicy, workspace: &Path) -> Self {
        let parser_buffer = policy.maximum_csv_record_bytes.clamp(1, 64 * 1024);
        // A JSON string can require six output bytes for one control byte.
        let maximum_record_memory = policy.maximum_csv_record_bytes.saturating_mul(7);
        let maximum_column_index_memory = policy
            .maximum_csv_columns
            .saturating_mul(std::mem::size_of::<usize>());
        let declared_buffer_bytes = u64::try_from(policy.spool_buffer_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(maximum_record_memory).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(maximum_column_index_memory).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(parser_buffer).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(CRC_BUFFER_BYTES).unwrap_or(u64::MAX));
        let mut metrics = Self {
            started: Instant::now(),
            peak_rss_bytes: None,
            declared_buffer_bytes,
            database_path: workspace.join(DATABASE_FILENAME),
            peak_database_bytes: None,
            peak_wal_bytes: None,
            peak_shm_bytes: None,
        };
        metrics.sample();
        metrics
    }

    fn sample(&mut self) {
        if let Some(bytes) = app_memory_usage_fetcher::get_memory_usage_bytes() {
            self.peak_rss_bytes = Some(
                self.peak_rss_bytes
                    .map_or(bytes.get(), |current| current.max(bytes.get())),
            );
        }
        sample_peak_file_size(&self.database_path, &mut self.peak_database_bytes);
        sample_peak_file_size(
            &sqlite_sidecar_path(&self.database_path, "-wal"),
            &mut self.peak_wal_bytes,
        );
        sample_peak_file_size(
            &sqlite_sidecar_path(&self.database_path, "-shm"),
            &mut self.peak_shm_bytes,
        );
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn execution_metrics(&self) -> JobExecutionMetrics {
        JobExecutionMetrics {
            elapsed_ms: self.elapsed_ms(),
            peak_rss_bytes: self.peak_rss_bytes,
            declared_buffer_bytes: self.declared_buffer_bytes,
            peak_database_bytes: self.peak_database_bytes,
            peak_wal_bytes: self.peak_wal_bytes,
            peak_shm_bytes: self.peak_shm_bytes,
        }
    }
}

fn sample_peak_file_size(path: &Path, peak: &mut Option<u64>) {
    if let Ok(metadata) = fs::metadata(path) {
        *peak = Some(peak.map_or(metadata.len(), |current| current.max(metadata.len())));
    }
}

fn sqlite_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = database_path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

/// Spool, inspect, ingest, reconcile, and durably complete one ZIP package.
///
/// # Errors
///
/// Returns a typed [`KernelError`] for lock, filesystem, archive, CSV, `SQLite`,
/// or reconciliation failures. Once the durable job exists, failures are
/// recorded before this function returns.
pub fn ingest_package(
    source: &Path,
    workspace: &Path,
    policy: &IntakePolicy,
) -> Result<IngestReport> {
    ingest_package_with_control(source, workspace, policy, &IngestControl::default())
}

/// Ingest one ZIP package with cooperative cancellation at bounded safe points.
///
/// A cancelled job is durably classified as `INTERRUPTED` with reconciled
/// committed counters. Rows from the in-flight transaction are rolled back.
///
/// # Errors
///
/// Returns [`KernelError::Cancelled`] when a configured stop signal is
/// observed, or the same typed validation and persistence errors as
/// [`ingest_package`].
pub fn ingest_package_with_control(
    source: &Path,
    workspace: &Path,
    policy: &IntakePolicy,
    control: &IngestControl,
) -> Result<IngestReport> {
    IntakePolicy::validate(policy)?;
    reject_tombstone_source(source)?;
    let source_file = open_regular_nofollow(source)?;
    let source_size = source_file
        .metadata()
        .map_err(|error| KernelError::io(source, error))?
        .len();
    enforce_source_size(source_size, policy.maximum_source_bytes)?;
    let mut metrics = Metrics::new(policy, workspace);
    let mut writer = WorkspaceWriter::open(workspace)?;
    let packages = writer.root.join(PACKAGES_DIRECTORY);
    match fs::symlink_metadata(&packages) {
        Ok(_) => ensure_real_directory(&packages, "package")?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(KernelError::io(&packages, error)),
    }
    let recovery = recover_locked(&writer)?;
    if !recovery.cleanup_pending.is_empty() {
        return Err(KernelError::Reconciliation(format!(
            "implicit recovery retained {} inert tombstone(s): {}",
            recovery.cleanup_pending.len(),
            recovery
                .cleanup_pending
                .iter()
                .map(|pending| pending.diagnostic.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    metrics.sample();

    let job_id = Uuid::new_v4().to_string();
    let source_name = source
        .file_name()
        .map_or_else(|| source.as_os_str(), |name| name)
        .to_string_lossy()
        .into_owned();
    let source_path = source.to_string_lossy().into_owned();
    let policy_json = serde_json::to_string(policy)?;
    create_job(
        &writer.connection,
        &NewJob {
            id: &job_id,
            source_name: &source_name,
            source_path: &source_path,
            source_size: Some(source_size),
            policy_json: &policy_json,
        },
    )?;

    metrics.sample();
    let pending = run_ingest(
        source,
        &source_file,
        source_size,
        &job_id,
        policy,
        &mut writer,
        &mut metrics,
        control,
    );
    metrics.sample();
    let pending = pending.and_then(|pending| {
        control.check_batch_boundary(pending.committed_batches)?;
        Ok(pending)
    });
    let execution_metrics = metrics.execution_metrics();
    match pending {
        Ok(pending) => {
            record_job_metrics(&writer.connection, &job_id, &execution_metrics)?;
            if let Err(error) = transition_job(&writer.connection, &job_id, JobState::Complete) {
                fail_job(&writer.connection, &job_id, &error, &execution_metrics)?;
                return Err(error);
            }
            Ok(IngestReport {
                job_id,
                source_name,
                source_hash: pending.source_hash,
                source_size: pending.source_size,
                stored_archive: pending.stored_archive,
                manifest: pending.manifest,
                source_records: pending.source_records,
                stored_records: pending.stored_records,
                pending_records: pending.pending_records,
                elapsed_ms: execution_metrics.elapsed_ms,
                peak_rss_bytes: metrics.peak_rss_bytes,
                declared_buffer_bytes: metrics.declared_buffer_bytes,
                peak_database_bytes: metrics.peak_database_bytes,
                peak_wal_bytes: metrics.peak_wal_bytes,
                peak_shm_bytes: metrics.peak_shm_bytes,
            })
        }
        Err(error) => {
            if matches!(&error, KernelError::Cancelled(_)) {
                interrupt_job(&writer.connection, &job_id, &error, &execution_metrics)?;
            } else {
                fail_job(&writer.connection, &job_id, &error, &execution_metrics)?;
            }
            Err(error)
        }
    }
}

/// Validate paths, types, limits, encryption, structure, and every entry CRC.
///
/// # Errors
///
/// Returns a typed archive rejection or an I/O/policy error. This function is
/// read-only and never ingests rows.
#[allow(clippy::too_many_lines)]
pub fn inspect_archive(archive_path: &Path, policy: &IntakePolicy) -> Result<ArchiveManifest> {
    IntakePolicy::validate(policy)?;
    reject_tombstone_source(archive_path)?;
    let file = open_regular_nofollow(archive_path)?;
    let source_size = file
        .metadata()
        .map_err(|error| KernelError::io(archive_path, error))?
        .len();
    enforce_source_size(source_size, policy.maximum_source_bytes)?;
    inspect_archive_handle(&file, archive_path, policy)
}

/// Re-read a stored package under its persisted policy and compare every
/// manifest/header/source-row fact with durable `SQLite` evidence.
///
/// Comparison is ordered and streaming for CSV rows. Reported issues are
/// bounded so corrupted database state cannot amplify verification memory.
pub(crate) fn verify_ingested_package(
    connection: &Connection,
    job_id: &str,
    archive_path: &Path,
    policy: &IntakePolicy,
) -> Result<PackageEvidenceVerification> {
    IntakePolicy::validate(policy)?;
    ensure_real_directory(
        archive_path.parent().ok_or_else(|| {
            KernelError::ArchiveIntegrity("stored package has no parent directory".to_owned())
        })?,
        "package",
    )?;
    let file = open_regular_nofollow(archive_path)?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(archive_path, error))?;
    enforce_source_size(metadata.len(), policy.maximum_source_bytes)?;
    let initially_readonly = metadata.permissions().readonly();
    let (initial_hash, initial_size) =
        hash_file_handle(&file, archive_path, policy.spool_buffer_bytes)?;
    let manifest = inspect_archive_handle(&file, archive_path, policy)?;
    let mut issues = compare_manifest_evidence(connection, job_id, &manifest)?;
    compare_csv_evidence(
        connection,
        job_id,
        &file,
        archive_path,
        &manifest,
        policy,
        &mut issues,
    )?;
    let (sha256, size) = hash_file_handle(&file, archive_path, policy.spool_buffer_bytes)?;
    verify_named_file_identity(archive_path, &file)?;
    ensure_real_directory(
        archive_path.parent().ok_or_else(|| {
            KernelError::ArchiveIntegrity("stored package has no parent directory".to_owned())
        })?,
        "package",
    )?;
    let readonly = initially_readonly && file_is_readonly(&file, archive_path)?;
    if sha256 != initial_hash || size != initial_size {
        push_evidence_issue(
            &mut issues,
            format!(
                "package changed during evidence verification: initial {initial_hash}/{initial_size}, final {sha256}/{size}"
            ),
        );
    }
    Ok(PackageEvidenceVerification {
        sha256,
        size,
        readonly,
        issues,
    })
}

/// Hash one stored package through a retained no-follow regular-file handle.
pub(crate) fn inspect_stored_package_file(
    archive_path: &Path,
    policy: &IntakePolicy,
) -> Result<PackageFileVerification> {
    IntakePolicy::validate(policy)?;
    ensure_real_directory(
        archive_path.parent().ok_or_else(|| {
            KernelError::ArchiveIntegrity("stored package has no parent directory".to_owned())
        })?,
        "package",
    )?;
    let file = open_regular_nofollow(archive_path)?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(archive_path, error))?;
    enforce_source_size(metadata.len(), policy.maximum_source_bytes)?;
    let initially_readonly = metadata.permissions().readonly();
    let (sha256, size) = hash_file_handle(&file, archive_path, policy.spool_buffer_bytes)?;
    verify_named_file_identity(archive_path, &file)?;
    ensure_real_directory(
        archive_path.parent().ok_or_else(|| {
            KernelError::ArchiveIntegrity("stored package has no parent directory".to_owned())
        })?,
        "package",
    )?;
    let readonly = initially_readonly && file_is_readonly(&file, archive_path)?;
    Ok(PackageFileVerification {
        sha256,
        size,
        readonly,
    })
}

fn compare_manifest_evidence(
    connection: &Connection,
    job_id: &str,
    manifest: &ArchiveManifest,
) -> Result<Vec<String>> {
    let mut issues = Vec::new();
    let mut statement = connection.prepare(
        "SELECT path, is_directory, is_csv, compressed_size, expanded_size, crc32
         FROM archive_entries WHERE job_id = ?1 ORDER BY path",
    )?;
    let mut rows = statement.query([job_id])?;
    let mut index = 0_usize;
    while let Some(row) = rows.next()? {
        let expected = (
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        );
        let Some(actual) = manifest.entries.get(index) else {
            push_evidence_issue(
                &mut issues,
                format!(
                    "job {job_id} has an extra durable manifest row for {:?}",
                    expected.0
                ),
            );
            break;
        };
        let matches = expected.0 == actual.path
            && expected.1 == i64::from(actual.is_directory)
            && expected.2 == i64::from(actual.is_csv)
            && u64::try_from(expected.3).ok() == Some(actual.compressed_size)
            && u64::try_from(expected.4).ok() == Some(actual.expanded_size)
            && u32::try_from(expected.5).ok() == Some(actual.crc32);
        if !matches {
            push_evidence_issue(
                &mut issues,
                format!(
                    "job {job_id} manifest row {} does not match package entry {:?}",
                    index.saturating_add(1),
                    actual.path
                ),
            );
        }
        index = index.saturating_add(1);
    }
    for actual in manifest.entries.iter().skip(index) {
        push_evidence_issue(
            &mut issues,
            format!(
                "job {job_id} package entry {:?} is missing from durable manifest evidence",
                actual.path
            ),
        );
    }
    Ok(issues)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn compare_csv_evidence(
    connection: &Connection,
    job_id: &str,
    archive_file: &File,
    archive_path: &Path,
    manifest: &ArchiveManifest,
    policy: &IntakePolicy,
    issues: &mut Vec<String>,
) -> Result<()> {
    let input = archive_file
        .try_clone()
        .map_err(|error| KernelError::io(archive_path, error))?;
    let mut archive = ZipArchive::new(input).map_err(|error| archive_integrity(&error))?;
    let mut actual_csv_expanded = 0_u64;
    let mut actual_csv_compressed = 0_u64;
    let mut total_records = 0_u64;
    for path in &manifest.csv_paths {
        let entry = archive
            .by_name(path)
            .map_err(|error| archive_integrity(&error))?;
        let expected_entry = manifest
            .entries
            .iter()
            .find(|candidate| candidate.path == *path)
            .ok_or_else(|| {
                KernelError::Reconciliation(format!(
                    "CSV path {path:?} is absent from its inspected manifest"
                ))
            })?;
        let validating = CsvLimitReader::new(
            entry,
            policy,
            actual_csv_expanded,
            actual_csv_compressed,
            expected_entry.compressed_size,
        );
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .buffer_capacity(policy.maximum_csv_record_bytes.clamp(1, 64 * 1024))
            .from_reader(validating);
        let header = match reader.headers().cloned() {
            Ok(header) => header,
            Err(error) => return Err(map_csv_error(path, &reader, &error)),
        };
        validate_string_record(path, &header, policy)?;
        let actual_header = record_json(&header)?;
        let durable_header: Option<Option<String>> = connection
            .query_row(
                "SELECT header_json FROM csv_files WHERE job_id = ?1 AND path = ?2",
                params![job_id, path],
                |row| row.get(0),
            )
            .optional()?;
        if durable_header.as_ref().and_then(Option::as_deref) != Some(actual_header.as_str()) {
            push_evidence_issue(
                issues,
                format!("job {job_id} CSV header evidence differs for {path:?}"),
            );
        }

        let mut statement = connection.prepare(
            "SELECT source_ordinal, line_start, line_end, fields_json
             FROM source_records
             WHERE job_id = ?1 AND file_path = ?2
             ORDER BY source_ordinal",
        )?;
        let mut rows = statement.query(params![job_id, path])?;
        let mut ordinal = 0_u64;
        loop {
            let before = reader.position().clone();
            let mut record = StringRecord::new();
            match reader.read_record(&mut record) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => return Err(map_csv_error(path, &reader, &error)),
            }
            validate_string_record(path, &record, policy)?;
            total_records =
                total_records
                    .checked_add(1)
                    .ok_or(KernelError::CsvRecordCountLimit {
                        actual: u64::MAX,
                        limit: policy.maximum_csv_records,
                    })?;
            if total_records > policy.maximum_csv_records {
                return Err(KernelError::CsvRecordCountLimit {
                    actual: total_records,
                    limit: policy.maximum_csv_records,
                });
            }
            ordinal = ordinal.saturating_add(1);
            let line_start = record
                .position()
                .map_or_else(|| before.line(), csv::Position::line);
            let line_end = line_start.saturating_add(record_embedded_line_breaks(&record));
            let fields_json = record_json(&record)?;
            let Some(row) = rows.next()? else {
                push_evidence_issue(
                    issues,
                    format!(
                        "job {job_id} CSV record {ordinal} in {path:?} is missing from durable evidence"
                    ),
                );
                continue;
            };
            let durable_ordinal = row.get::<_, i64>(0)?;
            let durable_line_start = row.get::<_, i64>(1)?;
            let durable_line_end = row.get::<_, i64>(2)?;
            let durable_fields = row.get::<_, String>(3)?;
            if u64::try_from(durable_ordinal).ok() != Some(ordinal)
                || u64::try_from(durable_line_start).ok() != Some(line_start)
                || u64::try_from(durable_line_end).ok() != Some(line_end)
                || durable_fields != fields_json
            {
                push_evidence_issue(
                    issues,
                    format!("job {job_id} CSV record {ordinal} evidence differs for {path:?}"),
                );
            }
        }
        if rows.next()?.is_some() {
            push_evidence_issue(
                issues,
                format!("job {job_id} has extra durable CSV row evidence for {path:?}"),
            );
        }
        drop(rows);
        drop(statement);
        let validating = reader.into_inner();
        let entry_bytes = validating.total_bytes();
        if entry_bytes != expected_entry.expanded_size {
            push_evidence_issue(
                issues,
                format!("job {job_id} CSV expanded size differs for {path:?}"),
            );
        }
        actual_csv_expanded = actual_csv_expanded.saturating_add(entry_bytes);
        actual_csv_compressed =
            actual_csv_compressed.saturating_add(expected_entry.compressed_size);
    }
    Ok(())
}

fn push_evidence_issue(issues: &mut Vec<String>, issue: String) {
    if issues.len() < MAXIMUM_EVIDENCE_ISSUES {
        issues.push(issue);
    }
}

#[allow(clippy::too_many_lines)]
fn inspect_archive_handle(
    file: &File,
    archive_path: &Path,
    policy: &IntakePolicy,
) -> Result<ArchiveManifest> {
    let input = file
        .try_clone()
        .map_err(|error| KernelError::io(archive_path, error))?;
    let mut archive = ZipArchive::new(input).map_err(|error| archive_integrity(&error))?;
    let count = archive.len();
    if count > policy.maximum_entries {
        return Err(KernelError::ArchiveEntryLimit {
            actual: count,
            limit: policy.maximum_entries,
        });
    }

    let mut entries = Vec::with_capacity(count);
    let mut seen = HashSet::with_capacity(count);
    let mut total_compressed = 0_u64;
    let mut declared_total_expanded = 0_u64;
    let mut total_expanded = 0_u64;
    let mut verified_expanded = 0_u64;
    let mut maximum_ratio = 1.0_f64;
    for index in 0..count {
        let raw_entry = archive
            .by_index_raw(index)
            .map_err(|error| archive_integrity(&error))?;
        let display_name = String::from_utf8_lossy(raw_entry.name_raw()).into_owned();
        if raw_entry.encrypted() {
            return Err(KernelError::ArchiveEncrypted(display_name));
        }
        let path = validate_archive_path(raw_entry.name_raw(), policy.maximum_entry_path_bytes)?;
        let is_directory = raw_entry.is_dir();
        if !seen.insert(path.clone()) {
            return Err(KernelError::ArchiveDuplicatePath(path));
        }
        validate_entry_type(&raw_entry, &path, is_directory)?;
        let compressed_size = raw_entry.compressed_size();
        let expanded_size = raw_entry.size();
        let crc32 = raw_entry.crc32();
        let ratio = compression_ratio(expanded_size, compressed_size);
        drop(raw_entry);

        total_compressed = total_compressed.checked_add(compressed_size).ok_or(
            KernelError::ArchiveExpandedSize {
                actual: u64::MAX,
                limit: policy.maximum_expanded_bytes,
            },
        )?;
        declared_total_expanded = declared_total_expanded.checked_add(expanded_size).ok_or(
            KernelError::ArchiveExpandedSize {
                actual: u64::MAX,
                limit: policy.maximum_expanded_bytes,
            },
        )?;
        if declared_total_expanded > policy.maximum_expanded_bytes {
            return Err(KernelError::ArchiveExpandedSize {
                actual: declared_total_expanded,
                limit: policy.maximum_expanded_bytes,
            });
        }
        if ratio > policy.maximum_compression_ratio {
            return Err(KernelError::ArchiveCompressionRatio {
                actual: ratio,
                limit: policy.maximum_compression_ratio,
            });
        }
        maximum_ratio = maximum_ratio.max(ratio);

        let mut checked_entry = archive
            .by_index(index)
            .map_err(|error| archive_integrity(&error))?;
        let copied = copy_to_sink(
            &mut checked_entry,
            verified_expanded,
            total_compressed.saturating_sub(compressed_size),
            compressed_size,
            policy.maximum_expanded_bytes,
            policy.maximum_compression_ratio,
        )?;
        verified_expanded =
            verified_expanded
                .checked_add(copied)
                .ok_or(KernelError::ArchiveExpandedSize {
                    actual: u64::MAX,
                    limit: policy.maximum_expanded_bytes,
                })?;
        if copied != expanded_size {
            return Err(KernelError::ArchiveIntegrity(format!(
                "entry {path:?} yielded {copied} bytes; central directory declares {expanded_size}"
            )));
        }
        total_expanded = verified_expanded;
        let actual_ratio = compression_ratio(copied, compressed_size);
        if actual_ratio > policy.maximum_compression_ratio {
            return Err(KernelError::ArchiveCompressionRatio {
                actual: actual_ratio,
                limit: policy.maximum_compression_ratio,
            });
        }
        maximum_ratio = maximum_ratio.max(actual_ratio);
        drop(checked_entry);

        let is_csv = !is_directory && has_csv_extension(&path);
        entries.push(ArchiveEntryManifest {
            path,
            is_directory,
            is_csv,
            compressed_size,
            expanded_size: copied,
            compression_ratio: actual_ratio,
            crc32,
        });
    }

    let aggregate_ratio = compression_ratio(total_expanded, total_compressed);
    if aggregate_ratio > policy.maximum_compression_ratio {
        return Err(KernelError::ArchiveCompressionRatio {
            actual: aggregate_ratio,
            limit: policy.maximum_compression_ratio,
        });
    }
    maximum_ratio = maximum_ratio.max(aggregate_ratio);
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let csv_paths: Vec<String> = entries
        .iter()
        .filter(|entry| entry.is_csv)
        .map(|entry| entry.path.clone())
        .collect();
    if csv_paths.is_empty() {
        return Err(KernelError::ArchiveMissingCsv);
    }
    Ok(ArchiveManifest {
        entries,
        csv_paths,
        total_compressed_bytes: total_compressed,
        total_expanded_bytes: total_expanded,
        maximum_observed_compression_ratio: maximum_ratio,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_ingest(
    source: &Path,
    source_file: &File,
    source_size: u64,
    job_id: &str,
    policy: &IntakePolicy,
    writer: &mut WorkspaceWriter,
    metrics: &mut Metrics,
    control: &IngestControl,
) -> Result<PendingReport> {
    control.check_external()?;
    let required = source_size.saturating_add(policy.minimum_free_space_bytes);
    let available =
        fs2::available_space(&writer.root).map_err(|error| KernelError::io(&writer.root, error))?;
    if available < required {
        return Err(KernelError::InsufficientSpace {
            required,
            available,
        });
    }

    let spooled = spool_source(
        source,
        source_file,
        source_size,
        job_id,
        policy,
        writer,
        metrics,
        control,
    )?;
    let relative_archive = format!("{PACKAGES_DIRECTORY}/{}.zip", spooled.hash);
    record_spooled_archive(
        &writer.connection,
        job_id,
        &spooled.hash,
        spooled.size,
        &relative_archive,
    )?;
    transition_job(&writer.connection, job_id, JobState::Inspecting)?;
    let manifest = inspect_archive_handle(&spooled.file, &spooled.path, policy)?;
    metrics.sample();
    control.check_external()?;
    persist_manifest(&writer.connection, job_id, &manifest)?;
    metrics.sample();
    transition_job(&writer.connection, job_id, JobState::Ingesting)?;
    let committed_batches = ingest_csv_files(
        &mut writer.connection,
        job_id,
        &spooled.file,
        &spooled.path,
        &manifest,
        policy,
        metrics,
        control,
    )?;
    control.check_batch_boundary(committed_batches)?;
    let (final_hash, final_size) =
        hash_file_handle(&spooled.file, &spooled.path, policy.spool_buffer_bytes)?;
    verify_named_file_identity(&spooled.path, &spooled.file)?;
    ensure_real_directory(
        spooled.path.parent().ok_or_else(|| {
            KernelError::ArchiveIntegrity("stored package has no parent directory".to_owned())
        })?,
        "package",
    )?;
    if !file_is_readonly(&spooled.file, &spooled.path)? {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package {} became writable during intake",
            spooled.path.display()
        )));
    }
    if final_hash != spooled.hash || final_size != spooled.size {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package changed during intake: expected {}/{}, found {final_hash}/{final_size}",
            spooled.hash, spooled.size
        )));
    }
    transition_job(&writer.connection, job_id, JobState::Reconciling)?;
    let (source_records, stored_records, pending_records) = reconcile(&writer.connection, job_id)?;
    if source_records != stored_records || pending_records != 0 {
        return Err(KernelError::Reconciliation(format!(
            "job {job_id}: source={source_records}, stored={stored_records}, pending={pending_records}"
        )));
    }
    Ok(PendingReport {
        source_hash: spooled.hash,
        source_size: spooled.size,
        stored_archive: spooled.path,
        manifest,
        source_records,
        stored_records,
        pending_records,
        committed_batches,
    })
}

struct SpooledArchive {
    hash: String,
    size: u64,
    path: PathBuf,
    file: File,
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn spool_source(
    source: &Path,
    source_file: &File,
    expected_size: u64,
    job_id: &str,
    policy: &IntakePolicy,
    writer: &WorkspaceWriter,
    metrics: &mut Metrics,
    control: &IngestControl,
) -> Result<SpooledArchive> {
    control.check_external()?;
    let packages = writer.root.join(PACKAGES_DIRECTORY);
    fs::create_dir_all(&packages).map_err(|error| KernelError::io(&packages, error))?;
    ensure_real_directory(&packages, "package")?;
    let partial = packages.join(format!("{job_id}.part"));
    let mut input = source_file
        .try_clone()
        .map_err(|error| KernelError::io(source, error))?;
    input
        .rewind()
        .map_err(|error| KernelError::io(source, error))?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&partial)
        .map_err(|error| KernelError::io(&partial, error))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; policy.spool_buffer_bytes];
    loop {
        control.check_external()?;
        let read = input
            .read(&mut buffer)
            .map_err(|error| KernelError::io(source, error))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|error| KernelError::io(&partial, error))?;
        digest.update(&buffer[..read]);
        total = total
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                KernelError::Reconciliation("spooled source byte count overflow".to_owned())
            })?;
        enforce_source_size(total, policy.maximum_source_bytes)?;
        metrics.sample();
    }
    control.check_external()?;
    output
        .sync_all()
        .map_err(|error| KernelError::io(&partial, error))?;
    if total != expected_size {
        return Err(KernelError::ArchiveIntegrity(format!(
            "source changed size while being spooled: initial {expected_size} bytes, read {total}"
        )));
    }
    drop(output);
    let hash = format!("{:x}", digest.finalize());
    let stored = packages.join(format!("{hash}.zip"));
    let mut permissions = fs::metadata(&partial)
        .map_err(|error| KernelError::io(&partial, error))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&partial, permissions).map_err(|error| KernelError::io(&partial, error))?;
    match publish_new(&partial, &stored) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = open_regular_nofollow(&stored)?;
            let (existing_hash, existing_size) =
                hash_file_handle(&existing, &stored, policy.spool_buffer_bytes)?;
            if existing_hash != hash || existing_size != total {
                return Err(KernelError::ArchiveIntegrity(format!(
                    "existing hash-named package {} does not match its name/content",
                    stored.display()
                )));
            }
            let cleanup = recover_namespace_partials(writer, CleanupNamespace::Packages)?;
            cleanup.ensure_no_pending()?;
        }
        Err(error) => return Err(KernelError::io(&stored, error)),
    }
    let file = open_regular_nofollow(&stored)?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(&stored, error))?;
    if !metadata.permissions().readonly() {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package {} is not read-only",
            stored.display()
        )));
    }
    let (stored_hash, stored_size) = hash_file_handle(&file, &stored, policy.spool_buffer_bytes)?;
    if stored_hash != hash || stored_size != total {
        return Err(KernelError::ArchiveIntegrity(format!(
            "promoted package {} failed its final hash/size verification",
            stored.display()
        )));
    }
    ensure_real_directory(&packages, "package")?;
    metrics.sample();
    Ok(SpooledArchive {
        hash,
        size: total,
        path: stored,
        file,
    })
}

fn hash_file_handle(file: &File, path: &Path, buffer_size: usize) -> Result<(String, u64)> {
    let mut reader = file
        .try_clone()
        .map_err(|error| KernelError::io(path, error))?;
    reader
        .rewind()
        .map_err(|error| KernelError::io(path, error))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; buffer_size];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| KernelError::io(path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        total = total
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| KernelError::Reconciliation("file byte count overflow".to_owned()))?;
    }
    Ok((format!("{:x}", digest.finalize()), total))
}

fn enforce_source_size(actual: u64, limit: u64) -> Result<()> {
    if actual > limit {
        return Err(KernelError::SourceSize { actual, limit });
    }
    Ok(())
}

fn reject_tombstone_source(path: &Path) -> Result<()> {
    if path_uses_tombstone_namespace(path) {
        return Err(KernelError::QuarantinedSource(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| map_regular_open_error(path, error.into()))?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .is_file()
    {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let is_regular = metadata.file_type().is_file();
    #[cfg(windows)]
    let is_regular = is_regular && !crate::durability::is_reparse_point(&metadata);
    if !is_regular {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .is_file()
    {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(unix)]
fn map_regular_open_error(path: &Path, error: io::Error) -> KernelError {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        KernelError::SourceNotRegular(path.to_path_buf())
    } else {
        KernelError::io(path, error)
    }
}

fn verify_named_file_identity(path: &Path, file: &File) -> Result<()> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let is_regular = named.file_type().is_file();
    #[cfg(windows)]
    let is_regular = is_regular && !crate::durability::is_reparse_point(&named);
    if !is_regular {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package path {} is no longer a regular file",
            path.display()
        )));
    }
    #[cfg(not(windows))]
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(KernelError::ArchiveIntegrity(format!(
                "stored package path {} no longer names the inspected file",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))?
    {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package path {} no longer names the inspected file",
            path.display()
        )));
    }
    #[cfg(not(any(unix, windows)))]
    if named.len() != opened.len() {
        return Err(KernelError::ArchiveIntegrity(format!(
            "stored package path {} changed during intake",
            path.display()
        )));
    }
    Ok(())
}

fn file_is_readonly(file: &File, path: &Path) -> Result<bool> {
    Ok(file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .permissions()
        .readonly())
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let is_real_directory = metadata.file_type().is_dir();
    #[cfg(windows)]
    let is_real_directory = is_real_directory && !crate::durability::is_reparse_point(&metadata);
    if !is_real_directory {
        return Err(KernelError::ArchiveIntegrity(format!(
            "{label} directory {} is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn persist_manifest(
    connection: &Connection,
    job_id: &str,
    manifest: &ArchiveManifest,
) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    for entry in &manifest.entries {
        transaction.execute(
            "INSERT INTO archive_entries (
                 job_id, path, is_directory, is_csv, compressed_size,
                 expanded_size, crc32
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                job_id,
                entry.path,
                i64::from(entry.is_directory),
                i64::from(entry.is_csv),
                sqlite_integer(entry.compressed_size, "compressed entry size")?,
                sqlite_integer(entry.expanded_size, "expanded entry size")?,
                i64::from(entry.crc32),
            ],
        )?;
        if entry.is_csv {
            transaction.execute(
                "INSERT INTO csv_files (job_id, path) VALUES (?1, ?2)",
                params![job_id, entry.path],
            )?;
        }
    }
    let changed = transaction.execute(
        "UPDATE jobs
         SET manifest_entries = ?1, csv_files = ?2, expanded_bytes = ?3,
             updated_at = ?4
         WHERE id = ?5 AND state = 'INSPECTING'",
        params![
            sqlite_integer(
                u64::try_from(manifest.entries.len()).unwrap_or(u64::MAX),
                "manifest entry count"
            )?,
            sqlite_integer(
                u64::try_from(manifest.csv_paths.len()).unwrap_or(u64::MAX),
                "CSV file count"
            )?,
            sqlite_integer(manifest.total_expanded_bytes, "expanded archive size")?,
            crate::state::now_rfc3339(),
            job_id,
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::Reconciliation(format!(
            "could not persist manifest for job {job_id}"
        )));
    }
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn ingest_csv_files(
    connection: &mut Connection,
    job_id: &str,
    archive_file: &File,
    archive_path: &Path,
    manifest: &ArchiveManifest,
    policy: &IntakePolicy,
    metrics: &mut Metrics,
    control: &IngestControl,
) -> Result<u64> {
    let file = archive_file
        .try_clone()
        .map_err(|error| KernelError::io(archive_path, error))?;
    let mut archive = ZipArchive::new(file).map_err(|error| archive_integrity(&error))?;
    let crash_after_batches = crash_after_batches();
    let mut committed_batches = 0_u64;
    let mut actual_csv_expanded = 0_u64;
    let mut actual_csv_compressed = 0_u64;
    let mut total_records = 0_u64;
    for path in &manifest.csv_paths {
        control.check_batch_boundary(committed_batches)?;
        let entry = archive
            .by_name(path)
            .map_err(|error| archive_integrity(&error))?;
        let expected = manifest
            .entries
            .iter()
            .find(|candidate| candidate.path == *path)
            .ok_or_else(|| {
                KernelError::Reconciliation(format!(
                    "CSV path {path:?} is absent from its inspected manifest"
                ))
            })?;
        if entry.size() != expected.expanded_size
            || entry.compressed_size() != expected.compressed_size
            || entry.crc32() != expected.crc32
        {
            return Err(KernelError::ArchiveIntegrity(format!(
                "CSV entry {path:?} metadata changed after inspection"
            )));
        }
        let validating = CsvLimitReader::new(
            entry,
            policy,
            actual_csv_expanded,
            actual_csv_compressed,
            expected.compressed_size,
        );
        let parser_buffer = policy.maximum_csv_record_bytes.clamp(1, 64 * 1024);
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .buffer_capacity(parser_buffer)
            .from_reader(validating);
        let header_result = reader.headers().cloned();
        let header = match header_result {
            Ok(header) => header,
            Err(error) => return Err(map_csv_error(path, &reader, &error)),
        };
        validate_string_record(path, &header, policy)?;
        if header.is_empty() || header.iter().all(str::is_empty) {
            return Err(KernelError::CsvEmptyHeader(path.clone()));
        }
        let header_json = record_json(&header)?;
        drop(header);
        let changed = connection.execute(
            "UPDATE csv_files SET header_json = ?1 WHERE job_id = ?2 AND path = ?3",
            params![header_json, job_id, path],
        )?;
        if changed != 1 {
            return Err(KernelError::Reconciliation(format!(
                "missing CSV manifest row for {path:?}"
            )));
        }

        let mut ordinal = 0_u64;
        let mut reached_end = false;
        while !reached_end {
            control.check_batch_boundary(committed_batches)?;
            let transaction = connection.transaction()?;
            let mut batch_source = 0_u64;
            let mut batch_stored = 0_u64;
            for _ in 0..policy.transaction_batch_rows {
                if let Err(error) = control.check_batch_boundary(committed_batches) {
                    transaction.rollback()?;
                    return Err(error);
                }
                let before = reader.position().clone();
                let mut record = StringRecord::new();
                match reader.read_record(&mut record) {
                    Ok(true) => {}
                    Ok(false) => {
                        reached_end = true;
                        break;
                    }
                    Err(error) => return Err(map_csv_error(path, &reader, &error)),
                }
                validate_string_record(path, &record, policy)?;
                total_records =
                    total_records
                        .checked_add(1)
                        .ok_or(KernelError::CsvRecordCountLimit {
                            actual: u64::MAX,
                            limit: policy.maximum_csv_records,
                        })?;
                if total_records > policy.maximum_csv_records {
                    return Err(KernelError::CsvRecordCountLimit {
                        actual: total_records,
                        limit: policy.maximum_csv_records,
                    });
                }
                ordinal = ordinal.checked_add(1).ok_or_else(|| {
                    KernelError::Reconciliation("CSV record ordinal overflow".to_owned())
                })?;
                let line_start = record
                    .position()
                    .map_or_else(|| before.line(), csv::Position::line);
                let line_end = line_start
                    .checked_add(record_embedded_line_breaks(&record))
                    .ok_or_else(|| {
                        KernelError::Reconciliation("CSV line number overflow".to_owned())
                    })?;
                let fields_json = record_json(&record)?;
                let inserted = transaction.execute(
                    "INSERT INTO source_records (
                         job_id, file_path, source_ordinal, line_start, line_end, fields_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        job_id,
                        path,
                        sqlite_integer(ordinal, "CSV source ordinal")?,
                        sqlite_integer(line_start, "CSV starting line")?,
                        sqlite_integer(line_end, "CSV ending line")?,
                        fields_json,
                    ],
                )?;
                batch_source += 1;
                batch_stored = batch_stored
                    .checked_add(u64::try_from(inserted).unwrap_or(u64::MAX))
                    .ok_or_else(|| {
                        KernelError::Reconciliation("stored record count overflow".to_owned())
                    })?;
            }
            if batch_source == 0 {
                transaction.rollback()?;
                break;
            }
            if let Err(error) = control.check_batch_boundary(committed_batches) {
                transaction.rollback()?;
                return Err(error);
            }
            let batch_pending = batch_source.checked_sub(batch_stored).ok_or_else(|| {
                KernelError::Reconciliation(
                    "stored record count exceeds source record count".to_owned(),
                )
            })?;
            transaction.execute(
                "UPDATE csv_files
                 SET source_records = source_records + ?1,
                     stored_records = stored_records + ?2
                 WHERE job_id = ?3 AND path = ?4",
                params![
                    sqlite_integer(batch_source, "CSV source batch count")?,
                    sqlite_integer(batch_stored, "CSV stored batch count")?,
                    job_id,
                    path,
                ],
            )?;
            transaction.execute(
                "UPDATE jobs
                 SET source_records = source_records + ?1,
                     stored_records = stored_records + ?2,
                     pending_records = pending_records + ?3,
                     updated_at = ?4
                 WHERE id = ?5 AND state = 'INGESTING'",
                params![
                    sqlite_integer(batch_source, "source batch count")?,
                    sqlite_integer(batch_stored, "stored batch count")?,
                    sqlite_integer(batch_pending, "pending batch count")?,
                    crate::state::now_rfc3339(),
                    job_id,
                ],
            )?;
            transaction.commit()?;
            committed_batches = committed_batches.checked_add(1).ok_or_else(|| {
                KernelError::Reconciliation("committed batch count overflow".to_owned())
            })?;
            metrics.sample();
            inject_hard_crash(crash_after_batches, committed_batches);
        }
        let validating = reader.into_inner();
        let entry_bytes = validating.total_bytes();
        if entry_bytes != expected.expanded_size {
            return Err(KernelError::ArchiveIntegrity(format!(
                "CSV entry {path:?} yielded {entry_bytes} bytes; inspected manifest declares {}",
                expected.expanded_size
            )));
        }
        actual_csv_expanded = actual_csv_expanded.checked_add(entry_bytes).ok_or(
            KernelError::ArchiveExpandedSize {
                actual: u64::MAX,
                limit: policy.maximum_expanded_bytes,
            },
        )?;
        actual_csv_compressed = actual_csv_compressed
            .checked_add(expected.compressed_size)
            .ok_or(KernelError::ArchiveCompressionRatio {
                actual: f64::INFINITY,
                limit: policy.maximum_compression_ratio,
            })?;
        let actual_ratio = compression_ratio(entry_bytes, expected.compressed_size);
        let aggregate_ratio = compression_ratio(actual_csv_expanded, actual_csv_compressed);
        let maximum_ratio = actual_ratio.max(aggregate_ratio);
        if maximum_ratio > policy.maximum_compression_ratio {
            return Err(KernelError::ArchiveCompressionRatio {
                actual: maximum_ratio,
                limit: policy.maximum_compression_ratio,
            });
        }
    }
    control.check_batch_boundary(committed_batches)?;
    Ok(committed_batches)
}

fn reconcile(connection: &Connection, job_id: &str) -> Result<(u64, u64, u64)> {
    let source: i64 = connection.query_row(
        "SELECT COALESCE(SUM(source_records), 0) FROM csv_files WHERE job_id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    let stored: i64 = connection.query_row(
        "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    let source = u64::try_from(source).map_err(|_| {
        KernelError::Reconciliation("negative aggregate source record count".to_owned())
    })?;
    let stored = u64::try_from(stored).map_err(|_| {
        KernelError::Reconciliation("negative aggregate stored record count".to_owned())
    })?;
    let pending = source.checked_sub(stored).ok_or_else(|| {
        KernelError::Reconciliation(format!(
            "stored record count {stored} exceeds source record count {source}"
        ))
    })?;
    let changed = connection.execute(
        "UPDATE jobs
         SET source_records = ?1, stored_records = ?2, pending_records = ?3,
             updated_at = ?4
         WHERE id = ?5 AND state = 'RECONCILING'",
        params![
            sqlite_integer(source, "reconciled source count")?,
            sqlite_integer(stored, "reconciled stored count")?,
            sqlite_integer(pending, "reconciled pending count")?,
            crate::state::now_rfc3339(),
            job_id,
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::Reconciliation(format!(
            "could not persist reconciliation for job {job_id}"
        )));
    }
    Ok((source, stored, pending))
}

fn validate_archive_path(raw: &[u8], maximum_bytes: usize) -> Result<String> {
    let display = String::from_utf8_lossy(raw).into_owned();
    if raw.len() > maximum_bytes {
        return Err(KernelError::ArchiveUnsafePath(display));
    }
    let name =
        std::str::from_utf8(raw).map_err(|_| KernelError::ArchiveUnsafePath(display.clone()))?;
    if name.starts_with('/')
        || name.starts_with('\\')
        || (name.as_bytes().get(1) == Some(&b':')
            && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
    {
        return Err(KernelError::ArchiveAbsolutePath(name.to_owned()));
    }
    if name.is_empty()
        || name.contains('\\')
        || name
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(KernelError::ArchiveUnsafePath(name.to_owned()));
    }
    let without_trailing_slash = name.strip_suffix('/').unwrap_or(name);
    if without_trailing_slash.is_empty() {
        return Err(KernelError::ArchiveUnsafePath(name.to_owned()));
    }
    let mut components = Vec::new();
    for component in without_trailing_slash.split('/') {
        if component == ".." {
            return Err(KernelError::ArchivePathTraversal(name.to_owned()));
        }
        if component.is_empty() || component == "." {
            return Err(KernelError::ArchiveUnsafePath(name.to_owned()));
        }
        components.push(component);
    }
    Ok(components.join("/"))
}

fn validate_entry_type<R: Read>(
    entry: &zip::read::ZipFile<'_, R>,
    path: &str,
    is_directory: bool,
) -> Result<()> {
    const FILE_TYPE_MASK: u32 = 0o170_000;
    const REGULAR_FILE: u32 = 0o100_000;
    const DIRECTORY: u32 = 0o040_000;
    const SYMBOLIC_LINK: u32 = 0o120_000;
    let file_type = entry.unix_mode().map_or(0, |mode| mode & FILE_TYPE_MASK);
    if entry.is_symlink()
        || file_type == SYMBOLIC_LINK
        || !matches!(file_type, 0 | REGULAR_FILE | DIRECTORY)
    {
        return Err(KernelError::ArchiveLink(path.to_owned()));
    }
    if (is_directory && file_type == REGULAR_FILE) || (!is_directory && file_type == DIRECTORY) {
        return Err(KernelError::ArchiveLink(path.to_owned()));
    }
    Ok(())
}

fn has_csv_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

fn compression_ratio(expanded: u64, compressed: u64) -> f64 {
    if expanded == 0 {
        1.0
    } else if compressed == 0 {
        f64::INFINITY
    } else {
        #[allow(clippy::cast_precision_loss)]
        {
            expanded as f64 / compressed as f64
        }
    }
}

fn copy_to_sink(
    reader: &mut impl Read,
    prior_expanded: u64,
    prior_compressed: u64,
    entry_compressed: u64,
    expanded_limit: u64,
    ratio_limit: f64,
) -> Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; CRC_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| archive_integrity_io(&error))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or(KernelError::ArchiveExpandedSize {
                actual: u64::MAX,
                limit: expanded_limit,
            })?;
        let aggregate =
            prior_expanded
                .checked_add(total)
                .ok_or(KernelError::ArchiveExpandedSize {
                    actual: u64::MAX,
                    limit: expanded_limit,
                })?;
        if aggregate > expanded_limit {
            return Err(KernelError::ArchiveExpandedSize {
                actual: aggregate,
                limit: expanded_limit,
            });
        }
        let entry_ratio = compression_ratio(total, entry_compressed);
        let aggregate_compressed = prior_compressed.saturating_add(entry_compressed);
        let aggregate_ratio = compression_ratio(aggregate, aggregate_compressed);
        let observed_ratio = entry_ratio.max(aggregate_ratio);
        if observed_ratio > ratio_limit {
            return Err(KernelError::ArchiveCompressionRatio {
                actual: observed_ratio,
                limit: ratio_limit,
            });
        }
    }
    Ok(total)
}

fn archive_integrity(error: &zip::result::ZipError) -> KernelError {
    KernelError::ArchiveIntegrity(error.to_string())
}

fn archive_integrity_io(error: &io::Error) -> KernelError {
    KernelError::ArchiveIntegrity(error.to_string())
}

fn record_json(record: &StringRecord) -> Result<String> {
    serde_json::to_string(&RecordFields(record)).map_err(Into::into)
}

struct RecordFields<'a>(&'a StringRecord);

impl Serialize for RecordFields<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for field in self.0 {
            sequence.serialize_element(field)?;
        }
        sequence.end()
    }
}

fn record_embedded_line_breaks(record: &StringRecord) -> u64 {
    record.iter().fold(0_u64, |total, field| {
        let mut field_breaks = 0_u64;
        let mut previous_was_carriage_return = false;
        for byte in field.bytes() {
            if byte == b'\r' {
                field_breaks = field_breaks.saturating_add(1);
                previous_was_carriage_return = true;
            } else {
                if byte == b'\n' && !previous_was_carriage_return {
                    field_breaks = field_breaks.saturating_add(1);
                }
                previous_was_carriage_return = false;
            }
        }
        total.saturating_add(field_breaks)
    })
}

fn validate_string_record(entry: &str, record: &StringRecord, policy: &IntakePolicy) -> Result<()> {
    if record.len() > policy.maximum_csv_columns {
        return Err(KernelError::CsvColumnLimit {
            entry: entry.to_owned(),
            actual: record.len(),
            limit: policy.maximum_csv_columns,
        });
    }
    if record
        .iter()
        .any(|field| field.len() > policy.maximum_csv_field_bytes)
    {
        return Err(KernelError::CsvFieldLimit {
            entry: entry.to_owned(),
            limit: policy.maximum_csv_field_bytes,
        });
    }
    let decoded_size = record
        .iter()
        .try_fold(record.len().saturating_sub(1), |size, field| {
            size.checked_add(field.len())
        })
        .unwrap_or(usize::MAX);
    if decoded_size > policy.maximum_csv_record_bytes {
        return Err(KernelError::CsvRecordLimit {
            entry: entry.to_owned(),
            limit: policy.maximum_csv_record_bytes,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum CsvViolation {
    Field,
    Record,
    Expanded,
    CompressionRatio,
}

#[derive(Clone, Copy, Debug)]
enum CsvScanState {
    FieldStart,
    Unquoted,
    Quoted,
    AfterQuote,
}

struct CsvLimitReader<R> {
    inner: R,
    maximum_field_bytes: usize,
    maximum_record_bytes: usize,
    prior_expanded_bytes: u64,
    prior_compressed_bytes: u64,
    entry_compressed_bytes: u64,
    maximum_expanded_bytes: u64,
    maximum_compression_ratio: f64,
    total_bytes: u64,
    field_bytes: usize,
    record_bytes: usize,
    state: CsvScanState,
    after_carriage_return: bool,
    violation: Option<CsvViolation>,
    observed_compression_ratio: f64,
}

impl<R> CsvLimitReader<R> {
    fn new(
        inner: R,
        policy: &IntakePolicy,
        prior_expanded_bytes: u64,
        prior_compressed_bytes: u64,
        entry_compressed_bytes: u64,
    ) -> Self {
        Self {
            inner,
            maximum_field_bytes: policy.maximum_csv_field_bytes,
            maximum_record_bytes: policy.maximum_csv_record_bytes,
            prior_expanded_bytes,
            prior_compressed_bytes,
            entry_compressed_bytes,
            maximum_expanded_bytes: policy.maximum_expanded_bytes,
            maximum_compression_ratio: policy.maximum_compression_ratio,
            total_bytes: 0,
            field_bytes: 0,
            record_bytes: 0,
            state: CsvScanState::FieldStart,
            after_carriage_return: false,
            violation: None,
            observed_compression_ratio: 1.0,
        }
    }

    const fn violation(&self) -> Option<CsvViolation> {
        self.violation
    }

    const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn scan(&mut self, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            if self.after_carriage_return {
                self.after_carriage_return = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match self.state {
                CsvScanState::Quoted => {
                    self.add_record_byte()?;
                    if byte == b'"' {
                        self.state = CsvScanState::AfterQuote;
                    } else {
                        self.add_field_byte()?;
                    }
                }
                CsvScanState::AfterQuote => {
                    if byte == b'\n' || byte == b'\r' {
                        self.finish_record(byte);
                    } else {
                        self.add_record_byte()?;
                        if byte == b'"' {
                            self.add_field_byte()?;
                            self.state = CsvScanState::Quoted;
                        } else if byte == b',' {
                            self.field_bytes = 0;
                            self.state = CsvScanState::FieldStart;
                        } else {
                            self.add_field_byte()?;
                            self.state = CsvScanState::Unquoted;
                        }
                    }
                }
                CsvScanState::FieldStart => {
                    if byte == b'\n' || byte == b'\r' {
                        self.finish_record(byte);
                    } else {
                        self.add_record_byte()?;
                        if byte == b'"' {
                            self.state = CsvScanState::Quoted;
                        } else if byte == b',' {
                            self.field_bytes = 0;
                        } else {
                            self.add_field_byte()?;
                            self.state = CsvScanState::Unquoted;
                        }
                    }
                }
                CsvScanState::Unquoted => {
                    if byte == b'\n' || byte == b'\r' {
                        self.finish_record(byte);
                    } else {
                        self.add_record_byte()?;
                        if byte == b',' {
                            self.field_bytes = 0;
                            self.state = CsvScanState::FieldStart;
                        } else {
                            self.add_field_byte()?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn add_field_byte(&mut self) -> io::Result<()> {
        self.field_bytes = self.field_bytes.saturating_add(1);
        if self.field_bytes > self.maximum_field_bytes {
            self.violation = Some(CsvViolation::Field);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CSV field limit exceeded",
            ));
        }
        Ok(())
    }

    fn add_record_byte(&mut self) -> io::Result<()> {
        self.record_bytes = self.record_bytes.saturating_add(1);
        if self.record_bytes > self.maximum_record_bytes {
            self.violation = Some(CsvViolation::Record);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CSV record limit exceeded",
            ));
        }
        Ok(())
    }

    fn finish_record(&mut self, terminator: u8) {
        self.field_bytes = 0;
        self.record_bytes = 0;
        self.state = CsvScanState::FieldStart;
        self.after_carriage_return = terminator == b'\r';
    }
}

impl<R: Read> Read for CsvLimitReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.violation.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CSV limit already exceeded",
            ));
        }
        let read = self.inner.read(buffer)?;
        self.total_bytes = self
            .total_bytes
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let aggregate_expanded = self.prior_expanded_bytes.saturating_add(self.total_bytes);
        if aggregate_expanded > self.maximum_expanded_bytes {
            self.violation = Some(CsvViolation::Expanded);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive expanded-byte limit exceeded",
            ));
        }
        let entry_ratio = compression_ratio(self.total_bytes, self.entry_compressed_bytes);
        let aggregate_compressed = self
            .prior_compressed_bytes
            .saturating_add(self.entry_compressed_bytes);
        let aggregate_ratio = compression_ratio(aggregate_expanded, aggregate_compressed);
        self.observed_compression_ratio = entry_ratio.max(aggregate_ratio);
        if self.observed_compression_ratio > self.maximum_compression_ratio {
            self.violation = Some(CsvViolation::CompressionRatio);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive compression-ratio limit exceeded",
            ));
        }
        self.scan(&buffer[..read])?;
        Ok(read)
    }
}

fn map_csv_error<R: Read>(
    entry: &str,
    reader: &csv::Reader<CsvLimitReader<R>>,
    error: &csv::Error,
) -> KernelError {
    match reader.get_ref().violation() {
        Some(CsvViolation::Field) => KernelError::CsvFieldLimit {
            entry: entry.to_owned(),
            limit: reader.get_ref().maximum_field_bytes,
        },
        Some(CsvViolation::Record) => KernelError::CsvRecordLimit {
            entry: entry.to_owned(),
            limit: reader.get_ref().maximum_record_bytes,
        },
        Some(CsvViolation::Expanded) => KernelError::ArchiveExpandedSize {
            actual: reader
                .get_ref()
                .prior_expanded_bytes
                .saturating_add(reader.get_ref().total_bytes),
            limit: reader.get_ref().maximum_expanded_bytes,
        },
        Some(CsvViolation::CompressionRatio) => KernelError::ArchiveCompressionRatio {
            actual: reader.get_ref().observed_compression_ratio,
            limit: reader.get_ref().maximum_compression_ratio,
        },
        None if error.is_io_error() => {
            KernelError::ArchiveIntegrity(format!("while reading CSV entry {entry:?}: {error}"))
        }
        None => KernelError::CsvSyntax {
            entry: entry.to_owned(),
            detail: error.to_string(),
        },
    }
}

fn crash_after_batches() -> Option<u64> {
    std::env::var("MIGRATION_STREAM_TEST_CRASH_AFTER_BATCHES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

fn inject_hard_crash(configured: Option<u64>, committed_batches: u64) {
    if configured.is_some_and(|configured| committed_batches >= configured) {
        std::process::abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn test_policy() -> IntakePolicy {
        IntakePolicy {
            spool_buffer_bytes: 128,
            minimum_free_space_bytes: 0,
            maximum_source_bytes: 128 * 1024,
            maximum_entries: 20,
            maximum_entry_path_bytes: 128,
            maximum_expanded_bytes: 64 * 1024,
            maximum_compression_ratio: 200.0,
            maximum_csv_field_bytes: 128,
            maximum_csv_record_bytes: 512,
            maximum_csv_columns: 32,
            maximum_csv_records: 1_000,
            transaction_batch_rows: 2,
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create ZIP");
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, contents) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(contents).expect("write entry");
        }
        writer.finish().expect("finish ZIP");
    }

    fn corrupt_first_entry(path: &Path) {
        let file = File::open(path).expect("open ZIP");
        let mut archive = ZipArchive::new(file).expect("parse ZIP");
        let entry = archive.by_index_raw(0).expect("first entry");
        let data_start = entry.data_start().expect("data offset");
        drop(entry);
        drop(archive);

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open ZIP for corruption");
        file.seek(SeekFrom::Start(data_start)).expect("seek entry");
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).expect("read compressed byte");
        byte[0] ^= 0xff;
        file.seek(SeekFrom::Start(data_start))
            .expect("seek entry again");
        file.write_all(&byte).expect("corrupt entry");
        file.sync_all().expect("sync corruption");
    }

    #[test]
    fn manifest_is_sorted_and_crc_checked() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        write_zip(&archive, &[("z.csv", b"id\n2\n"), ("a.csv", b"id\n1\n")]);
        let manifest = inspect_archive(&archive, &test_policy()).expect("manifest");
        assert_eq!(manifest.csv_paths, ["a.csv", "z.csv"]);
        assert_eq!(manifest.entries[0].path, "a.csv");
    }

    #[test]
    fn rejects_traversal_before_ingestion() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        write_zip(&archive, &[("../escape.csv", b"id\n1\n")]);
        assert!(matches!(
            inspect_archive(&archive, &test_policy()),
            Err(KernelError::ArchivePathTraversal(_))
        ));
    }

    #[test]
    fn quarantined_artifact_cannot_be_inspected_ingested_or_promoted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary
            .path()
            .join("11111111-1111-4111-8111-111111111111.tombstone");
        let workspace = temporary.path().join("workspace");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);

        let inspection = inspect_archive(&archive, &test_policy());
        assert!(matches!(
            inspection,
            Err(KernelError::QuarantinedSource(ref rejected)) if rejected == &archive
        ));
        let ingestion = ingest_package(&archive, &workspace, &test_policy());
        match ingestion {
            Err(error) => {
                match &error {
                    KernelError::QuarantinedSource(rejected) => assert_eq!(rejected, &archive),
                    other => panic!("unexpected tombstone rejection: {other}"),
                }
                assert_eq!(error.code(), "QUARANTINED_SOURCE");
            }
            Ok(_) => panic!("quarantined source must not be ingested"),
        }
        assert!(!workspace.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_variant_tombstone_cannot_be_ingested() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary
            .path()
            .join("11111111-1111-4111-8111-111111111111.TOMBSTONE");
        let workspace = temporary.path().join("workspace");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);

        assert!(matches!(
            ingest_package(&archive, &workspace, &test_policy()),
            Err(KernelError::QuarantinedSource(ref rejected)) if rejected == &archive
        ));
        assert!(!workspace.exists());
    }

    #[test]
    fn rejects_duplicate_normalized_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        write_zip(&archive, &[("same.csv", b"id\n1\n"), ("same.csv/", b"")]);
        assert!(matches!(
            inspect_archive(&archive, &test_policy()),
            Err(KernelError::ArchiveDuplicatePath(_))
        ));
    }

    #[test]
    fn rejects_absolute_unsafe_and_link_entries_with_distinct_types() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let absolute = temporary.path().join("absolute.zip");
        write_zip(&absolute, &[("/root.csv", b"id\n1\n")]);
        assert!(matches!(
            inspect_archive(&absolute, &test_policy()),
            Err(KernelError::ArchiveAbsolutePath(_))
        ));

        let unsafe_path = temporary.path().join("unsafe.zip");
        write_zip(&unsafe_path, &[("folder\\data.csv", b"id\n1\n")]);
        assert!(matches!(
            inspect_archive(&unsafe_path, &test_policy()),
            Err(KernelError::ArchiveUnsafePath(_))
        ));

        let link = temporary.path().join("link.zip");
        let file = File::create(&link).expect("create link ZIP");
        let mut writer = ZipWriter::new(file);
        writer
            .add_symlink("link.csv", "target.csv", SimpleFileOptions::default())
            .expect("add symlink");
        writer.finish().expect("finish link ZIP");
        assert!(matches!(
            inspect_archive(&link, &test_policy()),
            Err(KernelError::ArchiveLink(_))
        ));
    }

    #[test]
    fn rejects_missing_csv_and_each_archive_size_gate() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let missing = temporary.path().join("missing.zip");
        write_zip(&missing, &[("readme.txt", b"text")]);
        assert!(matches!(
            inspect_archive(&missing, &test_policy()),
            Err(KernelError::ArchiveMissingCsv)
        ));

        let archive = temporary.path().join("limits.zip");
        write_zip(&archive, &[("a.csv", b"id\n1\n"), ("b.csv", b"id\n2\n")]);
        let mut count_policy = test_policy();
        count_policy.maximum_entries = 1;
        assert!(matches!(
            inspect_archive(&archive, &count_policy),
            Err(KernelError::ArchiveEntryLimit { .. })
        ));

        let mut expanded_policy = test_policy();
        expanded_policy.maximum_expanded_bytes = 3;
        assert!(matches!(
            inspect_archive(&archive, &expanded_policy),
            Err(KernelError::ArchiveExpandedSize { .. })
        ));

        let ratio_archive = temporary.path().join("ratio.zip");
        let repetitive = format!("value\n{}\n", "a".repeat(4_000));
        write_zip(&ratio_archive, &[("data.csv", repetitive.as_bytes())]);
        let mut ratio_policy = test_policy();
        ratio_policy.maximum_compression_ratio = 2.0;
        assert!(matches!(
            inspect_archive(&ratio_archive, &ratio_policy),
            Err(KernelError::ArchiveCompressionRatio { .. })
        ));
    }

    #[test]
    fn corrupted_entry_is_a_typed_integrity_rejection() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("corrupt.zip");
        write_zip(&archive, &[("data.csv", b"id,value\n1,alpha\n")]);
        corrupt_first_entry(&archive);
        assert!(matches!(
            inspect_archive(&archive, &test_policy()),
            Err(KernelError::ArchiveIntegrity(_))
        ));
    }

    #[test]
    fn ingestion_preserves_text_and_multiline_lineage() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let workspace = temporary.path().join("workspace");
        write_zip(
            &archive,
            &[("data.csv", b"id,note\n1,\"alpha\nbeta\"\n2,\"  exact  \"\n")],
        );
        let report = ingest_package(&archive, &workspace, &test_policy()).expect("ingest");
        assert_eq!(report.source_records, 2);
        assert_eq!(report.source_records, report.stored_records);
        assert_eq!(report.pending_records, 0);
        assert!(
            fs::metadata(&report.stored_archive)
                .expect("stored archive metadata")
                .permissions()
                .readonly()
        );

        let connection = Connection::open(workspace.join(crate::state::DATABASE_FILENAME))
            .expect("open database");
        let rows: Vec<(String, i64, i64)> = {
            let mut statement = connection
                .prepare(
                    "SELECT fields_json, line_start, line_end
                     FROM source_records ORDER BY source_ordinal",
                )
                .expect("prepare");
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("rows")
                .collect::<std::result::Result<_, _>>()
                .expect("collect")
        };
        assert_eq!(rows[0].0, r#"["1","alpha\nbeta"]"#);
        assert_eq!((rows[0].1, rows[0].2), (2, 3));
        assert_eq!(rows[1].0, r#"["2","  exact  "]"#);
        let status = crate::state::workspace_status(&workspace).expect("status");
        assert_eq!(status.jobs[0].state, JobState::Complete);
    }

    #[test]
    fn field_limit_is_typed_and_failed_job_is_durable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let workspace = temporary.path().join("workspace");
        write_zip(&archive, &[("data.csv", b"id,value\n1,too-long\n")]);
        let mut policy = test_policy();
        policy.maximum_csv_field_bytes = 4;
        policy.maximum_csv_record_bytes = 32;
        let result = ingest_package(&archive, &workspace, &policy);
        assert!(matches!(result, Err(KernelError::CsvFieldLimit { .. })));
        let status = crate::state::workspace_status(&workspace).expect("status");
        assert_eq!(status.jobs[0].state, JobState::Failed);
        assert_eq!(
            status.jobs[0].error_code.as_deref(),
            Some("CSV_FIELD_LIMIT")
        );
    }

    #[test]
    fn invalid_utf8_and_record_limit_are_typed_and_never_complete() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let invalid = temporary.path().join("invalid.zip");
        let invalid_workspace = temporary.path().join("invalid-workspace");
        write_zip(&invalid, &[("data.csv", b"id\n\xff\n")]);
        assert!(matches!(
            ingest_package(&invalid, &invalid_workspace, &test_policy()),
            Err(KernelError::CsvSyntax { .. })
        ));
        let status = crate::state::workspace_status(&invalid_workspace).expect("status");
        assert_eq!(status.complete_jobs, 0);
        assert_eq!(status.failed_jobs, 1);

        let oversized = temporary.path().join("oversized.zip");
        let oversized_workspace = temporary.path().join("oversized-workspace");
        write_zip(&oversized, &[("data.csv", b"a,b,c\naaaa,bbbb,cccc\n")]);
        let mut policy = test_policy();
        policy.maximum_csv_field_bytes = 4;
        policy.maximum_csv_record_bytes = 8;
        assert!(matches!(
            ingest_package(&oversized, &oversized_workspace, &policy),
            Err(KernelError::CsvRecordLimit { .. })
        ));
        let status = crate::state::workspace_status(&oversized_workspace).expect("status");
        assert_eq!(
            status.jobs[0].error_code.as_deref(),
            Some("CSV_RECORD_LIMIT")
        );
    }

    #[test]
    fn streaming_limit_reader_handles_quoted_newlines() {
        let data = b"a,b\n1,\"x\ny\"\n";
        let mut policy = test_policy();
        policy.maximum_csv_field_bytes = 4;
        policy.maximum_csv_record_bytes = 16;
        policy.maximum_expanded_bytes = 64;
        policy.maximum_compression_ratio = 100.0;
        let mut reader = CsvLimitReader::new(&data[..], &policy, 0, 0, 10);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).expect("within limits");
        assert_eq!(output, data);
        assert!(reader.violation().is_none());
    }

    #[test]
    fn source_size_is_enforced_from_the_initial_regular_handle() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);
        let mut policy = test_policy();
        policy.maximum_source_bytes = 1;
        assert!(matches!(
            ingest_package(&archive, &temporary.path().join("workspace"), &policy),
            Err(KernelError::SourceSize { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_are_rejected_without_following_them() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let link = temporary.path().join("source-link.zip");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);
        symlink(&archive, &link).expect("create source symlink");
        assert!(matches!(
            inspect_archive(&link, &test_policy()),
            Err(KernelError::SourceNotRegular(path)) if path == link
        ));
    }

    #[test]
    fn csv_column_and_total_record_limits_are_typed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let columns = temporary.path().join("columns.zip");
        write_zip(&columns, &[("data.csv", b"a,b,c\n1,2,3\n")]);
        let mut column_policy = test_policy();
        column_policy.maximum_csv_columns = 2;
        assert!(matches!(
            ingest_package(
                &columns,
                &temporary.path().join("column-workspace"),
                &column_policy
            ),
            Err(KernelError::CsvColumnLimit {
                actual: 3,
                limit: 2,
                ..
            })
        ));

        let records = temporary.path().join("records.zip");
        let record_workspace = temporary.path().join("record-workspace");
        write_zip(&records, &[("data.csv", b"id\n1\n2\n")]);
        let mut record_policy = test_policy();
        record_policy.maximum_csv_records = 1;
        assert!(matches!(
            ingest_package(&records, &record_workspace, &record_policy),
            Err(KernelError::CsvRecordCountLimit {
                actual: 2,
                limit: 1
            })
        ));
        let status = crate::state::workspace_status(&record_workspace).expect("status");
        assert_eq!(
            status.jobs[0].error_code.as_deref(),
            Some("CSV_RECORD_COUNT_LIMIT")
        );
    }

    #[test]
    fn streaming_reader_enforces_actual_expansion_and_ratio() {
        let data = b"abcdefgh";
        let mut expanded_policy = test_policy();
        expanded_policy.maximum_expanded_bytes = 4;
        let mut expanded = CsvLimitReader::new(&data[..], &expanded_policy, 0, 0, 8);
        let mut output = Vec::new();
        assert!(expanded.read_to_end(&mut output).is_err());
        assert!(matches!(expanded.violation(), Some(CsvViolation::Expanded)));

        let mut ratio_policy = test_policy();
        ratio_policy.maximum_compression_ratio = 2.0;
        let mut ratio = CsvLimitReader::new(&data[..], &ratio_policy, 0, 0, 1);
        output.clear();
        assert!(ratio.read_to_end(&mut output).is_err());
        assert!(matches!(
            ratio.violation(),
            Some(CsvViolation::CompressionRatio)
        ));
    }

    #[test]
    fn inspection_copy_enforces_actual_expansion_and_ratio() {
        let mut expanded = &b"abcdefgh"[..];
        assert!(matches!(
            copy_to_sink(&mut expanded, 3, 8, 8, 10, 100.0),
            Err(KernelError::ArchiveExpandedSize {
                actual: 11,
                limit: 10
            })
        ));

        let mut ratio = &b"abcdefgh"[..];
        assert!(matches!(
            copy_to_sink(&mut ratio, 0, 0, 1, 64, 2.0),
            Err(KernelError::ArchiveCompressionRatio { .. })
        ));
    }

    #[test]
    fn content_addressed_promotion_never_replaces_an_existing_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let workspace = temporary.path().join("workspace");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);
        let source_file = open_regular_nofollow(&archive).expect("open source");
        let (hash, _) = hash_file_handle(&source_file, &archive, 128).expect("source hash");
        let packages = workspace.join(PACKAGES_DIRECTORY);
        fs::create_dir_all(&packages).expect("packages directory");
        let occupied = packages.join(format!("{hash}.zip"));
        fs::write(&occupied, b"not the source archive").expect("occupied hash name");

        assert!(matches!(
            ingest_package(&archive, &workspace, &test_policy()),
            Err(KernelError::ArchiveIntegrity(_))
        ));
        assert_eq!(
            fs::read(&occupied).expect("occupied path remains"),
            b"not the source archive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn package_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let workspace = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&outside).expect("outside directory");
        symlink(&outside, workspace.join(PACKAGES_DIRECTORY)).expect("package directory symlink");
        write_zip(&archive, &[("data.csv", b"id\n1\n")]);

        assert!(matches!(
            ingest_package(&archive, &workspace, &test_policy()),
            Err(KernelError::ArchiveIntegrity(_))
        ));
        assert!(
            fs::read_dir(&outside)
                .expect("outside remains readable")
                .next()
                .is_none()
        );
    }

    #[test]
    fn package_evidence_reread_detects_row_content_tampering() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive = temporary.path().join("source.zip");
        let workspace = temporary.path().join("workspace");
        let policy = test_policy();
        write_zip(&archive, &[("data.csv", b"id,value\n1,alpha\n")]);
        let report = ingest_package(&archive, &workspace, &policy).expect("ingest");
        let connection = Connection::open(workspace.join(crate::state::DATABASE_FILENAME))
            .expect("open database");
        connection
            .execute(
                "UPDATE source_records SET fields_json = '[\"1\",\"tampered\"]' WHERE job_id = ?1",
                [&report.job_id],
            )
            .expect("tamper row evidence");
        let verification =
            verify_ingested_package(&connection, &report.job_id, &report.stored_archive, &policy)
                .expect("verify package evidence");
        assert!(
            verification
                .issues
                .iter()
                .any(|issue| issue.contains("record 1 evidence differs"))
        );
    }

    #[test]
    fn legacy_policy_json_receives_safe_defaults_for_new_limits() {
        let legacy = serde_json::json!({
            "spool_buffer_bytes": 128,
            "minimum_free_space_bytes": 0,
            "maximum_entries": 20,
            "maximum_entry_path_bytes": 128,
            "maximum_expanded_bytes": 65_536,
            "maximum_compression_ratio": 200.0,
            "maximum_csv_field_bytes": 128,
            "maximum_csv_record_bytes": 512,
            "transaction_batch_rows": 2
        });
        let policy: IntakePolicy = serde_json::from_value(legacy).expect("legacy policy");
        IntakePolicy::validate(&policy).expect("defaulted policy is valid");
        assert_eq!(policy.maximum_source_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.maximum_csv_columns, 1_024);
        assert_eq!(policy.maximum_csv_records, 1_000_000);
    }
}

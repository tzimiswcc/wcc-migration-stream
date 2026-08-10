#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

//! WP-007 Publication Read Model and Queries.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

use crate::error::{KernelError, Result};
use crate::publication::{
    MAX_PACKAGE_SIZE_BYTES, PublicationEligibility, PublicationManifest, PublicationRecord,
    PublicationStatus,
};

pub use crate::publication::PUBLICATION_DETAIL_SCHEMA;

pub const PUBLICATION_LIST_SCHEMA: &str = "migration-stream/publication-list/v1";
pub const PUBLICATION_ELIGIBILITY_SCHEMA: &str = "migration-stream/publication-eligibility/v1";
pub const ENGAGEMENT_PUBLICATION_LIST_SCHEMA: &str =
    "migration-stream/engagement-publication-list/v1";
const MAXIMUM_ENGAGEMENT_PUBLICATIONS: usize = 200;

/// Read-only WP-007 publication service over one controlled workspace.
#[derive(Clone, Debug)]
pub struct PublicationReadService {
    workspace: PathBuf,
}

impl PublicationReadService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    fn open_db(&self) -> Result<Connection> {
        let db_path = self.workspace.join("state.sqlite3");
        crate::state::open_read_only(&db_path)
    }

    pub fn get_eligibility(&self, job_id: &str) -> Result<PublicationEligibility> {
        let connection = self.open_db()?;
        get_publication_eligibility_from_conn(&connection, job_id)
    }

    pub fn list_publications(&self, job_id: &str) -> Result<PublicationListPage> {
        let connection = self.open_db()?;
        list_publications_from_conn(&connection, job_id)
    }

    /// Return only the durable count and latest immutable publication needed
    /// to bind a local runtime application identity.
    pub fn authority_head(&self, job_id: &str) -> Result<PublicationAuthorityHead> {
        let connection = self.open_db()?;
        publication_authority_head_from_conn(&connection, job_id)
    }

    pub fn get_detail(&self, publication_id: &str) -> Result<Option<PublicationRecord>> {
        let connection = self.open_db()?;
        get_publication_detail_from_conn(&connection, publication_id)
    }

    pub fn get_detail_for_job(
        &self,
        job_id: &str,
        publication_id: &str,
    ) -> Result<Option<PublicationRecord>> {
        let connection = self.open_db()?;
        get_publication_detail_for_job_from_conn(&connection, job_id, publication_id)
    }

    /// List immutable readiness packages for every registered revision in one
    /// engagement. Generation remains scoped to the active job elsewhere.
    pub fn list_for_engagement(&self, engagement_id: &str) -> Result<EngagementPublicationList> {
        let connection = self.open_db()?;
        list_publications_for_engagement_from_conn(&connection, engagement_id)
    }

    /// Read one historical package only when its job belongs to the exact
    /// engagement's active context or immutable revision chain.
    pub fn get_detail_for_engagement(
        &self,
        engagement_id: &str,
        publication_id: &str,
    ) -> Result<Option<PublicationRecord>> {
        let connection = self.open_db()?;
        if !publication_belongs_to_engagement(&connection, engagement_id, publication_id)? {
            return Ok(None);
        }
        get_publication_detail_from_conn(&connection, publication_id)
    }

    /// Re-read one exact historical archive only after engagement scope,
    /// controlled-path, file identity, size, and physical digest replay.
    pub fn read_archive_for_engagement(
        &self,
        engagement_id: &str,
        publication_id: &str,
    ) -> Result<Option<(PublicationRecord, Vec<u8>)>> {
        let Some(record) = self.get_detail_for_engagement(engagement_id, publication_id)? else {
            return Ok(None);
        };
        let relative = Path::new(&record.archive_relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || !record
                .archive_relative_path
                .starts_with("publications/completed/")
        {
            return Err(KernelError::IntegrityFailed(
                "historical readiness archive path is outside its controlled namespace".to_owned(),
            ));
        }
        let path = self.workspace.join(relative);
        let parent = path.parent().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "historical readiness archive has no controlled parent".to_owned(),
            )
        })?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|error| KernelError::io(parent, error))?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(KernelError::IntegrityFailed(
                "historical readiness archive parent is not a real directory".to_owned(),
            ));
        }
        let workspace = fs::canonicalize(&self.workspace)
            .map_err(|error| KernelError::io(&self.workspace, error))?;
        let parent = fs::canonicalize(parent).map_err(|error| KernelError::io(parent, error))?;
        if !parent.starts_with(&workspace) {
            return Err(KernelError::IntegrityFailed(
                "historical readiness archive resolves outside the object workspace".to_owned(),
            ));
        }
        let mut file = open_archive_nofollow(&path)?;
        let metadata = file
            .metadata()
            .map_err(|error| KernelError::io(&path, error))?;
        if !metadata.is_file()
            || archive_link_count(&file, &path)? != 1
            || !archive_path_names_file(&path, &file)?
            || metadata.len() != record.package_size_bytes
            || metadata.len() > MAX_PACKAGE_SIZE_BYTES
        {
            return Err(KernelError::IntegrityFailed(
                "historical readiness archive identity or size differs from immutable evidence"
                    .to_owned(),
            ));
        }
        let capacity = usize::try_from(metadata.len()).map_err(|_| {
            KernelError::PolicyInvalid(
                "historical readiness archive does not fit the host address space".to_owned(),
            )
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        file.by_ref()
            .take(MAX_PACKAGE_SIZE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| KernelError::io(&path, error))?;
        if u64::try_from(bytes.len()).ok() != Some(record.package_size_bytes)
            || crate::canonical::sha256_hex(&bytes) != record.physical_archive_digest
        {
            return Err(KernelError::IntegrityFailed(
                "historical readiness archive digest differs from immutable evidence".to_owned(),
            ));
        }
        Ok(Some((record, bytes)))
    }
}

#[cfg(unix)]
fn open_archive_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| KernelError::io(path, error.into()))?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .is_file()
    {
        return Err(KernelError::IntegrityFailed(
            "historical readiness archive is not a regular file".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_archive_nofollow(path: &Path) -> Result<File> {
    crate::durability::open_file_nofollow(path).map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_archive_nofollow(path: &Path) -> Result<File> {
    Err(KernelError::IntegrityFailed(format!(
        "historical readiness archive no-follow reads are unsupported: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn archive_link_count(file: &File, path: &Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt as _;

    file.metadata()
        .map(|metadata| metadata.nlink())
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(windows)]
fn archive_link_count(file: &File, path: &Path) -> Result<u64> {
    crate::durability::file_link_count(file).map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn archive_link_count(_file: &File, path: &Path) -> Result<u64> {
    Err(KernelError::IntegrityFailed(format!(
        "historical readiness archive link checks are unsupported: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn archive_path_names_file(path: &Path, file: &File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    Ok(named.file_type().is_file() && named.dev() == opened.dev() && named.ino() == opened.ino())
}

#[cfg(windows)]
fn archive_path_names_file(path: &Path, file: &File) -> Result<bool> {
    crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn archive_path_names_file(path: &Path, _file: &File) -> Result<bool> {
    Err(KernelError::IntegrityFailed(format!(
        "historical readiness archive identity checks are unsupported: {}",
        path.display()
    )))
}

/// Bounded publication summary item for history list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationSummaryItem {
    pub publication_id: String,
    pub job_id: String,
    pub candidate_id: String,
    pub approval_id: String,
    pub status: String,
    pub practitioner: String,
    pub publication_reason: String,
    pub semantic_publication_hash: Option<String>,
    pub physical_archive_digest: Option<String>,
    pub package_size_bytes: Option<u64>,
    pub archive_relative_path: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Bounded publication list page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationListPage {
    pub schema_version: String,
    pub job_id: String,
    pub total_count: usize,
    pub items: Vec<PublicationSummaryItem>,
}

/// Bounded, value-free readiness-package history across immutable revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngagementPublicationList {
    pub schema_version: String,
    pub engagement_id: String,
    pub total_count: u64,
    pub returned_count: u64,
    pub truncated: bool,
    pub items: Vec<PublicationSummaryItem>,
}

pub fn list_publications_for_engagement_from_conn(
    connection: &Connection,
    engagement_id: &str,
) -> Result<EngagementPublicationList> {
    let total_count: u64 = connection.query_row(
        "SELECT COUNT(*)
         FROM publication_records r
         WHERE EXISTS (
             SELECT 1 FROM engagement_active_context a
             WHERE a.engagement_id = ?1 AND a.active_job_id = r.job_id
         ) OR EXISTS (
             SELECT 1 FROM engagement_tours t
             WHERE t.engagement_id = ?1 AND t.job_id = r.job_id
         )",
        [engagement_id],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT r.publication_id, r.job_id, r.candidate_id, r.approval_id,
                r.practitioner, r.publication_reason,
                r.semantic_publication_hash, r.physical_archive_digest,
                r.package_size_bytes, r.archive_relative_path, r.created_at
         FROM publication_records r
         WHERE EXISTS (
             SELECT 1 FROM engagement_active_context a
             WHERE a.engagement_id = ?1 AND a.active_job_id = r.job_id
         ) OR EXISTS (
             SELECT 1 FROM engagement_tours t
             WHERE t.engagement_id = ?1 AND t.job_id = r.job_id
         )
         ORDER BY r.created_at DESC, r.publication_id DESC
         LIMIT ?2",
    )?;
    let limit = i64::try_from(MAXIMUM_ENGAGEMENT_PUBLICATIONS).map_err(|_| {
        KernelError::PolicyInvalid("publication history bound is invalid".to_owned())
    })?;
    let items = statement
        .query_map(params![engagement_id, limit], |row| {
            Ok(PublicationSummaryItem {
                publication_id: row.get(0)?,
                job_id: row.get(1)?,
                candidate_id: row.get(2)?,
                approval_id: row.get(3)?,
                status: "COMPLETE".to_owned(),
                practitioner: row.get(4)?,
                publication_reason: row.get(5)?,
                semantic_publication_hash: Some(row.get(6)?),
                physical_archive_digest: Some(row.get(7)?),
                package_size_bytes: Some(row.get(8)?),
                archive_relative_path: Some(row.get(9)?),
                created_at: row.get(10)?,
                completed_at: Some(row.get(10)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let returned_count = u64::try_from(items.len()).map_err(|_| {
        KernelError::PolicyInvalid("publication history count does not fit u64".to_owned())
    })?;
    Ok(EngagementPublicationList {
        schema_version: ENGAGEMENT_PUBLICATION_LIST_SCHEMA.to_owned(),
        engagement_id: engagement_id.to_owned(),
        total_count,
        returned_count,
        truncated: returned_count < total_count,
        items,
    })
}

fn publication_belongs_to_engagement(
    connection: &Connection,
    engagement_id: &str,
    publication_id: &str,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM publication_records r
                 WHERE r.publication_id = ?2
                   AND (
                       EXISTS (
                           SELECT 1 FROM engagement_active_context a
                           WHERE a.engagement_id = ?1
                             AND a.active_job_id = r.job_id
                       )
                       OR EXISTS (
                           SELECT 1 FROM engagement_tours t
                           WHERE t.engagement_id = ?1 AND t.job_id = r.job_id
                       )
                   )
             )",
            params![engagement_id, publication_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

/// Bounded latest publication identity for assisted-runtime restart replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationAuthorityHead {
    pub total_count: u64,
    pub latest: Option<PublicationSummaryItem>,
}

pub fn publication_authority_head_from_conn(
    connection: &Connection,
    job_id: &str,
) -> Result<PublicationAuthorityHead> {
    let total_count = connection.query_row(
        "SELECT COUNT(*) FROM publication_attempts WHERE job_id = ?1",
        [job_id],
        |row| row.get::<_, u64>(0),
    )?;
    let latest = connection
        .query_row(
            "SELECT a.attempt_id, a.job_id, a.candidate_id, a.approval_id,
                    a.practitioner, a.publication_reason,
                    r.semantic_publication_hash, r.physical_archive_digest,
                    r.package_size_bytes, r.archive_relative_path, a.created_at,
                    CASE WHEN r.attempt_id IS NULL THEN 'PENDING' ELSE 'COMPLETE' END
             FROM publication_attempts a
             LEFT JOIN publication_records r ON r.attempt_id = a.attempt_id
             WHERE a.job_id = ?1
             ORDER BY a.created_at DESC, a.attempt_id DESC
             LIMIT 1",
            [job_id],
            |row| {
                Ok(PublicationSummaryItem {
                    publication_id: row.get(0)?,
                    job_id: row.get(1)?,
                    candidate_id: row.get(2)?,
                    approval_id: row.get(3)?,
                    status: row.get(11)?,
                    practitioner: row.get(4)?,
                    publication_reason: row.get(5)?,
                    semantic_publication_hash: row.get(6)?,
                    physical_archive_digest: row.get(7)?,
                    package_size_bytes: row.get(8)?,
                    archive_relative_path: row.get(9)?,
                    created_at: row.get(10)?,
                    completed_at: Some(row.get(10)?),
                })
            },
        )
        .optional()?;
    Ok(PublicationAuthorityHead {
        total_count,
        latest,
    })
}

/// Query publication eligibility from Connection.
pub fn get_publication_eligibility_from_conn(
    connection: &Connection,
    job_id: &str,
) -> Result<PublicationEligibility> {
    // 1. Fetch active approval head for job
    let mut stmt = connection.prepare(
        "SELECT a.action_id, a.action_hash, a.candidate_id, a.candidate_hash, c.snapshot_id, c.snapshot_hash,
                c.blocked_count, c.included_count, c.excluded_count, c.accepted_row_count
         FROM migration_result_actions a
         JOIN migration_result_candidates c ON a.candidate_id = c.candidate_id AND a.job_id = c.job_id
         WHERE a.job_id = ? AND a.action_kind = 'APPROVED'
         ORDER BY a.generation DESC, a.action_sequence DESC
         LIMIT 1"
    )?;

    let row = stmt
        .query_row([job_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, usize>(6)?,
                r.get::<_, usize>(7)?,
                r.get::<_, usize>(8)?,
                r.get::<_, usize>(9)?,
            ))
        })
        .optional()?;

    let evaluated_at = chrono::Utc::now().to_rfc3339();

    let Some((
        approval_id,
        approval_hash,
        candidate_id,
        candidate_hash,
        _snapshot_id,
        _snapshot_hash,
        blocked_count,
        included_rows,
        excluded_rows,
        total_accepted_rows,
    )) = row
    else {
        return Ok(PublicationEligibility {
            schema_version: PUBLICATION_ELIGIBILITY_SCHEMA.to_owned(),
            is_eligible: false,
            job_id: job_id.to_owned(),
            candidate_id: None,
            candidate_hash: None,
            approval_id: None,
            approval_hash: None,
            total_accepted_rows: 0,
            included_rows: 0,
            excluded_rows: 0,
            blocked_rows: 0,
            blockers: vec![
                "No active APPROVED migration-result candidate exists for this job.".to_owned(),
            ],
            warnings: vec![],
            required_acknowledgments: vec![],
            evaluated_at,
        });
    };

    // Verify approval action isn't revoked or superseded by subsequent action
    let active_head_id: Option<String> = connection
        .query_row(
            "SELECT action_id FROM migration_result_actions
         WHERE job_id = ?
         ORDER BY generation DESC, action_sequence DESC
         LIMIT 1",
            [job_id],
            |r| r.get(0),
        )
        .optional()?;

    let mut blockers = Vec::new();
    let warnings =
        vec!["Readiness publication generates a local deterministic package only.".to_owned()];
    let required_acknowledgments = vec![
        "I confirm that this readiness package is local readiness evidence only.".to_owned(),
        "I acknowledge that this package does not execute or authorize a target load.".to_owned(),
    ];

    if active_head_id.as_deref() != Some(&approval_id) {
        blockers.push("The approval is not the current active authority head.".to_owned());
    }

    if blocked_count > 0 {
        blockers.push(format!(
            "Candidate contains {blocked_count} unresolved blocking row(s)."
        ));
    }

    let is_eligible = blockers.is_empty();

    Ok(PublicationEligibility {
        schema_version: PUBLICATION_ELIGIBILITY_SCHEMA.to_owned(),
        is_eligible,
        job_id: job_id.to_owned(),
        candidate_id: Some(candidate_id),
        candidate_hash: Some(candidate_hash),
        approval_id: Some(approval_id),
        approval_hash: Some(approval_hash),
        total_accepted_rows,
        included_rows,
        excluded_rows,
        blocked_rows: blocked_count,
        blockers,
        warnings,
        required_acknowledgments,
        evaluated_at,
    })
}

/// List publication attempts from Connection.
pub fn list_publications_from_conn(
    connection: &Connection,
    job_id: &str,
) -> Result<PublicationListPage> {
    let mut stmt = connection.prepare(
        "SELECT a.attempt_id, a.job_id, a.candidate_id, a.approval_id,
                a.practitioner, a.publication_reason,
                r.semantic_publication_hash, r.physical_archive_digest,
                r.package_size_bytes, r.archive_relative_path, a.created_at
         FROM publication_attempts a
         LEFT JOIN publication_records r ON r.attempt_id = a.attempt_id
         WHERE a.job_id = ?
         ORDER BY a.created_at DESC",
    )?;

    let items = stmt
        .query_map([job_id], |r| {
            Ok(PublicationSummaryItem {
                publication_id: r.get(0)?,
                job_id: r.get(1)?,
                candidate_id: r.get(2)?,
                approval_id: r.get(3)?,
                status: "COMPLETE".to_owned(),
                practitioner: r.get(4)?,
                publication_reason: r.get(5)?,
                semantic_publication_hash: r.get(6)?,
                physical_archive_digest: r.get(7)?,
                package_size_bytes: r.get(8)?,
                archive_relative_path: r.get(9)?,
                created_at: r.get(10)?,
                completed_at: Some(r.get(10)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let total_count = items.len();

    Ok(PublicationListPage {
        schema_version: PUBLICATION_LIST_SCHEMA.to_owned(),
        job_id: job_id.to_owned(),
        total_count,
        items,
    })
}

/// Get detailed publication record by publication_id from Connection.
pub fn get_publication_detail_from_conn(
    connection: &Connection,
    publication_id: &str,
) -> Result<Option<PublicationRecord>> {
    get_publication_detail_scoped_from_conn(connection, publication_id, None)
}

/// Get one detailed publication record only when it belongs to the selected job.
pub fn get_publication_detail_for_job_from_conn(
    connection: &Connection,
    job_id: &str,
    publication_id: &str,
) -> Result<Option<PublicationRecord>> {
    get_publication_detail_scoped_from_conn(connection, publication_id, Some(job_id))
}

fn get_publication_detail_scoped_from_conn(
    connection: &Connection,
    publication_id: &str,
    job_id: Option<&str>,
) -> Result<Option<PublicationRecord>> {
    let row = connection.query_row(
        "SELECT publication_id, job_id, candidate_id, candidate_hash, approval_id, approval_hash,
                contract_version, semantic_publication_hash, physical_archive_digest, package_size_bytes,
                member_count, included_row_count, excluded_row_count, archive_relative_path,
                practitioner, publication_reason, canonical_manifest_json, created_at
         FROM publication_records
         WHERE publication_id = ?1 AND (?2 IS NULL OR job_id = ?2)",
        rusqlite::params![publication_id, job_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, u64>(9)?,
                r.get::<_, usize>(10)?,
                r.get::<_, usize>(11)?,
                r.get::<_, usize>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(14)?,
                r.get::<_, String>(15)?,
                r.get::<_, String>(16)?,
                r.get::<_, String>(17)?,
            ))
        },
    ).optional()?;

    let Some((
        pub_id,
        job_id,
        candidate_id,
        candidate_hash,
        approval_id,
        approval_hash,
        contract_version,
        semantic_hash,
        physical_digest,
        package_size,
        member_count,
        included_count,
        excluded_count,
        archive_path,
        practitioner,
        reason,
        manifest_json,
        created_at,
    )) = row
    else {
        return Ok(None);
    };

    let manifest: PublicationManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| KernelError::Reconciliation(format!("Invalid manifest json: {e}")))?;

    Ok(Some(PublicationRecord {
        schema_version: PUBLICATION_DETAIL_SCHEMA.to_owned(),
        publication_id: pub_id,
        job_id,
        candidate_id,
        candidate_hash,
        approval_id,
        approval_hash,
        contract_version,
        status: PublicationStatus::Complete,
        practitioner,
        publication_reason: reason,
        semantic_publication_hash: semantic_hash,
        physical_archive_digest: physical_digest,
        package_size_bytes: package_size,
        member_count,
        included_row_count: included_count,
        excluded_row_count: excluded_count,
        archive_relative_path: archive_path,
        manifest,
        created_at: created_at.clone(),
        completed_at: created_at,
    }))
}

// Helper trait for optional query row result
trait OptionalExt<T> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::open_archive_nofollow;
    use std::{process::Command, time::Duration};

    #[test]
    fn historical_archive_fifo_is_rejected_without_waiting_for_a_writer() {
        let temporary = tempfile::tempdir().expect("temporary archive directory");
        let fifo = temporary.path().join("historical-readiness.zip");
        let status = Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .expect("create FIFO fixture");
        assert!(status.success(), "mkfifo fixture command failed");

        let started = std::time::Instant::now();
        let error = open_archive_nofollow(&fifo).expect_err("FIFO must fail closed");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "FIFO read waited for a writer"
        );
        assert!(error.to_string().contains("not a regular file"));
    }
}

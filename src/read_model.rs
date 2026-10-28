//! Bounded, read-only practitioner contracts for WP-003.
//!
//! Every source-facing query resolves through accepted `SQLite` lineage. This
//! module never opens an accepted ZIP, live `.part`, or inert tombstone and it
//! never calls the writer/migration path. The structs are presentation-neutral
//! application contracts suitable for both the CLI and a future local API.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, decode_cursor, encode_cursor, parse_unique_json, sha256_hex},
    durability::path_uses_tombstone_namespace,
    mapping::{
        CanonicalFieldIdentity, MappingDocument, MappingKind, RequiredCanonicalFieldIdentity,
        load_stored_mapping,
    },
    profile_registry::{inspect_profile_connection, load_verified_profile_document},
    validation::{verify_validation_issue_page_for_read, verify_validation_run_for_read},
};

pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAXIMUM_PAGE_SIZE: u32 = 200;
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAXIMUM_QUERY_TIMEOUT: Duration = Duration::from_mins(1);

const DATABASE_FILENAME: &str = "state.sqlite3";
const REQUIRED_SCHEMA_VERSION: i64 = crate::state::SCHEMA_VERSION;
const MAXIMUM_ROW_ISSUES: usize = 200;
const MAXIMUM_ROW_DETAIL_COLUMNS: usize = 4_096;
const MAXIMUM_ROW_DETAIL_HEADER_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_ROW_DETAIL_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_ROW_DETAIL_VALUE_CHARS: usize = 1024 * 1024;
const MAXIMUM_ROW_DETAIL_HEADER_CHARS: usize = 1_024;

const JOB_SUMMARY_SCHEMA: &str = "migration-stream/read/job-summary/v1";
const FILE_MANIFEST_SCHEMA: &str = "migration-stream/read/file-manifest/v1";
const PROFILING_SUMMARY_SCHEMA: &str = "migration-stream/read/profiling-summary/v1";
const PROFILE_VIEW_SCHEMA: &str = "migration-stream/read/profile-view/v1";
const MAPPING_COVERAGE_SCHEMA: &str = "migration-stream/read/mapping-coverage/v1";
const VALIDATION_SUMMARY_SCHEMA: &str = "migration-stream/read/validation-summary/v1";
const ISSUE_LIST_SCHEMA: &str = "migration-stream/read/issue-list/v1";
const ROW_DETAIL_SCHEMA: &str = "migration-stream/read/row-detail/v1";
const RECONCILIATION_SCHEMA: &str = "migration-stream/read/reconciliation-summary/v1";
const OPERATIONAL_CLEANUP_SCHEMA: &str = "migration-stream/read/operational-cleanup/v1";

const FILE_MANIFEST_CURSOR: &str = "file-manifest/v1";
const PROFILING_SUMMARY_CURSOR: &str = "profiling-summary/v1";
const MAPPING_COVERAGE_CURSOR: &str = "mapping-coverage/v1";
const ISSUE_LIST_CURSOR: &str = "issue-list/v1";
const RECONCILIATION_CURSOR: &str = "reconciliation-summary/v1";
const OPERATIONAL_CLEANUP_CURSOR: &str = "operational-cleanup/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageInfo {
    pub page_size: u32,
    pub returned: u32,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedSourceSummary {
    pub source_name: String,
    pub source_hash: String,
    pub source_size: u64,
    pub stored_archive: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobTimestamps {
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobCounts {
    pub accepted_file_count: u64,
    pub accepted_row_count: u64,
    pub accepted_field_count: u64,
    pub pending_row_count: u64,
    pub profiling_run_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationStatus {
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
    pub closed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedGovernance {
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub validation_run_id: String,
    pub validation_state: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalWarning {
    pub code: String,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobSummaryData {
    pub job_id: String,
    pub state: String,
    pub accepted_source: Option<AcceptedSourceSummary>,
    pub timestamps: JobTimestamps,
    pub counts: JobCounts,
    pub reconciliation: ReconciliationStatus,
    pub integrity_status: String,
    pub selected_governance: Option<SelectedGovernance>,
    pub warnings: Vec<OperationalWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobSummary {
    pub schema_version: String,
    pub data: JobSummaryData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedJobScope {
    pub job_id: String,
    pub source_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileManifestItem {
    pub file_id: String,
    pub path: String,
    pub source_ordinal: u64,
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub row_count: u64,
    pub column_count: u64,
    pub ingestion_state: String,
    pub profiling_state: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileManifestPage {
    pub schema_version: String,
    pub scope: AcceptedJobScope,
    pub items: Vec<FileManifestItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingScope {
    pub run_id: String,
    pub job_id: String,
    pub result_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingSampleView {
    pub sha256: String,
    pub utf8_bytes: u64,
    pub decoded_length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingSummaryItem {
    pub path: String,
    pub column_ordinal: u64,
    pub column_name: String,
    pub empty_count: u64,
    pub nonempty_count: u64,
    pub min_decoded_length: Option<u64>,
    pub max_decoded_length: Option<u64>,
    pub oversized_count: u64,
    pub distinct_status: String,
    pub distinct_count: Option<u64>,
    pub sample_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<Vec<ProfilingSampleView>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingSummaryPage {
    pub schema_version: String,
    pub scope: ProfilingScope,
    pub include_samples: bool,
    pub items: Vec<ProfilingSummaryItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApprovalView {
    pub approval_id: String,
    pub approver: String,
    pub reason: String,
    pub acknowledged: bool,
    pub approved_at: String,
    pub approval_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileLifecycleEventView {
    pub sequence: u64,
    pub from_state: Option<String>,
    pub to_state: String,
    pub actor: String,
    pub reason: String,
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
    pub occurred_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileViewData {
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub schema_hash: String,
    pub document: Value,
    pub declared_status: String,
    pub validation_hash: String,
    pub registered_at: String,
    pub lifecycle_state: String,
    pub approvals: Vec<ProfileApprovalView>,
    pub lifecycle_events: Vec<ProfileLifecycleEventView>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileView {
    pub schema_version: String,
    pub data: ProfileViewData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MappingIdentity {
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub source_hash: String,
    pub validated_job_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MappingCoverageItem {
    pub kind: String,
    pub mapping_kind: Option<String>,
    pub source_file: Option<String>,
    pub source_column_ordinal: Option<u64>,
    pub source_column_name: Option<String>,
    pub entity_id: Option<String>,
    pub field_id: Option<String>,
    pub binding_id: Option<String>,
    pub issue_code: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MappingCoveragePage {
    pub schema_version: String,
    pub scope: MappingCoverageScope,
    pub mapping: MappingIdentity,
    pub items: Vec<MappingCoverageItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MappingCoverageScope {
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroupedIssueCount {
    pub key: String,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueCountGroup {
    pub items: Vec<GroupedIssueCount>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationIssueBreakdowns {
    pub by_severity: IssueCountGroup,
    pub by_rule: IssueCountGroup,
    pub by_entity: IssueCountGroup,
    pub by_field: IssueCountGroup,
    pub by_source_file: IssueCountGroup,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationSummaryData {
    pub run_id: String,
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
    pub state: String,
    pub result_hash: Option<String>,
    pub issue_count: u64,
    pub rows_processed: u64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub timestamps: ValidationTimestamps,
    pub issue_breakdowns: ValidationIssueBreakdowns,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationTimestamps {
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationSummary {
    pub schema_version: String,
    pub data: ValidationSummaryData,
    pub canonical_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueSeverity {
    Error,
    Warning,
    Information,
}

impl IssueSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
            Self::Information => "INFORMATION",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueFilters {
    pub severity: Option<IssueSeverity>,
    pub rule_id: Option<String>,
    pub entity_id: Option<String>,
    pub field_id: Option<String>,
    pub source_file: Option<String>,
    pub source_ordinal: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationIdentity {
    pub run_id: String,
    pub identity_hash: String,
    pub job_id: String,
    pub source_hash: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub state: String,
    pub result_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueListItem {
    pub issue_id: String,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub issue_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueListPage {
    pub schema_version: String,
    pub scope: ValidationIdentity,
    pub filters: IssueFilters,
    pub items: Vec<IssueListItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceValue {
    pub column_ordinal: u64,
    pub column_name: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RowDetailData {
    pub run_id: String,
    pub job_id: String,
    pub source_hash: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub source_values: Vec<SourceValue>,
    pub canonical_projection: Vec<CanonicalProjectionItem>,
    pub issues: Vec<RowIssueItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalLineageView {
    pub kind: String,
    pub binding_id: String,
    pub source_column_ordinal: Option<u64>,
    pub source_column_name: Option<String>,
    pub lookup_table_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalProjectionItem {
    pub entity_id: String,
    pub field_id: String,
    pub value: Option<String>,
    pub lineage: CanonicalLineageView,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RowIssueItem {
    pub issue_id: String,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    pub issue_hash: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RowDetail {
    pub schema_version: String,
    pub data: RowDetailData,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationScope {
    pub job_id: String,
    pub source_hash: String,
    pub validation_run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationAggregate {
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
    pub closed: bool,
    pub validation_rows_processed: Option<u64>,
    pub validation_issue_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileReconciliationItem {
    pub path: String,
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
    pub closed: bool,
    pub validation_issue_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationSummary {
    pub schema_version: String,
    pub scope: ReconciliationScope,
    pub aggregate: ReconciliationAggregate,
    pub items: Vec<FileReconciliationItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupFilters {
    pub namespace: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalCleanupItem {
    pub cleanup_id: String,
    pub namespace: String,
    pub job_id: Option<String>,
    pub state: String,
    pub cleanup_attempts: u64,
    pub last_error_code: Option<String>,
    pub diagnostic_present: bool,
    pub created_at: String,
    pub updated_at: String,
    pub quarantined_at: Option<String>,
    pub cleaned_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalCleanupPage {
    pub schema_version: String,
    pub scope: CleanupScope,
    pub items: Vec<OperationalCleanupItem>,
    pub page: PageInfo,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupScope {
    pub states: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ReadService {
    workspace: PathBuf,
    query_timeout: Duration,
}

impl ReadService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            query_timeout: DEFAULT_QUERY_TIMEOUT,
        }
    }

    /// Construct a read service with an explicit bounded execution deadline.
    ///
    /// # Errors
    ///
    /// Returns a read-model error when the deadline is zero or exceeds the
    /// supported maximum.
    pub fn with_query_timeout(
        workspace: impl Into<PathBuf>,
        query_timeout: Duration,
    ) -> Result<Self> {
        if query_timeout.is_zero() || query_timeout > MAXIMUM_QUERY_TIMEOUT {
            return contract_error(format!(
                "query timeout must be greater than zero and no more than {} seconds",
                MAXIMUM_QUERY_TIMEOUT.as_secs()
            ));
        }
        Ok(Self {
            workspace: workspace.into(),
            query_timeout,
        })
    }

    /// Return one bounded job summary without reading source-package bytes.
    ///
    /// # Errors
    ///
    /// Returns a request, not-found, integrity, or read-only persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn job_summary(&self, job_id: &str) -> Result<JobSummary> {
        validate_identifier(job_id, "job ID")?;
        let connection = self.open_snapshot()?;
        let row = connection
            .query_row(
                "SELECT state, source_name, source_hash, source_size, stored_archive,
                        source_records, stored_records, pending_records,
                        created_at, updated_at, completed_at
                 FROM jobs WHERE id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| not_found("job", job_id))?;
        let source_records = nonnegative(row.5, "job source records")?;
        let stored_records = nonnegative(row.6, "job stored records")?;
        let pending_records = nonnegative(row.7, "job pending records")?;
        let accepted_source = if row.0 == "COMPLETE" {
            let source_hash = row.2.as_deref().ok_or_else(|| {
                integrity_error_value(format!("complete job {job_id} has no source hash"))
            })?;
            let source_size = optional_nonnegative(row.3, "job source size")?.ok_or_else(|| {
                integrity_error_value(format!("complete job {job_id} has no source size"))
            })?;
            let stored_archive = row.4.as_deref().ok_or_else(|| {
                integrity_error_value(format!("complete job {job_id} has no stored archive"))
            })?;
            validate_accepted_archive(source_hash, stored_archive)?;
            Some(AcceptedSourceSummary {
                source_name: row.1,
                source_hash: source_hash.to_owned(),
                source_size,
                stored_archive: stored_archive.to_owned(),
            })
        } else {
            None
        };
        let accepted = row.0 == "COMPLETE";
        let accepted_file_count = if accepted {
            query_count(
                &connection,
                "SELECT COUNT(*) FROM csv_files WHERE job_id = ?1",
                job_id,
                "accepted file count",
            )?
        } else {
            0
        };
        let accepted_field_count = if accepted {
            accepted_field_count(&connection, job_id)?
        } else {
            0
        };
        let profiling_run_count = query_count(
            &connection,
            "SELECT COUNT(*) FROM profiling_runs WHERE job_id = ?1",
            job_id,
            "profiling run count",
        )?;
        let reconciliation_closed =
            accepted && source_records == stored_records && pending_records == 0;
        let mut response = JobSummary {
            schema_version: JOB_SUMMARY_SCHEMA.to_owned(),
            data: JobSummaryData {
                job_id: job_id.to_owned(),
                state: row.0,
                accepted_source,
                timestamps: JobTimestamps {
                    created_at: row.8,
                    updated_at: row.9,
                    completed_at: row.10,
                },
                counts: JobCounts {
                    accepted_file_count,
                    accepted_row_count: if accepted { stored_records } else { 0 },
                    accepted_field_count,
                    pending_row_count: pending_records,
                    profiling_run_count,
                },
                reconciliation: ReconciliationStatus {
                    source_records,
                    stored_records,
                    pending_records,
                    closed: reconciliation_closed,
                },
                integrity_status: "NOT_CHECKED".to_owned(),
                selected_governance: latest_governance(&connection, job_id)?,
                warnings: job_warnings(&connection, job_id)?,
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return one deterministic page of accepted CSV files only.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, integrity, or read-only persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn file_manifest(&self, job_id: &str, page: &PageRequest) -> Result<FileManifestPage> {
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let accepted = load_accepted_job(&connection, job_id)?;
        let scope = AcceptedJobScope {
            job_id: job_id.to_owned(),
            source_hash: accepted.source_hash.clone(),
        };
        let scope_hash = canonical_hash(&serde_json::to_value(&scope)?)?;
        let filter_hash = canonical_hash(&json!({}))?;
        let (after_path, cursor_ordinal) = page.cursor.as_deref().map_or_else(
            || Ok((String::new(), 0_u64)),
            |cursor| {
                let payload =
                    decode_cursor(cursor, FILE_MANIFEST_CURSOR, &scope_hash, &filter_hash)?;
                cursor_string_u64(&payload.last_key, "file manifest")
            },
        )?;
        let previous_ordinal = if page.cursor.is_some() {
            if after_path.is_empty() || cursor_ordinal == 0 {
                return contract_error("file manifest cursor has an invalid last key");
            }
            reject_tombstone_path(&after_path, "file manifest cursor path")?;
            let (authoritative_ordinal, matching_path): (i64, i64) = connection.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN path = ?2 THEN 1 ELSE 0 END), 0)
                 FROM csv_files WHERE job_id = ?1 AND path <= ?2",
                params![job_id, after_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let authoritative_ordinal =
                nonnegative(authoritative_ordinal, "file manifest cursor ordinal")?;
            if matching_path != 1 || authoritative_ordinal != cursor_ordinal {
                return contract_error(
                    "file manifest cursor does not identify its authoritative accepted-file ordinal",
                );
            }
            authoritative_ordinal
        } else {
            0
        };
        let profiling_state: Option<String> = connection
            .query_row(
                "SELECT state FROM profiling_runs WHERE job_id = ?1
                 ORDER BY created_at DESC, id DESC LIMIT 1",
                [job_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut statement = connection.prepare(
            "SELECT c.path, a.compressed_size, a.expanded_size, c.header_json,
                    c.source_records, c.stored_records
             FROM csv_files c
             JOIN archive_entries a ON a.job_id = c.job_id AND a.path = c.path
             WHERE c.job_id = ?1 AND c.path > ?2
             ORDER BY c.path LIMIT ?3",
        )?;
        let mapped = statement.query_map(
            params![job_id, after_path, sqlite_limit(page_size)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for (offset, row) in mapped.enumerate() {
            let row = row?;
            reject_tombstone_path(&row.0, "accepted file")?;
            let source_records = nonnegative(row.4, "file source records")?;
            let stored_records = nonnegative(row.5, "file stored records")?;
            if source_records != stored_records {
                return integrity_error(format!(
                    "complete job {job_id} has unreconciled file {:?}",
                    row.0
                ));
            }
            let header_json = row.3.ok_or_else(|| {
                integrity_error_value(format!("accepted file {:?} has no header evidence", row.0))
            })?;
            let header: Vec<String> = serde_json::from_str(&header_json)?;
            let offset = u64::try_from(offset)
                .map_err(|_| integrity_error_value("file page offset does not fit in u64"))?;
            let source_ordinal = previous_ordinal
                .checked_add(offset)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| integrity_error_value("file source ordinal overflow"))?;
            items.push(FileManifestItem {
                file_id: source_file_id(&accepted.source_hash, &row.0),
                path: row.0,
                source_ordinal,
                compressed_bytes: nonnegative(row.1, "compressed file bytes")?,
                expanded_bytes: nonnegative(row.2, "expanded file bytes")?,
                row_count: stored_records,
                column_count: u64::try_from(header.len())
                    .map_err(|_| integrity_error_value("file column count does not fit u64"))?,
                ingestion_state: "COMPLETE".to_owned(),
                profiling_state: profiling_state
                    .clone()
                    .unwrap_or_else(|| "NOT_RUN".to_owned()),
            });
        }
        let next_cursor = split_next_cursor(
            &mut items,
            page_size,
            |last| json!([last.path, last.source_ordinal]),
            FILE_MANIFEST_CURSOR,
            &scope_hash,
            &filter_hash,
        )?;
        let mut response = FileManifestPage {
            schema_version: FILE_MANIFEST_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return persisted textual profiling metrics with samples redacted unless
    /// the caller explicitly requests them.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, not-found, integrity, or persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn profiling_summary(
        &self,
        run_id: &str,
        include_samples: bool,
        page: &PageRequest,
    ) -> Result<ProfilingSummaryPage> {
        validate_identifier(run_id, "profiling run ID")?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let (job_id, state, result_hash, updated_at): (String, String, Option<String>, String) =
            connection
                .query_row(
                    "SELECT job_id, state, result_hash, updated_at
                     FROM profiling_runs WHERE id = ?1",
                    [run_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or_else(|| not_found("profiling run", run_id))?;
        if state != "COMPLETE" {
            return contract_error(format!(
                "profiling run {run_id} is {state}; summary evidence requires COMPLETE"
            ));
        }
        let result_hash = result_hash.ok_or_else(|| {
            integrity_error_value(format!(
                "complete profiling run {run_id} has no result hash"
            ))
        })?;
        validate_hash(&result_hash, "profiling result hash")?;
        load_accepted_job(&connection, &job_id)?;
        let scope = ProfilingScope {
            run_id: run_id.to_owned(),
            job_id,
            result_hash,
        };
        let scope_hash = canonical_hash(&json!({
            "scope": scope,
            "state": state,
            "updated_at": updated_at,
        }))?;
        let filter_hash = canonical_hash(&json!({"include_samples": include_samples}))?;
        let (after_path, after_ordinal) = page.cursor.as_deref().map_or_else(
            || Ok((String::new(), -1_i64)),
            |cursor| {
                let payload =
                    decode_cursor(cursor, PROFILING_SUMMARY_CURSOR, &scope_hash, &filter_hash)?;
                cursor_string_i64(&payload.last_key, "profiling summary")
            },
        )?;
        let mut statement = connection.prepare(
            "SELECT path, column_ordinal, column_name, empty_count, nonempty_count,
                    min_decoded_length, max_decoded_length, oversized_count,
                    distinct_mode, distinct_count, samples_json
             FROM profiling_columns
             WHERE run_id = ?1
               AND (path > ?2 OR (path = ?2 AND column_ordinal > ?3))
             ORDER BY path, column_ordinal LIMIT ?4",
        )?;
        let mapped = statement.query_map(
            params![run_id, after_path, after_ordinal, sqlite_limit(page_size)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in mapped {
            let row = row?;
            reject_tombstone_path(&row.0, "profiled accepted file")?;
            let samples: Vec<ProfilingSampleView> = serde_json::from_str(&row.10)?;
            items.push(ProfilingSummaryItem {
                path: row.0,
                column_ordinal: nonnegative(row.1, "profile column ordinal")?,
                column_name: row.2,
                empty_count: nonnegative(row.3, "profile empty count")?,
                nonempty_count: nonnegative(row.4, "profile nonempty count")?,
                min_decoded_length: optional_nonnegative(row.5, "profile minimum length")?,
                max_decoded_length: optional_nonnegative(row.6, "profile maximum length")?,
                oversized_count: nonnegative(row.7, "profile oversized count")?,
                distinct_status: row.8,
                distinct_count: optional_nonnegative(row.9, "profile distinct count")?,
                sample_count: u64::try_from(samples.len()).map_err(|_| {
                    integrity_error_value("profiling sample count does not fit in u64")
                })?,
                samples: include_samples.then_some(samples),
            });
        }
        let next_cursor = split_next_cursor(
            &mut items,
            page_size,
            |last| json!([last.path, last.column_ordinal]),
            PROFILING_SUMMARY_CURSOR,
            &scope_hash,
            &filter_hash,
        )?;
        let mut response = ProfilingSummaryPage {
            schema_version: PROFILING_SUMMARY_SCHEMA.to_owned(),
            scope,
            include_samples,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Inspect one exact immutable profile version and its separate registry
    /// lifecycle evidence.
    ///
    /// # Errors
    ///
    /// Returns a request, not-found, integrity, or read-only persistence error.
    pub fn profile_view(
        &self,
        profile_id: &str,
        profile_version: &str,
        profile_hash: &str,
    ) -> Result<ProfileView> {
        validate_identifier(profile_id, "profile ID")?;
        validate_identifier(profile_version, "profile version")?;
        validate_hash(profile_hash, "profile hash")?;
        let connection = self.open_snapshot()?;
        let document =
            load_verified_profile_document(&connection, profile_id, profile_version, profile_hash)?;
        let verified =
            inspect_profile_connection(&connection, profile_id, profile_version, profile_hash)?;
        let lifecycle_state = verified.lifecycle_status.as_str().to_owned();
        let approvals = verified
            .approval
            .into_iter()
            .map(|approval| ProfileApprovalView {
                approval_id: approval.approval_id,
                approver: approval.approver,
                reason: approval.reason,
                acknowledged: approval.acknowledged,
                approved_at: approval.approved_at,
                approval_hash: approval.approval_hash,
            })
            .collect();
        let lifecycle_events = verified
            .lifecycle_events
            .into_iter()
            .map(|event| ProfileLifecycleEventView {
                sequence: event.sequence,
                from_state: event.from_state,
                to_state: event.to_state.as_str().to_owned(),
                actor: event.actor,
                reason: event.reason,
                previous_event_hash: event.previous_event_hash,
                event_hash: event.event_hash,
                occurred_at: event.occurred_at,
            })
            .collect();
        let mut response = ProfileView {
            schema_version: PROFILE_VIEW_SCHEMA.to_owned(),
            data: ProfileViewData {
                profile_id: profile_id.to_owned(),
                profile_version: profile_version.to_owned(),
                profile_hash: profile_hash.to_owned(),
                schema_hash: verified.schema_hash,
                document,
                declared_status: verified.declared_status,
                validation_hash: verified.validation_hash,
                registered_at: verified.registered_at,
                lifecycle_state,
                approvals,
                lifecycle_events,
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return a stable bounded page from immutable mapping coverage evidence.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, not-found, integrity, or persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn mapping_coverage(
        &self,
        mapping_id: &str,
        mapping_version: &str,
        mapping_hash: &str,
        page: &PageRequest,
    ) -> Result<MappingCoveragePage> {
        validate_identifier(mapping_id, "mapping ID")?;
        validate_identifier(mapping_version, "mapping version")?;
        validate_hash(mapping_hash, "mapping hash")?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let exact_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mapping_versions
                           WHERE mapping_id = ?1 AND mapping_version = ?2 AND mapping_hash = ?3)",
            params![mapping_id, mapping_version, mapping_hash],
            |row| row.get(0),
        )?;
        if !exact_exists {
            return Err(not_found("mapping version", mapping_id));
        }
        let verified = load_stored_mapping(&connection, mapping_id, mapping_version)?;
        if verified.mapping_hash != mapping_hash {
            return integrity_error(format!(
                "mapping {mapping_id} {mapping_version} hash differs from verified mapping evidence"
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
            String,
            String,
        ) = connection
            .query_row(
                "SELECT profile_id, profile_version, profile_hash, source_hash,
                        validated_job_id, attempt_id, document_json, coverage_json,
                        coverage_hash, created_at
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
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| not_found("mapping version", mapping_id))?;
        let accepted = load_accepted_job(&connection, &row.4)?;
        if accepted.source_hash != row.3 {
            return integrity_error(format!(
                "mapping {mapping_id} {mapping_version} source hash does not match its accepted job"
            ));
        }
        let identity = MappingIdentity {
            mapping_id: mapping_id.to_owned(),
            mapping_version: mapping_version.to_owned(),
            mapping_hash: mapping_hash.to_owned(),
            profile_id: row.0,
            profile_version: row.1,
            profile_hash: row.2,
            source_hash: row.3,
            validated_job_id: row.4,
        };
        let scope = MappingCoverageScope {
            mapping_id: mapping_id.to_owned(),
            mapping_version: mapping_version.to_owned(),
            mapping_hash: mapping_hash.to_owned(),
        };
        let document_value = parse_unique_json(row.6.as_bytes())?;
        if canonical_hash(&document_value)? != identity.mapping_hash {
            return integrity_error(format!(
                "mapping {mapping_id} {mapping_version} document hash mismatch"
            ));
        }
        let document: MappingDocument = serde_json::from_value(document_value)?;
        validate_mapping_document_identity(&connection, &document, &identity)?;
        let coverage_value = parse_unique_json(row.7.as_bytes())?;
        let coverage_hash = canonical_hash(&coverage_value)?;
        if coverage_hash != row.8 {
            return integrity_error(format!(
                "mapping {mapping_id} {mapping_version} coverage hash mismatch"
            ));
        }
        validate_mapping_coverage_identity(&coverage_value, &identity)?;
        let mut all_items = mapping_coverage_items(&coverage_value, &document)?;
        all_items.sort_by_key(mapping_coverage_key);
        let scope_hash = canonical_hash(&json!({
            "mapping": identity,
            "coverage_hash": coverage_hash,
        }))?;
        let filter_hash = canonical_hash(&json!({}))?;
        let after_index = page.cursor.as_deref().map_or_else(
            || Ok(0_usize),
            |cursor| {
                let payload =
                    decode_cursor(cursor, MAPPING_COVERAGE_CURSOR, &scope_hash, &filter_hash)?;
                cursor_offset(&payload.last_key, "mapping coverage")
            },
        )?;
        if after_index > all_items.len() {
            return contract_error("mapping coverage cursor is beyond the immutable item set");
        }
        let requested = usize::try_from(page_size).unwrap_or(usize::MAX);
        let end = after_index.saturating_add(requested).min(all_items.len());
        let items = all_items[after_index..end].to_vec();
        let next_cursor = if end < all_items.len() {
            Some(encode_cursor(
                MAPPING_COVERAGE_CURSOR,
                &scope_hash,
                &filter_hash,
                json!([end]),
            )?)
        } else {
            None
        };
        let mut response = MappingCoveragePage {
            schema_version: MAPPING_COVERAGE_SCHEMA.to_owned(),
            scope,
            mapping: identity,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return the lifecycle and bounded aggregate evidence for one exact
    /// validation attempt/run.
    ///
    /// # Errors
    ///
    /// Returns a request, not-found, integrity, or read-only persistence error.
    pub fn validation_summary(&self, run_id: &str) -> Result<ValidationSummary> {
        let connection = self.open_snapshot()?;
        let identity = load_validation_identity(&connection, run_id)?;
        ensure_validation_sources_accepted(&connection, &identity)?;
        let data = connection.query_row(
            "SELECT identity_hash, job_id, source_hash, profile_id, profile_version,
                    profile_hash, mapping_id, mapping_version, mapping_hash,
                    rule_set_hash, engine_version, policy_hash, state, result_hash,
                    issue_count, rows_processed, error_code, error_message,
                    created_at, updated_at, completed_at
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
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, Option<String>>(20)?,
                ))
            },
        )?;
        let issue_count = nonnegative(data.14, "validation issue count")?;
        let actual_issue_count = query_count(
            &connection,
            "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
            run_id,
            "validation issue row count",
        )?;
        if issue_count != actual_issue_count {
            return integrity_error(format!(
                "validation run {run_id} issue count {issue_count} differs from {actual_issue_count} rows"
            ));
        }
        let mut response = ValidationSummary {
            schema_version: VALIDATION_SUMMARY_SCHEMA.to_owned(),
            data: ValidationSummaryData {
                run_id: run_id.to_owned(),
                identity_hash: data.0,
                job_id: data.1,
                source_hash: data.2,
                profile_id: data.3,
                profile_version: data.4,
                profile_hash: data.5,
                mapping_id: data.6,
                mapping_version: data.7,
                mapping_hash: data.8,
                rule_set_hash: data.9,
                engine_version: data.10,
                policy_hash: data.11,
                state: data.12,
                result_hash: data.13,
                issue_count,
                rows_processed: nonnegative(data.15, "validation processed rows")?,
                error_code: data.16,
                error_message: data.17,
                timestamps: ValidationTimestamps {
                    created_at: data.18,
                    updated_at: data.19,
                    completed_at: data.20,
                },
                issue_breakdowns: ValidationIssueBreakdowns {
                    by_severity: grouped_issue_counts(&connection, run_id, "severity")?,
                    by_rule: grouped_issue_counts(&connection, run_id, "rule_id")?,
                    by_entity: grouped_issue_counts(&connection, run_id, "entity_id")?,
                    by_field: grouped_issue_counts(&connection, run_id, "field_id")?,
                    by_source_file: grouped_issue_counts(&connection, run_id, "source_file")?,
                },
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return a keyset page of issues. Filters are a closed equality-only set
    /// and the response intentionally excludes source and canonical values.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, not-found, integrity, or persistence error.
    pub fn issue_list(
        &self,
        run_id: &str,
        filters: &IssueFilters,
        page: &PageRequest,
    ) -> Result<IssueListPage> {
        validate_issue_filters(filters)?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let identity = load_validation_identity(&connection, run_id)?;
        if identity.state == "RUNNING" {
            return contract_error(
                "issue pagination is unavailable while validation membership is still changing",
            );
        }
        ensure_validation_sources_accepted(&connection, &identity)?;
        let scope_hash = validation_scope_hash(&identity)?;
        let filter_hash = canonical_hash(&serde_json::to_value(filters)?)?;
        let after_issue = page.cursor.as_deref().map_or_else(
            || Ok(String::new()),
            |cursor| {
                let payload = decode_cursor(cursor, ISSUE_LIST_CURSOR, &scope_hash, &filter_hash)?;
                cursor_string(&payload.last_key, "issue list")
            },
        )?;
        let mut sql = String::from(
            "SELECT issue_id, severity, rule_id, issue_code, display_message,
                    entity_id, field_id, source_file, source_ordinal,
                    line_start, line_end, issue_hash
             FROM validation_issues
             WHERE run_id = ? AND issue_id > ?",
        );
        let mut values = vec![
            SqlValue::Text(run_id.to_owned()),
            SqlValue::Text(after_issue),
        ];
        append_issue_filters(&mut sql, &mut values, filters)?;
        sql.push_str(" ORDER BY issue_id LIMIT ?");
        values.push(SqlValue::Integer(sqlite_limit(page_size)?));
        let mut statement = connection.prepare(&sql)?;
        let mapped = statement.query_map(params_from_iter(values), issue_from_row)?;
        let mut items = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
        for issue in &items {
            reject_tombstone_path(&issue.source_file, "issue source file")?;
        }
        verify_validation_issue_page_for_read(
            &connection,
            run_id,
            &items
                .iter()
                .map(|issue| issue.issue_id.clone())
                .collect::<Vec<_>>(),
        )?;
        let next_cursor = split_next_cursor(
            &mut items,
            page_size,
            |last| json!([last.issue_id]),
            ISSUE_LIST_CURSOR,
            &scope_hash,
            &filter_hash,
        )?;
        let mut response = IssueListPage {
            schema_version: ISSUE_LIST_SCHEMA.to_owned(),
            scope: identity,
            filters: filters.clone(),
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return one exact accepted-row context. This is the only practitioner
    /// contract that exposes source values, and it requires an exact run and
    /// accepted source lineage.
    ///
    /// # Errors
    ///
    /// Returns a request, not-found, integrity, mapping, or persistence error.
    #[allow(clippy::too_many_lines)] // Exact row shape, lineage, and issue bounds remain one auditable read contract.
    pub fn row_detail(
        &self,
        run_id: &str,
        source_file: &str,
        source_ordinal: u64,
    ) -> Result<RowDetail> {
        reject_tombstone_path(source_file, "row-detail source file")?;
        if source_file.is_empty() || source_file.chars().count() > 1_024 {
            return contract_error("row-detail source file exceeds the governed path bound");
        }
        if source_ordinal == 0 {
            return contract_error("row-detail source ordinal must be greater than zero");
        }
        let connection = self.open_snapshot()?;
        let identity = load_validation_identity(&connection, run_id)?;
        ensure_validation_sources_accepted(&connection, &identity)?;
        ensure_accepted_file(&connection, &identity.job_id, source_file)?;
        let ordinal = sqlite_u64(source_ordinal, "row-detail source ordinal")?;
        let sizes: Option<(Option<i64>, i64)> = connection
            .query_row(
                "SELECT length(CAST(c.header_json AS BLOB)),
                        length(CAST(r.fields_json AS BLOB))
                 FROM source_records r
                 JOIN csv_files c ON c.job_id = r.job_id AND c.path = r.file_path
                 WHERE r.job_id = ?1 AND r.file_path = ?2 AND r.source_ordinal = ?3",
                params![identity.job_id, source_file, ordinal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((header_bytes, source_bytes)) = sizes else {
            return Err(not_found(
                "accepted source row",
                &source_ordinal.to_string(),
            ));
        };
        let header_bytes = header_bytes.ok_or_else(|| {
            integrity_error_value("accepted row has no immutable header evidence")
        })?;
        let header_bytes = usize::try_from(header_bytes).unwrap_or(usize::MAX);
        let source_bytes = usize::try_from(source_bytes).unwrap_or(usize::MAX);
        if header_bytes > MAXIMUM_ROW_DETAIL_HEADER_BYTES
            || source_bytes > MAXIMUM_ROW_DETAIL_SOURCE_BYTES
        {
            return contract_error(format!(
                "row detail header/source bytes {header_bytes}/{source_bytes} exceed bounds {MAXIMUM_ROW_DETAIL_HEADER_BYTES}/{MAXIMUM_ROW_DETAIL_SOURCE_BYTES}"
            ));
        }
        let row: (String, String, i64, i64) = connection
            .query_row(
                "SELECT c.header_json, r.fields_json, r.line_start, r.line_end
                 FROM source_records r
                 JOIN csv_files c ON c.job_id = r.job_id AND c.path = r.file_path
                 WHERE r.job_id = ?1 AND r.file_path = ?2 AND r.source_ordinal = ?3",
                params![identity.job_id, source_file, ordinal],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| not_found("accepted source row", &source_ordinal.to_string()))?;
        let headers: Vec<String> = serde_json::from_str(&row.0)?;
        let values: Vec<String> = serde_json::from_str(&row.1)?;
        validate_row_detail_shape(&headers, &values, source_file, source_ordinal)?;
        let (line_start, line_end) = validate_row_detail_lines(row.2, row.3)?;
        let source_values = headers
            .into_iter()
            .zip(values)
            .enumerate()
            .map(|(index, (column_name, value))| {
                Ok(SourceValue {
                    column_ordinal: u64::try_from(index)
                        .map_err(|_| integrity_error_value("source column ordinal overflow"))?,
                    column_name,
                    value,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let projected = crate::mapping::project_row_for_validation(
            &connection,
            run_id,
            source_file,
            source_ordinal,
        )?;
        let canonical_projection = practitioner_projection(&projected)?;
        let mut statement = connection.prepare(
            "SELECT issue_id, severity, rule_id, issue_code, display_message,
                    entity_id, field_id, source_file, source_ordinal,
                    line_start, line_end, issue_hash
             FROM validation_issues
             WHERE run_id = ?1 AND source_file = ?2 AND source_ordinal = ?3
             ORDER BY issue_id LIMIT ?4",
        )?;
        let issue_limit = i64::try_from(MAXIMUM_ROW_ISSUES + 1).unwrap_or(i64::MAX);
        let mapped = statement.query_map(
            params![run_id, source_file, ordinal, issue_limit],
            issue_from_row,
        )?;
        let issues = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
        if issues.len() > MAXIMUM_ROW_ISSUES {
            return contract_error(format!(
                "row detail exceeds maximum associated issue count {MAXIMUM_ROW_ISSUES}"
            ));
        }
        verify_validation_issue_page_for_read(
            &connection,
            run_id,
            &issues
                .iter()
                .map(|issue| issue.issue_id.clone())
                .collect::<Vec<_>>(),
        )?;
        let mut response = RowDetail {
            schema_version: ROW_DETAIL_SCHEMA.to_owned(),
            data: RowDetailData {
                run_id: run_id.to_owned(),
                job_id: identity.job_id,
                source_hash: identity.source_hash,
                source_file: source_file.to_owned(),
                source_ordinal,
                line_start,
                line_end,
                source_values,
                canonical_projection,
                issues: issues.into_iter().map(row_issue).collect(),
            },
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return aggregate and per-file accepted-source reconciliation evidence.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, not-found, integrity, or persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn reconciliation_summary(
        &self,
        job_id: &str,
        validation_run_id: Option<&str>,
        page: &PageRequest,
    ) -> Result<ReconciliationSummary> {
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let accepted = load_accepted_job(&connection, job_id)?;
        let (source_records, stored_records, pending_records): (i64, i64, i64) = connection
            .query_row(
                "SELECT source_records, stored_records, pending_records
                 FROM jobs WHERE id = ?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let validation = validation_run_id
            .map(|run_id| load_validation_identity(&connection, run_id))
            .transpose()?;
        if let Some(identity) = &validation {
            ensure_validation_sources_accepted(&connection, identity)?;
            if identity.job_id != job_id || identity.source_hash != accepted.source_hash {
                return contract_error("validation run does not belong to the accepted job");
            }
        }
        let scope = ReconciliationScope {
            job_id: job_id.to_owned(),
            source_hash: accepted.source_hash,
            validation_run_id: validation_run_id.map(str::to_owned),
        };
        let scope_hash = canonical_hash(&json!({
            "scope": scope,
            "validation": validation,
        }))?;
        let filter_hash = canonical_hash(&json!({}))?;
        let after_path = page.cursor.as_deref().map_or_else(
            || Ok(String::new()),
            |cursor| {
                let payload =
                    decode_cursor(cursor, RECONCILIATION_CURSOR, &scope_hash, &filter_hash)?;
                cursor_string(&payload.last_key, "reconciliation summary")
            },
        )?;
        let mut statement = connection.prepare(
            "SELECT path, source_records, stored_records,
                    CASE WHEN ?2 IS NULL THEN NULL ELSE (
                        SELECT COUNT(*) FROM validation_issues vi
                        WHERE vi.run_id = ?2 AND vi.source_file = csv_files.path
                    ) END
             FROM csv_files
             WHERE job_id = ?1 AND path > ?3
             ORDER BY path LIMIT ?4",
        )?;
        let mapped = statement.query_map(
            params![
                job_id,
                validation_run_id,
                after_path,
                sqlite_limit(page_size)?
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in mapped {
            let row = row?;
            reject_tombstone_path(&row.0, "reconciled accepted file")?;
            let source = nonnegative(row.1, "file source records")?;
            let stored = nonnegative(row.2, "file stored records")?;
            let pending = source.checked_sub(stored).ok_or_else(|| {
                integrity_error_value(format!(
                    "file {:?} stored count exceeds source count",
                    row.0
                ))
            })?;
            items.push(FileReconciliationItem {
                path: row.0,
                source_records: source,
                stored_records: stored,
                pending_records: pending,
                closed: source == stored,
                validation_issue_count: optional_nonnegative(row.3, "file validation issues")?,
            });
        }
        let next_cursor = split_next_cursor(
            &mut items,
            page_size,
            |last| json!([last.path]),
            RECONCILIATION_CURSOR,
            &scope_hash,
            &filter_hash,
        )?;
        let validation_rows_processed = validation_run_id
            .map(|run_id| {
                connection.query_row(
                    "SELECT rows_processed FROM validation_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get::<_, i64>(0),
                )
            })
            .transpose()?
            .map(|value| nonnegative(value, "validation rows processed"))
            .transpose()?;
        let validation_issue_count = validation_run_id
            .map(|run_id| {
                connection.query_row(
                    "SELECT issue_count FROM validation_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get::<_, i64>(0),
                )
            })
            .transpose()?
            .map(|value| nonnegative(value, "validation issue count"))
            .transpose()?;
        let source_records = nonnegative(source_records, "job source records")?;
        let stored_records = nonnegative(stored_records, "job stored records")?;
        let pending_records = nonnegative(pending_records, "job pending records")?;
        let mut response = ReconciliationSummary {
            schema_version: RECONCILIATION_SCHEMA.to_owned(),
            scope,
            aggregate: ReconciliationAggregate {
                source_records,
                stored_records,
                pending_records,
                closed: source_records == stored_records && pending_records == 0,
                validation_rows_processed,
                validation_issue_count,
            },
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    /// Return bounded cleanup lifecycle evidence without exposing quarantine
    /// names, paths, contents, or messages.
    ///
    /// # Errors
    ///
    /// Returns a request, cursor, integrity, or read-only persistence error.
    #[allow(clippy::too_many_lines)]
    pub fn operational_cleanup(
        &self,
        filters: &CleanupFilters,
        page: &PageRequest,
    ) -> Result<OperationalCleanupPage> {
        validate_cleanup_filters(filters)?;
        let page_size = validate_page(page)?;
        let connection = self.open_snapshot()?;
        let scope = CleanupScope {
            states: filters.state.as_ref().map_or_else(
                || {
                    vec![
                        "PLANNED".to_owned(),
                        "QUARANTINED".to_owned(),
                        "CLEANUP_PENDING".to_owned(),
                        "CLEANED".to_owned(),
                        "CONFLICT".to_owned(),
                    ]
                },
                |state| vec![state.clone()],
            ),
        };
        let snapshot: (i64, Option<String>, Option<String>, i64) = connection.query_row(
            "SELECT COUNT(*), MAX(updated_at), MAX(cleanup_id),
                    COALESCE(SUM(cleanup_attempts), 0)
             FROM partial_cleanups",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let scope_hash = canonical_hash(&json!({
            "scope": &scope,
            "snapshot": {
                "count": nonnegative(snapshot.0, "cleanup count")?,
                "maximum_updated_at": snapshot.1,
                "maximum_cleanup_id": snapshot.2,
                "attempt_sum": nonnegative(snapshot.3, "cleanup attempt sum")?,
            }
        }))?;
        let filter_hash = canonical_hash(&serde_json::to_value(filters)?)?;
        let (after_namespace, after_id) = page.cursor.as_deref().map_or_else(
            || Ok((String::new(), String::new())),
            |cursor| {
                let payload = decode_cursor(
                    cursor,
                    OPERATIONAL_CLEANUP_CURSOR,
                    &scope_hash,
                    &filter_hash,
                )?;
                cursor_two_strings(&payload.last_key, "operational cleanup")
            },
        )?;
        let mut sql = String::from(
            "SELECT cleanup_id, namespace, job_id, state, cleanup_attempts,
                    last_error_code, last_error_message IS NOT NULL,
                    created_at, updated_at, quarantined_at, cleaned_at
             FROM partial_cleanups
             WHERE (namespace > ? OR (namespace = ? AND cleanup_id > ?))",
        );
        let mut values = vec![
            SqlValue::Text(after_namespace.clone()),
            SqlValue::Text(after_namespace),
            SqlValue::Text(after_id),
        ];
        if let Some(namespace) = &filters.namespace {
            sql.push_str(" AND namespace = ?");
            values.push(SqlValue::Text(namespace.clone()));
        }
        if let Some(state) = &filters.state {
            sql.push_str(" AND state = ?");
            values.push(SqlValue::Text(state.clone()));
        }
        sql.push_str(" ORDER BY namespace, cleanup_id LIMIT ?");
        values.push(SqlValue::Integer(sqlite_limit(page_size)?));
        let mut statement = connection.prepare(&sql)?;
        let mapped = statement.query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in mapped {
            let row = row?;
            items.push(OperationalCleanupItem {
                cleanup_id: row.0,
                namespace: row.1,
                job_id: row.2,
                state: row.3,
                cleanup_attempts: nonnegative(row.4, "cleanup attempts")?,
                diagnostic_present: row.5.is_some() || row.6,
                last_error_code: row.5,
                created_at: row.7,
                updated_at: row.8,
                quarantined_at: row.9,
                cleaned_at: row.10,
            });
        }
        let next_cursor = split_next_cursor(
            &mut items,
            page_size,
            |last| json!([last.namespace, last.cleanup_id]),
            OPERATIONAL_CLEANUP_CURSOR,
            &scope_hash,
            &filter_hash,
        )?;
        let mut response = OperationalCleanupPage {
            schema_version: OPERATIONAL_CLEANUP_SCHEMA.to_owned(),
            scope,
            page: page_info(page.page_size, items.len(), next_cursor)?,
            items,
            canonical_hash: String::new(),
        };
        response.canonical_hash = response_hash(&response)?;
        Ok(response)
    }

    fn open_snapshot(&self) -> Result<Connection> {
        let database_path = self.workspace.join(DATABASE_FILENAME);
        let mut connection = crate::state::open_read_only(&database_path)?;
        let deadline_started = Instant::now();
        let query_timeout = self.query_timeout;
        connection.progress_handler(
            100,
            Some(move || deadline_started.elapsed() >= query_timeout),
        )?;
        connection.execute_batch("BEGIN DEFERRED;")?;
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version != REQUIRED_SCHEMA_VERSION {
            return Err(KernelError::Conflict(format!(
                "practitioner reads require schema version {REQUIRED_SCHEMA_VERSION}; found {schema_version}"
            )));
        }
        Ok(connection)
    }
}

#[derive(Debug)]
struct AcceptedJob {
    source_hash: String,
}

fn load_accepted_job(connection: &Connection, job_id: &str) -> Result<AcceptedJob> {
    validate_identifier(job_id, "job ID")?;
    let row: (String, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT state, source_hash, stored_archive FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("job", job_id))?;
    if row.0 != "COMPLETE" {
        return contract_error(format!(
            "job {job_id} is {:?}; accepted-source reads require COMPLETE",
            row.0
        ));
    }
    let source_hash = row.1.ok_or_else(|| {
        integrity_error_value(format!("complete job {job_id} has no source hash"))
    })?;
    let stored_archive = row.2.ok_or_else(|| {
        integrity_error_value(format!("complete job {job_id} has no stored archive"))
    })?;
    validate_accepted_archive(&source_hash, &stored_archive)?;
    Ok(AcceptedJob { source_hash })
}

pub(crate) fn validate_accepted_archive(source_hash: &str, stored_archive: &str) -> Result<()> {
    validate_hash(source_hash, "accepted source hash")?;
    reject_tombstone_path(stored_archive, "accepted archive")?;
    let expected = format!("packages/{source_hash}.zip");
    if stored_archive != expected {
        return integrity_error(format!(
            "accepted archive path {stored_archive:?} does not equal {expected:?}"
        ));
    }
    Ok(())
}

fn ensure_accepted_file(connection: &Connection, job_id: &str, source_file: &str) -> Result<()> {
    reject_tombstone_path(source_file, "accepted source file")?;
    let accepted = connection
        .query_row(
            "SELECT c.source_records, c.stored_records
             FROM csv_files c JOIN jobs j ON j.id = c.job_id
             WHERE c.job_id = ?1 AND c.path = ?2 AND j.state = 'COMPLETE'",
            params![job_id, source_file],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("accepted source file", source_file))?;
    if accepted.0 != accepted.1 {
        return integrity_error(format!(
            "accepted source file {source_file:?} is not reconciled"
        ));
    }
    Ok(())
}

fn load_validation_identity(connection: &Connection, run_id: &str) -> Result<ValidationIdentity> {
    validate_identifier(run_id, "validation run ID")?;
    let identity = connection
        .query_row(
            "SELECT identity_hash, job_id, source_hash, profile_id, profile_version,
                    profile_hash, mapping_id, mapping_version, mapping_hash, state, result_hash
             FROM validation_runs WHERE id = ?1",
            [run_id],
            |row| {
                Ok(ValidationIdentity {
                    run_id: run_id.to_owned(),
                    identity_hash: row.get(0)?,
                    job_id: row.get(1)?,
                    source_hash: row.get(2)?,
                    profile_id: row.get(3)?,
                    profile_version: row.get(4)?,
                    profile_hash: row.get(5)?,
                    mapping_id: row.get(6)?,
                    mapping_version: row.get(7)?,
                    mapping_hash: row.get(8)?,
                    state: row.get(9)?,
                    result_hash: row.get(10)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| not_found("validation run", run_id))?;
    validate_hash(&identity.identity_hash, "validation identity hash")?;
    validate_hash(&identity.source_hash, "validation source hash")?;
    validate_hash(&identity.profile_hash, "validation profile hash")?;
    validate_hash(&identity.mapping_hash, "validation mapping hash")?;
    if let Some(result_hash) = &identity.result_hash {
        validate_hash(result_hash, "validation result hash")?;
    }
    Ok(identity)
}

fn ensure_validation_sources_accepted(
    connection: &Connection,
    identity: &ValidationIdentity,
) -> Result<()> {
    verify_validation_run_for_read(connection, &identity.run_id)?;
    let accepted = load_accepted_job(connection, &identity.job_id)?;
    if accepted.source_hash != identity.source_hash {
        return integrity_error(format!(
            "validation run {} source hash does not match its accepted job",
            identity.run_id
        ));
    }
    let mapping = load_stored_mapping(connection, &identity.mapping_id, &identity.mapping_version)
        .map_err(|error| {
            integrity_error_value(format!(
                "validation run {} mapping/authority replay failed: {error}",
                identity.run_id
            ))
        })?;
    let mapping_source_matches = mapping.document.source.package_sha256 == identity.source_hash;
    let exact_source_reuse = if mapping_source_matches {
        true
    } else {
        let reuse: Option<(String, String)> = connection
            .query_row(
                "SELECT source_reuse_id, source_reuse_hash
                 FROM validation_runs
                 WHERE id = ?1 AND source_reuse_id IS NOT NULL
                   AND source_reuse_hash IS NOT NULL",
                [&identity.run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        reuse.is_some_and(|(source_reuse_id, source_reuse_hash)| {
            crate::validation::verify_mapping_source_reuse_authority(
                connection,
                &source_reuse_id,
                &source_reuse_hash,
                &identity.job_id,
                &identity.source_hash,
                &identity.profile_id,
                &identity.profile_version,
                &identity.profile_hash,
                &identity.mapping_id,
                &identity.mapping_version,
                &identity.mapping_hash,
            )
            .is_ok()
        })
    };
    if mapping.mapping_hash != identity.mapping_hash
        || mapping.document.profile.profile_id != identity.profile_id
        || mapping.document.profile.version != identity.profile_version
        || mapping.document.profile.sha256 != identity.profile_hash
        || !exact_source_reuse
    {
        return integrity_error(format!(
            "validation run {} identity differs from its immutable mapping",
            identity.run_id
        ));
    }
    let invalid_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM validation_issues vi
         LEFT JOIN source_records r
           ON r.job_id = ?2
          AND r.file_path = vi.source_file
          AND r.source_ordinal = vi.source_ordinal
         WHERE vi.run_id = ?1
           AND (r.file_path IS NULL
                OR r.line_start != vi.line_start
                OR r.line_end != vi.line_end)",
        params![identity.run_id, identity.job_id],
        |row| row.get(0),
    )?;
    if invalid_count != 0 {
        return integrity_error(format!(
            "validation run {} has {invalid_count} issue rows outside accepted source lineage",
            identity.run_id
        ));
    }
    let mut paths = connection.prepare(
        "SELECT DISTINCT source_file FROM validation_issues WHERE run_id = ?1 ORDER BY source_file",
    )?;
    for path in paths.query_map([&identity.run_id], |row| row.get::<_, String>(0))? {
        reject_tombstone_path(&path?, "validation issue source file")?;
    }
    Ok(())
}

fn latest_governance(connection: &Connection, job_id: &str) -> Result<Option<SelectedGovernance>> {
    let value = connection
        .query_row(
            "SELECT profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash, id, state
             FROM validation_runs WHERE job_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            [job_id],
            |row| {
                Ok(SelectedGovernance {
                    profile_id: row.get(0)?,
                    profile_version: row.get(1)?,
                    profile_hash: row.get(2)?,
                    mapping_id: row.get(3)?,
                    mapping_version: row.get(4)?,
                    mapping_hash: row.get(5)?,
                    validation_run_id: row.get(6)?,
                    validation_state: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(value)
}

fn job_warnings(connection: &Connection, job_id: &str) -> Result<Vec<OperationalWarning>> {
    let mut statement = connection.prepare(
        "SELECT CASE
                    WHEN state = 'CLEANUP_PENDING' THEN 'CLEANUP_PENDING'
                    WHEN state = 'CONFLICT' THEN 'CLEANUP_CONFLICT'
                    ELSE 'QUARANTINE_EVIDENCE'
                END AS warning_code,
                COUNT(*)
         FROM partial_cleanups
         WHERE job_id = ?1 AND state != 'CLEANED'
         GROUP BY warning_code ORDER BY warning_code",
    )?;
    let mapped = statement.query_map([job_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    mapped
        .map(|row| {
            let row = row?;
            Ok(OperationalWarning {
                code: row.0,
                count: nonnegative(row.1, "operational warning count")?,
            })
        })
        .collect()
}

fn accepted_field_count(connection: &Connection, job_id: &str) -> Result<u64> {
    let mut statement =
        connection.prepare("SELECT path, header_json FROM csv_files WHERE job_id = ?1")?;
    let mut total = 0_u64;
    for row in statement.query_map([job_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })? {
        let (path, header) = row?;
        reject_tombstone_path(&path, "accepted file")?;
        let header = header.ok_or_else(|| {
            integrity_error_value(format!("accepted file {path:?} has no header evidence"))
        })?;
        let columns: Vec<String> = serde_json::from_str(&header)?;
        total =
            total
                .checked_add(u64::try_from(columns.len()).map_err(|_| {
                    integrity_error_value("accepted header count does not fit in u64")
                })?)
                .ok_or_else(|| integrity_error_value("accepted field count overflow"))?;
    }
    Ok(total)
}

fn validate_mapping_coverage_identity(value: &Value, identity: &MappingIdentity) -> Result<()> {
    let coverage: crate::mapping::MappingCoverage = serde_json::from_value(value.clone())?;
    if coverage.schema_version != crate::mapping::MAPPING_SCHEMA_VERSION
        || coverage.mapping_id != identity.mapping_id
        || coverage.mapping_version != identity.mapping_version
        || coverage.mapping_hash != identity.mapping_hash
        || coverage.profile.profile_id != identity.profile_id
        || coverage.profile.version != identity.profile_version
        || coverage.profile.sha256 != identity.profile_hash
        || coverage.source.package_sha256 != identity.source_hash
        || coverage.validated_job_id != identity.validated_job_id
    {
        return integrity_error("stored mapping coverage identity is inconsistent");
    }
    for source_file in &coverage.source.source_files {
        reject_tombstone_path(source_file, "mapping coverage source file")?;
    }
    Ok(())
}

fn validate_mapping_document_identity(
    connection: &Connection,
    document: &MappingDocument,
    identity: &MappingIdentity,
) -> Result<()> {
    if document.schema_version != crate::mapping::MAPPING_SCHEMA_VERSION
        || document.mapping_id != identity.mapping_id
        || document.version != identity.mapping_version
        || document.profile.profile_id != identity.profile_id
        || document.profile.version != identity.profile_version
        || document.profile.sha256 != identity.profile_hash
        || document.source.package_sha256 != identity.source_hash
    {
        return integrity_error("stored mapping document identity is inconsistent");
    }
    let mut accepted_headers = BTreeMap::new();
    for source_file in &document.source.source_files {
        reject_tombstone_path(source_file, "mapping document source file")?;
        ensure_accepted_file(connection, &identity.validated_job_id, source_file)?;
        let header_json: Option<String> = connection.query_row(
            "SELECT header_json FROM csv_files WHERE job_id = ?1 AND path = ?2",
            params![identity.validated_job_id, source_file],
            |row| row.get(0),
        )?;
        let header_json = header_json.ok_or_else(|| {
            integrity_error_value("accepted mapping source has no header evidence")
        })?;
        let header: Vec<String> = serde_json::from_str(&header_json)?;
        accepted_headers.insert(source_file.as_str(), header);
    }
    let accepted_sources = document
        .source
        .source_files
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if document.has_explicit_entity_scope() {
        let assigned_sources = document
            .source_entity_assignments
            .iter()
            .map(|assignment| assignment.source_file.as_str())
            .collect::<BTreeSet<_>>();
        if assigned_sources.len() != document.source_entity_assignments.len()
            || assigned_sources != accepted_sources
        {
            return integrity_error(
                "stored mapping source/entity assignments are missing or ambiguous",
            );
        }
    }
    let mut binding_ids = BTreeSet::new();
    for binding in &document.bindings {
        if !binding_ids.insert(binding.binding_id.as_str()) {
            return integrity_error("stored mapping document contains duplicate binding IDs");
        }
        if binding.kind == MappingKind::Constant {
            if binding.source_file.is_some()
                || binding.source_column.is_some()
                || binding.source_column_ordinal.is_some()
            {
                return integrity_error(
                    "stored constant mapping binding unexpectedly references source columns",
                );
            }
        } else {
            let source_file = binding.source_file.as_deref().ok_or_else(|| {
                integrity_error_value("stored mapping binding has no source file")
            })?;
            reject_tombstone_path(source_file, "mapping binding source file")?;
            let source_column = binding.source_column.as_deref().ok_or_else(|| {
                integrity_error_value("stored mapping binding has no source column")
            })?;
            let source_column_ordinal = binding.source_column_ordinal.ok_or_else(|| {
                integrity_error_value("stored mapping binding has no source column ordinal")
            })?;
            let header = accepted_headers.get(source_file).ok_or_else(|| {
                integrity_error_value("stored mapping binding references an unaccepted file")
            })?;
            if !accepted_sources.contains(source_file)
                || header.get(source_column_ordinal).map(String::as_str) != Some(source_column)
            {
                return integrity_error("stored mapping binding source identity is inconsistent");
            }
            let source_entity = document.entity_for_source_file(source_file)?;
            if source_entity != document.effective_binding_entity(binding) {
                return integrity_error(
                    "stored mapping binding source/entity route is inconsistent",
                );
            }
        }
    }
    Ok(())
}

fn mapping_kind_name(kind: MappingKind) -> &'static str {
    match kind {
        MappingKind::Direct => "direct",
        MappingKind::Constant => "constant",
        MappingKind::EmptyAsNull => "empty_as_null",
        MappingKind::Trim => "trim",
        MappingKind::UppercaseAscii => "uppercase_ascii",
        MappingKind::LowercaseAscii => "lowercase_ascii",
        MappingKind::Lookup => "lookup",
    }
}

#[allow(clippy::too_many_lines)]
fn mapping_coverage_items(
    value: &Value,
    document: &MappingDocument,
) -> Result<Vec<MappingCoverageItem>> {
    let coverage: crate::mapping::MappingCoverage = serde_json::from_value(value.clone())?;
    if coverage.profile != document.profile || coverage.source != document.source {
        return integrity_error("stored mapping coverage differs from its immutable document");
    }
    let explicit_scope = document.has_explicit_entity_scope();
    let mapped_fields = document
        .bindings
        .iter()
        .map(|binding| {
            if explicit_scope {
                CanonicalFieldIdentity::Scoped {
                    entity_id: document.effective_binding_entity(binding).to_owned(),
                    field_id: binding.target_field_id.clone(),
                }
            } else {
                CanonicalFieldIdentity::Legacy(binding.target_field_id.clone())
            }
        })
        .collect::<BTreeSet<_>>();
    let defaulted_fields = document
        .bindings
        .iter()
        .filter(|binding| binding.kind == MappingKind::Constant)
        .map(|binding| {
            if explicit_scope {
                CanonicalFieldIdentity::Scoped {
                    entity_id: document.effective_binding_entity(binding).to_owned(),
                    field_id: binding.target_field_id.clone(),
                }
            } else {
                CanonicalFieldIdentity::Legacy(binding.target_field_id.clone())
            }
        })
        .collect::<BTreeSet<_>>();
    if coverage
        .mapped_canonical_fields
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != mapped_fields
        || coverage
            .defaulted_canonical_fields
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != defaulted_fields
    {
        return integrity_error("stored mapping coverage differs from immutable bindings");
    }
    let mut items = Vec::new();
    for binding in &document.bindings {
        let source_column_ordinal = binding
            .source_column_ordinal
            .map(|ordinal| {
                u64::try_from(ordinal).map_err(|_| {
                    integrity_error_value("mapping source column ordinal does not fit u64")
                })
            })
            .transpose()?;
        items.push(MappingCoverageItem {
            kind: if binding.kind == MappingKind::Constant {
                "DEFAULTED".to_owned()
            } else {
                "MAPPED".to_owned()
            },
            mapping_kind: Some(mapping_kind_name(binding.kind).to_owned()),
            source_file: binding.source_file.clone(),
            source_column_ordinal,
            source_column_name: binding.source_column.clone(),
            entity_id: Some(document.effective_binding_entity(binding).to_owned()),
            field_id: Some(binding.target_field_id.clone()),
            binding_id: Some(binding.binding_id.clone()),
            issue_code: None,
            detail: None,
        });
    }
    for source in coverage.unmapped_source_columns {
        reject_tombstone_path(&source.source_file, "unmapped source column")?;
        items.push(MappingCoverageItem {
            kind: "UNMAPPED_SOURCE".to_owned(),
            mapping_kind: None,
            source_file: Some(source.source_file),
            source_column_ordinal: Some(u64::try_from(source.source_column_ordinal).map_err(
                |_| integrity_error_value("mapping source column ordinal does not fit u64"),
            )?),
            source_column_name: Some(source.source_column),
            entity_id: source.entity_id,
            field_id: None,
            binding_id: None,
            issue_code: None,
            detail: None,
        });
    }
    for uncovered in coverage.required_canonical_fields_without_coverage {
        let (source_file, entity_id, field_id) = match uncovered {
            RequiredCanonicalFieldIdentity::Legacy(value) => {
                let (source_file, field_id) = value.split_once(':').map_or_else(
                    || (None, value.clone()),
                    |(source_file, field_id)| (Some(source_file.to_owned()), field_id.to_owned()),
                );
                (source_file, Some(document.root_entity_id.clone()), field_id)
            }
            RequiredCanonicalFieldIdentity::Scoped {
                source_file,
                entity_id,
                field_id,
            } => (Some(source_file), Some(entity_id), field_id),
        };
        if let Some(source_file) = &source_file {
            reject_tombstone_path(source_file, "required-uncovered source file")?;
        }
        items.push(MappingCoverageItem {
            kind: "REQUIRED_UNCOVERED".to_owned(),
            mapping_kind: None,
            source_file,
            source_column_ordinal: None,
            source_column_name: None,
            entity_id,
            field_id: Some(field_id),
            binding_id: None,
            issue_code: Some("REQUIRED_UNCOVERED".to_owned()),
            detail: None,
        });
    }
    for detail in coverage.invalid_bindings {
        items.push(MappingCoverageItem {
            kind: "INVALID".to_owned(),
            mapping_kind: None,
            source_file: None,
            source_column_ordinal: None,
            source_column_name: None,
            entity_id: None,
            field_id: None,
            binding_id: None,
            issue_code: Some("INVALID_BINDING".to_owned()),
            detail: Some(detail),
        });
    }
    Ok(items)
}

fn mapping_coverage_key(
    item: &MappingCoverageItem,
) -> (
    String,
    String,
    u64,
    String,
    String,
    String,
    String,
    String,
    String,
) {
    (
        item.kind.clone(),
        item.source_file.clone().unwrap_or_default(),
        item.source_column_ordinal.unwrap_or_default(),
        item.source_column_name.clone().unwrap_or_default(),
        item.entity_id.clone().unwrap_or_default(),
        item.field_id.clone().unwrap_or_default(),
        item.binding_id.clone().unwrap_or_default(),
        item.mapping_kind.clone().unwrap_or_default(),
        item.detail.clone().unwrap_or_default(),
    )
}

fn practitioner_projection(value: &Value) -> Result<Vec<CanonicalProjectionItem>> {
    let object = value
        .as_object()
        .ok_or_else(|| integrity_error_value("canonical row projection is not an object"))?;
    let entity_id = required_json_string(object, "entityId", "canonical row projection")?;
    let canonical_values = object
        .get("canonicalValues")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            integrity_error_value("canonical row projection has no canonicalValues object")
        })?;
    let lineage = object
        .get("lineage")
        .and_then(Value::as_object)
        .ok_or_else(|| integrity_error_value("canonical row projection has no lineage object"))?;
    let mut fields = canonical_values.keys().collect::<Vec<_>>();
    fields.sort_unstable();
    let mut items = Vec::with_capacity(fields.len());
    for field_id in fields {
        let projected_value = match &canonical_values[field_id] {
            Value::Null => None,
            Value::String(value) => Some(value.clone()),
            _ => {
                return integrity_error(format!(
                    "canonical field {field_id:?} has a non-text projected value"
                ));
            }
        };
        let evidence = lineage.get(field_id).ok_or_else(|| {
            integrity_error_value(format!(
                "canonical field {field_id:?} has no lineage evidence"
            ))
        })?;
        items.push(CanonicalProjectionItem {
            entity_id: entity_id.clone(),
            field_id: field_id.clone(),
            value: projected_value,
            lineage: practitioner_lineage(evidence)?,
        });
    }
    if lineage.len() != canonical_values.len() {
        return integrity_error("canonical values and lineage evidence have different field sets");
    }
    Ok(items)
}

pub(crate) fn practitioner_lineage(value: &Value) -> Result<CanonicalLineageView> {
    let evidence = value
        .as_object()
        .ok_or_else(|| integrity_error_value("canonical lineage is not an object"))?;
    if let Some(source_file) = evidence.get("sourceFile").and_then(Value::as_str) {
        reject_tombstone_path(source_file, "canonical lineage source file")?;
    }
    let raw_kind = required_json_string(evidence, "kind", "canonical lineage")?;
    let kind = match raw_kind.as_str() {
        "direct" => "DIRECT",
        "constant" => "CONSTANT",
        "empty_as_null" => "EMPTY_AS_NULL",
        "trim" => "TRIM",
        "uppercase_ascii" => "UPPERCASE_ASCII",
        "lowercase_ascii" => "LOWERCASE_ASCII",
        "lookup" => "LOOKUP",
        _ => return integrity_error(format!("unsupported canonical lineage kind {raw_kind:?}")),
    };
    Ok(CanonicalLineageView {
        kind: kind.to_owned(),
        binding_id: required_json_string(evidence, "bindingId", "canonical lineage")?,
        source_column_ordinal: optional_json_u64(
            evidence,
            "sourceColumnOrdinal",
            "canonical lineage",
        )?,
        source_column_name: optional_json_string(evidence, "sourceColumn", "canonical lineage")?,
        lookup_table_id: optional_json_string(evidence, "lookupTableId", "canonical lineage")?,
    })
}

fn required_json_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| integrity_error_value(format!("{label} has no nonempty {key}")))
}

fn optional_json_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => integrity_error(format!("{label} has invalid optional {key}")),
    }
}

fn optional_json_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Option<u64>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| integrity_error_value(format!("{label} has invalid optional {key}"))),
    }
}

fn row_issue(issue: IssueListItem) -> RowIssueItem {
    RowIssueItem {
        issue_id: issue.issue_id,
        severity: issue.severity,
        rule_id: issue.rule_id,
        issue_code: issue.issue_code,
        display_message: issue.display_message,
        entity_id: issue.entity_id,
        field_id: issue.field_id,
        issue_hash: issue.issue_hash,
    }
}

fn grouped_issue_counts(
    connection: &Connection,
    run_id: &str,
    dimension: &str,
) -> Result<IssueCountGroup> {
    let column = match dimension {
        "severity" => "severity",
        "rule_id" => "rule_id",
        "entity_id" => "entity_id",
        "field_id" => "field_id",
        "source_file" => "source_file",
        _ => return contract_error("unsupported validation issue grouping"),
    };
    let limit = i64::from(MAXIMUM_PAGE_SIZE) + 1;
    let sql = format!(
        "SELECT {column}, COUNT(*) FROM validation_issues
         WHERE run_id = ?1 GROUP BY {column} ORDER BY {column} LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let mapped = statement.query_map(params![run_id, limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut items = Vec::new();
    for row in mapped {
        let row = row?;
        if column == "source_file" {
            reject_tombstone_path(&row.0, "validation issue source file")?;
        }
        items.push(GroupedIssueCount {
            key: row.0,
            count: nonnegative(row.1, "grouped issue count")?,
        });
    }
    let truncated = items.len() > usize::try_from(MAXIMUM_PAGE_SIZE).unwrap_or(usize::MAX);
    if truncated {
        items.truncate(usize::try_from(MAXIMUM_PAGE_SIZE).unwrap_or(usize::MAX));
    }
    Ok(IssueCountGroup { items, truncated })
}

fn issue_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueListItem> {
    Ok(IssueListItem {
        issue_id: row.get(0)?,
        severity: row.get(1)?,
        rule_id: row.get(2)?,
        issue_code: row.get(3)?,
        display_message: row.get(4)?,
        entity_id: row.get(5)?,
        field_id: row.get(6)?,
        source_file: row.get(7)?,
        source_ordinal: row.get(8)?,
        line_start: row.get(9)?,
        line_end: row.get(10)?,
        issue_hash: row.get(11)?,
    })
}

fn append_issue_filters(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    filters: &IssueFilters,
) -> Result<()> {
    if let Some(severity) = filters.severity {
        sql.push_str(" AND severity = ?");
        values.push(SqlValue::Text(severity.as_str().to_owned()));
    }
    for (column, value) in [
        ("rule_id", filters.rule_id.as_ref()),
        ("entity_id", filters.entity_id.as_ref()),
        ("field_id", filters.field_id.as_ref()),
        ("source_file", filters.source_file.as_ref()),
    ] {
        if let Some(value) = value {
            sql.push_str(" AND ");
            sql.push_str(column);
            sql.push_str(" = ?");
            values.push(SqlValue::Text(value.clone()));
        }
    }
    if let Some(source_ordinal) = filters.source_ordinal {
        sql.push_str(" AND source_ordinal = ?");
        values.push(SqlValue::Integer(sqlite_u64(
            source_ordinal,
            "issue source ordinal",
        )?));
    }
    Ok(())
}

fn validate_issue_filters(filters: &IssueFilters) -> Result<()> {
    if let Some(rule_id) = &filters.rule_id {
        validate_identifier(rule_id, "issue rule filter")?;
    }
    if let Some(entity_id) = &filters.entity_id {
        validate_identifier(entity_id, "issue entity filter")?;
    }
    if let Some(field_id) = &filters.field_id {
        validate_identifier(field_id, "issue field filter")?;
    }
    if let Some(source_file) = &filters.source_file {
        validate_nonempty_bounded(source_file, "issue source-file filter", 4096)?;
        reject_tombstone_path(source_file, "issue source-file filter")?;
    }
    if filters.source_ordinal == Some(0) {
        return contract_error("issue source ordinal must be greater than zero");
    }
    Ok(())
}

fn validate_cleanup_filters(filters: &CleanupFilters) -> Result<()> {
    if let Some(namespace) = &filters.namespace
        && !matches!(namespace.as_str(), "packages" | "metadata" | "catalogues")
    {
        return contract_error("unsupported cleanup namespace filter");
    }
    if let Some(state) = &filters.state
        && !matches!(
            state.as_str(),
            "PLANNED" | "QUARANTINED" | "CLEANUP_PENDING" | "CLEANED" | "CONFLICT"
        )
    {
        return contract_error("unsupported cleanup state filter");
    }
    Ok(())
}

fn validation_scope_hash(identity: &ValidationIdentity) -> Result<String> {
    canonical_hash(&serde_json::to_value(identity)?)
}

fn validate_page(page: &PageRequest) -> Result<u32> {
    if page.page_size == 0 || page.page_size > MAXIMUM_PAGE_SIZE {
        return contract_error(format!(
            "page size must be between 1 and {MAXIMUM_PAGE_SIZE}"
        ));
    }
    if page
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > 8192)
    {
        return contract_error("cursor exceeds 8192 bytes");
    }
    Ok(page.page_size)
}

fn split_next_cursor<T, F>(
    items: &mut Vec<T>,
    page_size: u32,
    last_key: F,
    contract: &str,
    scope_hash: &str,
    filter_hash: &str,
) -> Result<Option<String>>
where
    F: FnOnce(&T) -> Value,
{
    let maximum = usize::try_from(page_size).unwrap_or(usize::MAX);
    if items.len() <= maximum {
        return Ok(None);
    }
    items.truncate(maximum);
    let last = items
        .last()
        .ok_or_else(|| integrity_error_value("paged result unexpectedly has no last item"))?;
    Ok(Some(encode_cursor(
        contract,
        scope_hash,
        filter_hash,
        last_key(last),
    )?))
}

fn page_info(page_size: u32, returned: usize, next_cursor: Option<String>) -> Result<PageInfo> {
    Ok(PageInfo {
        page_size,
        returned: u32::try_from(returned)
            .map_err(|_| integrity_error_value("page result count does not fit in u32"))?,
        next_cursor,
    })
}

fn cursor_string(last_key: &Value, contract: &str) -> Result<String> {
    let values = last_key
        .as_array()
        .filter(|values| values.len() == 1)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    values[0]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))
}

fn cursor_string_u64(last_key: &Value, contract: &str) -> Result<(String, u64)> {
    let values = last_key
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let text = values[0]
        .as_str()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let number = values[1]
        .as_u64()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    Ok((text.to_owned(), number))
}

fn cursor_two_strings(last_key: &Value, contract: &str) -> Result<(String, String)> {
    let values = last_key
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let first = values[0]
        .as_str()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let second = values[1]
        .as_str()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    Ok((first.to_owned(), second.to_owned()))
}

fn cursor_offset(last_key: &Value, contract: &str) -> Result<usize> {
    let values = last_key
        .as_array()
        .filter(|values| values.len() == 1)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let value = values[0]
        .as_u64()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    usize::try_from(value)
        .map_err(|_| KernelError::ReadModel(format!("invalid {contract} cursor offset")))
}

fn cursor_string_i64(last_key: &Value, contract: &str) -> Result<(String, i64)> {
    let values = last_key
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let text = values[0]
        .as_str()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    let number = values[1]
        .as_i64()
        .ok_or_else(|| KernelError::ReadModel(format!("invalid {contract} cursor key")))?;
    if number < 0 {
        return contract_error(format!("invalid {contract} cursor ordinal"));
    }
    Ok((text.to_owned(), number))
}

fn response_hash<T: Serialize>(response: &T) -> Result<String> {
    let mut value = serde_json::to_value(response)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| integrity_error_value("read response must serialize as an object"))?;
    let hash = object.get_mut("canonical_hash").ok_or_else(|| {
        integrity_error_value("read response is missing its canonical hash field")
    })?;
    *hash = Value::String(String::new());
    canonical_hash(&value)
}

fn source_file_id(source_hash: &str, source_file: &str) -> String {
    let mut bytes = Vec::with_capacity(source_hash.len() + source_file.len() + 1);
    bytes.extend_from_slice(source_hash.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(source_file.as_bytes());
    sha256_hex(&bytes)
}

fn query_count(connection: &Connection, sql: &str, parameter: &str, label: &str) -> Result<u64> {
    let value: i64 = connection.query_row(sql, [parameter], |row| row.get(0))?;
    nonnegative(value, label)
}

const fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    validate_nonempty_bounded(value, label, 128)
}

fn validate_nonempty_bounded(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum {
        return contract_error(format!("{label} must contain 1 to {maximum} bytes"));
    }
    if value.chars().any(char::is_control) {
        return contract_error(format!("{label} contains a control character"));
    }
    Ok(())
}

fn validate_hash(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return contract_error(format!("{label} must be a lowercase hexadecimal SHA-256"));
    }
    Ok(())
}

fn reject_tombstone_path(path: &str, label: &str) -> Result<()> {
    validate_nonempty_bounded(path, label, 4096)?;
    let path = Path::new(path);
    let forbidden = path.components().any(|component| {
        let std::path::Component::Normal(component) = component else {
            return false;
        };
        component
            .to_string_lossy()
            .rsplit_once('.')
            .is_some_and(|(_, extension)| {
                extension.eq_ignore_ascii_case("part")
                    || extension.eq_ignore_ascii_case("tombstone")
            })
    });
    if forbidden || path_uses_tombstone_namespace(path) {
        return integrity_error(format!(
            "{label} cannot use a live-partial or quarantine namespace"
        ));
    }
    Ok(())
}

fn nonnegative(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| integrity_error_value(format!("{label} is negative: {value}")))
}

fn optional_nonnegative(value: Option<i64>, label: &str) -> Result<Option<u64>> {
    value.map(|value| nonnegative(value, label)).transpose()
}

fn sqlite_u64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::ReadModel(format!("{label} exceeds SQLite integer range")))
}

fn sqlite_limit(page_size: u32) -> Result<i64> {
    i64::from(page_size)
        .checked_add(1)
        .ok_or_else(|| integrity_error_value("page-size limit overflow"))
}

fn validate_row_detail_shape(
    headers: &[String],
    values: &[String],
    source_file: &str,
    source_ordinal: u64,
) -> Result<()> {
    if headers.len() != values.len() {
        return integrity_error(format!(
            "accepted row {source_file}:{source_ordinal} shape differs from its header"
        ));
    }
    if headers.len() > MAXIMUM_ROW_DETAIL_COLUMNS {
        return contract_error(format!(
            "row detail has {} columns; maximum is {MAXIMUM_ROW_DETAIL_COLUMNS}",
            headers.len()
        ));
    }
    if headers
        .iter()
        .any(|header| header.is_empty() || header.chars().count() > MAXIMUM_ROW_DETAIL_HEADER_CHARS)
    {
        return contract_error("row detail contains a blank or overlong header");
    }
    if values
        .iter()
        .any(|value| value.chars().count() > MAXIMUM_ROW_DETAIL_VALUE_CHARS)
    {
        return contract_error("row detail contains a source value above its contract limit");
    }
    Ok(())
}

fn validate_row_detail_lines(line_start: i64, line_end: i64) -> Result<(u64, u64)> {
    let line_start = nonnegative(line_start, "source line start")?;
    let line_end = nonnegative(line_end, "source line end")?;
    if line_start == 0 || line_end == 0 {
        return integrity_error("row detail line ordinals must be greater than zero");
    }
    if line_end < line_start {
        return integrity_error(format!(
            "row detail line end {line_end} precedes line start {line_start}"
        ));
    }
    Ok((line_start, line_end))
}

fn contract_error<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::ReadModel(detail.into()))
}

fn integrity_error<T>(detail: impl Into<String>) -> Result<T> {
    Err(integrity_error_value(detail))
}

fn integrity_error_value(detail: impl Into<String>) -> KernelError {
    KernelError::IntegrityFailed(detail.into())
}

fn not_found(kind: &str, identity: &str) -> KernelError {
    KernelError::NotFound(format!("{kind} {identity:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_bounds_are_closed_and_default_is_safe() {
        assert_eq!(validate_page(&PageRequest::default()).unwrap(), 50);
        assert!(
            validate_page(&PageRequest {
                page_size: 0,
                cursor: None
            })
            .is_err()
        );
        assert!(
            validate_page(&PageRequest {
                page_size: 201,
                cursor: None,
            })
            .is_err()
        );
    }

    #[test]
    fn issue_filters_reject_live_and_quarantine_namespaces() {
        for source_file in ["attempt.part", "attempt.tombstone"] {
            let filters = IssueFilters {
                source_file: Some(source_file.to_owned()),
                ..IssueFilters::default()
            };
            assert!(validate_issue_filters(&filters).is_err());
        }
    }

    #[test]
    fn issue_list_item_never_serializes_source_or_canonical_values() {
        let item = IssueListItem {
            issue_id: "issue-1".to_owned(),
            severity: "ERROR".to_owned(),
            rule_id: "rule-1".to_owned(),
            issue_code: "REQUIRED".to_owned(),
            display_message: "required value is absent".to_owned(),
            entity_id: "entity".to_owned(),
            field_id: "field".to_owned(),
            source_file: "source.csv".to_owned(),
            source_ordinal: 1,
            line_start: 2,
            line_end: 2,
            issue_hash: "a".repeat(64),
        };
        let serialized = serde_json::to_value(item).unwrap();
        let object = serialized.as_object().unwrap();
        assert!(!object.contains_key("source_values"));
        assert!(!object.contains_key("canonical_projection"));
        assert!(!object.contains_key("lineage_json"));
    }

    #[test]
    fn row_detail_shape_enforces_exact_schema_limits() {
        let valid_headers = vec!["id".to_owned()];
        let valid_values = vec!["1".to_owned()];
        validate_row_detail_shape(&valid_headers, &valid_values, "source.csv", 1).unwrap();

        let too_many_headers = (0..=MAXIMUM_ROW_DETAIL_COLUMNS)
            .map(|index| format!("column-{index}"))
            .collect::<Vec<_>>();
        let too_many_values = vec![String::new(); too_many_headers.len()];
        assert_eq!(
            validate_row_detail_shape(&too_many_headers, &too_many_values, "source.csv", 1)
                .unwrap_err()
                .code(),
            "READ_MODEL_INVALID"
        );
        assert_eq!(
            validate_row_detail_shape(&[String::new()], &valid_values, "source.csv", 1)
                .unwrap_err()
                .code(),
            "READ_MODEL_INVALID"
        );
        assert_eq!(
            validate_row_detail_shape(
                &["h".repeat(MAXIMUM_ROW_DETAIL_HEADER_CHARS + 1)],
                &valid_values,
                "source.csv",
                1,
            )
            .unwrap_err()
            .code(),
            "READ_MODEL_INVALID"
        );
        assert_eq!(
            validate_row_detail_shape(
                &valid_headers,
                &["v".repeat(MAXIMUM_ROW_DETAIL_VALUE_CHARS + 1)],
                "source.csv",
                1,
            )
            .unwrap_err()
            .code(),
            "READ_MODEL_INVALID"
        );
        assert_eq!(
            validate_row_detail_shape(&valid_headers, &[], "source.csv", 1)
                .unwrap_err()
                .code(),
            "INTEGRITY_FAILED"
        );
    }

    #[test]
    fn row_detail_lines_enforce_exact_schema_constraints() {
        assert_eq!(validate_row_detail_lines(1, 1).unwrap(), (1, 1));
        assert_eq!(validate_row_detail_lines(1, 2).unwrap(), (1, 2));
        for (line_start, line_end) in [(0, 1), (1, 0), (-1, 1), (2, 1)] {
            assert_eq!(
                validate_row_detail_lines(line_start, line_end)
                    .unwrap_err()
                    .code(),
                "INTEGRITY_FAILED"
            );
        }
    }

    #[test]
    fn response_hash_ignores_an_existing_hash_value() {
        let mut response = JobSummary {
            schema_version: JOB_SUMMARY_SCHEMA.to_owned(),
            data: JobSummaryData {
                job_id: "job-1".to_owned(),
                state: "INTERRUPTED".to_owned(),
                accepted_source: None,
                timestamps: JobTimestamps {
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                    updated_at: "2026-01-01T00:00:00Z".to_owned(),
                    completed_at: None,
                },
                counts: JobCounts {
                    accepted_file_count: 0,
                    accepted_row_count: 0,
                    accepted_field_count: 0,
                    pending_row_count: 0,
                    profiling_run_count: 0,
                },
                reconciliation: ReconciliationStatus {
                    source_records: 0,
                    stored_records: 0,
                    pending_records: 0,
                    closed: false,
                },
                integrity_status: "NOT_CHECKED".to_owned(),
                selected_governance: None,
                warnings: Vec::new(),
            },
            canonical_hash: String::new(),
        };
        let first = response_hash(&response).unwrap();
        response.canonical_hash = "f".repeat(64);
        assert_eq!(response_hash(&response).unwrap(), first);
    }
}

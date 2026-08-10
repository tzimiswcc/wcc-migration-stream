use std::path::PathBuf;

/// Kernel result with stable, machine-readable failure categories.
pub type Result<T> = std::result::Result<T, KernelError>;

/// Typed failures used by the CLI, durable job state, and tests.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("workspace is already in use: {0}")]
    WorkspaceLocked(PathBuf),
    #[error("source must be a regular file: {0}")]
    SourceNotRegular(PathBuf),
    #[error("quarantine tombstones cannot be used as source evidence: {0}")]
    QuarantinedSource(PathBuf),
    #[error("source is {actual} bytes; policy permits {limit}")]
    SourceSize { actual: u64, limit: u64 },
    #[error("insufficient free space: need {required} bytes, have {available} bytes")]
    InsufficientSpace { required: u64, available: u64 },
    #[error("archive contains an encrypted entry: {0}")]
    ArchiveEncrypted(String),
    #[error("archive entry uses an absolute path: {0}")]
    ArchiveAbsolutePath(String),
    #[error("archive entry traverses outside its root: {0}")]
    ArchivePathTraversal(String),
    #[error("archive entry has an unsafe or unsupported path: {0}")]
    ArchiveUnsafePath(String),
    #[error("archive contains a link or special entry: {0}")]
    ArchiveLink(String),
    #[error("archive contains duplicate normalized path: {0}")]
    ArchiveDuplicatePath(String),
    #[error("archive has {actual} entries; policy permits {limit}")]
    ArchiveEntryLimit { actual: usize, limit: usize },
    #[error("archive expands to {actual} bytes; policy permits {limit}")]
    ArchiveExpandedSize { actual: u64, limit: u64 },
    #[error("archive compression ratio {actual:.2} exceeds policy limit {limit:.2}")]
    ArchiveCompressionRatio { actual: f64, limit: f64 },
    #[error("archive failed structural or CRC integrity validation: {0}")]
    ArchiveIntegrity(String),
    #[error("archive does not contain a CSV source file")]
    ArchiveMissingCsv,
    #[error("CSV is not valid UTF-8 or has invalid syntax in {entry}: {detail}")]
    CsvSyntax { entry: String, detail: String },
    #[error("CSV field exceeds {limit} bytes in {entry}")]
    CsvFieldLimit { entry: String, limit: usize },
    #[error("CSV record exceeds {limit} bytes in {entry}")]
    CsvRecordLimit { entry: String, limit: usize },
    #[error("CSV record has {actual} columns in {entry}; policy permits {limit}")]
    CsvColumnLimit {
        entry: String,
        actual: usize,
        limit: usize,
    },
    #[error("CSV package has {actual} records; policy permits {limit}")]
    CsvRecordCountLimit { actual: u64, limit: u64 },
    #[error("CSV header is empty in {0}")]
    CsvEmptyHeader(String),
    #[error("reconciliation invariant failed: {0}")]
    Reconciliation(String),
    #[error("workspace integrity verification failed: {0}")]
    IntegrityFailed(String),
    #[error("intake policy is invalid: {0}")]
    PolicyInvalid(String),
    #[error("operation cancelled: {0}")]
    Cancelled(String),
    #[error("profile JSON Schema is invalid: {0}")]
    ProfileSchema(String),
    #[error("profile does not satisfy its JSON Schema: {0}")]
    ProfileInvalid(String),
    #[error("profile semantic contract is invalid: {0}")]
    ProfileSemantic(String),
    #[error("runtime authority requires APPROVED profile evidence; found {0}")]
    ProfileNotApproved(String),
    #[error("profile registry rejected the operation: {0}")]
    ProfileRegistry(String),
    #[error("profile approval evidence is invalid: {0}")]
    ProfileApproval(String),
    #[error("source mapping is invalid: {0}")]
    MappingInvalid(String),
    #[error("source mapping identity conflicts with immutable evidence: {0}")]
    MappingConflict(String),
    #[error("validation contract is invalid: {0}")]
    ValidationInvalid(String),
    #[error("validation stopped at an explicit bound: {0}")]
    ValidationBound(String),
    #[error("practitioner read request is invalid: {0}")]
    ReadModel(String),
    #[error("practitioner read exceeded its bounded execution deadline")]
    ReadQueryGuard,
    #[error("requested evidence was not found: {0}")]
    NotFound(String),
    #[error("immutable evidence conflicts with the requested operation: {0}")]
    Conflict(String),
    #[error("governed issue decision request is invalid: {0}")]
    DecisionInvalid(String),
    #[error("governed issue decision conflicts with current authority: {0}")]
    DecisionConflict(String),
    #[error("governed issue decision integrity verification failed: {0}")]
    DecisionIntegrity(String),
    #[error("governed issue decision exceeded an explicit bound: {0}")]
    DecisionBound(String),
    #[error("result reconciliation or approval request is invalid: {0}")]
    ResultApprovalInvalid(String),
    #[error("result reconciliation or approval conflicts with current authority: {0}")]
    ResultApprovalConflict(String),
    #[error("result reconciliation or approval integrity verification failed: {0}")]
    ResultApprovalIntegrity(String),
    #[error("result reconciliation or approval exceeded an explicit bound: {0}")]
    ResultApprovalBound(String),
    #[error("EDMX exceeds parser policy: {0}")]
    EdmxLimit(String),
    #[error("EDMX is not supported or is malformed: {0}")]
    EdmxParse(String),
    #[error("generated technical catalogue cannot be used as an approved profile")]
    GeneratedDraftAuthority,
    #[error("assisted runtime bundle is invalid: {0}")]
    BundleInvalid(String),
    #[error("assisted runtime bundle does not support this host: {0}")]
    BundlePlatform(String),
    #[error("assisted local runtime conflicts with current local state: {0}")]
    RuntimeConflict(String),
    #[error("assisted local runtime is unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error("SQLite error: {0}")]
    Sqlite(rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl KernelError {
    /// Stable failure code persisted in job state and returned by the CLI.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::WorkspaceLocked(_) => "WORKSPACE_LOCKED",
            Self::SourceNotRegular(_) => "SOURCE_NOT_REGULAR",
            Self::QuarantinedSource(_) => "QUARANTINED_SOURCE",
            Self::SourceSize { .. } => "SOURCE_SIZE",
            Self::InsufficientSpace { .. } => "INSUFFICIENT_SPACE",
            Self::ArchiveEncrypted(_) => "ARCHIVE_ENCRYPTED",
            Self::ArchiveAbsolutePath(_) => "ARCHIVE_ABSOLUTE_PATH",
            Self::ArchivePathTraversal(_) => "ARCHIVE_PATH_TRAVERSAL",
            Self::ArchiveUnsafePath(_) => "ARCHIVE_UNSAFE_PATH",
            Self::ArchiveLink(_) => "ARCHIVE_LINK",
            Self::ArchiveDuplicatePath(_) => "ARCHIVE_DUPLICATE_PATH",
            Self::ArchiveEntryLimit { .. } => "ARCHIVE_ENTRY_LIMIT",
            Self::ArchiveExpandedSize { .. } => "ARCHIVE_EXPANDED_SIZE",
            Self::ArchiveCompressionRatio { .. } => "ARCHIVE_COMPRESSION_RATIO",
            Self::ArchiveIntegrity(_) => "ARCHIVE_INTEGRITY",
            Self::ArchiveMissingCsv => "ARCHIVE_MISSING_CSV",
            Self::CsvSyntax { .. } => "CSV_SYNTAX",
            Self::CsvFieldLimit { .. } => "CSV_FIELD_LIMIT",
            Self::CsvRecordLimit { .. } => "CSV_RECORD_LIMIT",
            Self::CsvColumnLimit { .. } => "CSV_COLUMN_LIMIT",
            Self::CsvRecordCountLimit { .. } => "CSV_RECORD_COUNT_LIMIT",
            Self::CsvEmptyHeader(_) => "CSV_EMPTY_HEADER",
            Self::Reconciliation(_) => "RECONCILIATION_FAILED",
            Self::IntegrityFailed(_) => "INTEGRITY_FAILED",
            Self::PolicyInvalid(_) => "POLICY_INVALID",
            Self::Cancelled(_) => "COOPERATIVE_CANCELLED",
            Self::ProfileSchema(_) => "PROFILE_SCHEMA_INVALID",
            Self::ProfileInvalid(_) => "PROFILE_INVALID",
            Self::ProfileSemantic(_) => "PROFILE_SEMANTIC_INVALID",
            Self::ProfileNotApproved(_) => "PROFILE_NOT_APPROVED",
            Self::ProfileRegistry(_) => "PROFILE_REGISTRY",
            Self::ProfileApproval(_) => "PROFILE_APPROVAL",
            Self::MappingInvalid(_) => "MAPPING_INVALID",
            Self::MappingConflict(_) => "MAPPING_CONFLICT",
            Self::ValidationInvalid(_) => "VALIDATION_INVALID",
            Self::ValidationBound(_) => "VALIDATION_BOUND_EXCEEDED",
            Self::ReadModel(_) => "READ_MODEL_INVALID",
            Self::ReadQueryGuard => "READ_QUERY_GUARD_EXCEEDED",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT",
            Self::DecisionInvalid(_) => "DECISION_INVALID",
            Self::DecisionConflict(_) => "DECISION_CONFLICT",
            Self::DecisionIntegrity(_) => "DECISION_INTEGRITY_FAILED",
            Self::DecisionBound(_) => "DECISION_BOUND_EXCEEDED",
            Self::ResultApprovalInvalid(_) => "RESULT_APPROVAL_INVALID",
            Self::ResultApprovalConflict(_) => "RESULT_APPROVAL_CONFLICT",
            Self::ResultApprovalIntegrity(_) => "RESULT_APPROVAL_INTEGRITY_FAILED",
            Self::ResultApprovalBound(_) => "RESULT_APPROVAL_BOUND_EXCEEDED",
            Self::EdmxLimit(_) => "EDMX_LIMIT",
            Self::EdmxParse(_) => "EDMX_PARSE",
            Self::GeneratedDraftAuthority => "GENERATED_DRAFT_AUTHORITY",
            Self::BundleInvalid(_) => "BUNDLE_INVALID",
            Self::BundlePlatform(_) => "BUNDLE_PLATFORM_MISMATCH",
            Self::RuntimeConflict(_) => "RUNTIME_CONFLICT",
            Self::RuntimeUnavailable(_) => "RUNTIME_UNAVAILABLE",
            Self::Sqlite(_) => "SQLITE",
            Self::Json(_) => "JSON",
            Self::Io { .. } => "IO",
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<rusqlite::Error> for KernelError {
    fn from(error: rusqlite::Error) -> Self {
        if matches!(
            &error,
            rusqlite::Error::SqliteFailure(details, _)
                if details.code == rusqlite::ErrorCode::OperationInterrupted
        ) {
            Self::ReadQueryGuard
        } else {
            Self::Sqlite(error)
        }
    }
}

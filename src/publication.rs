//! WP-007 Publication Domain Models and Package Contracts.

use serde::{Deserialize, Serialize};

/// Canonical publication package contract identity.
pub const PUBLICATION_CONTRACT_VERSION: &str = "migration-stream/readiness-package/v1";

/// Governed local readiness-publication command contract identity.
pub const PUBLICATION_COMMAND_SCHEMA: &str = "migration-stream/publication-command/v1";

/// Immutable publication detail/result response contract identity.
pub const PUBLICATION_DETAIL_SCHEMA: &str = "migration-stream/publication-detail/v1";

/// Maximum allowed publication package size in bytes (100 MB safety bound).
pub const MAX_PACKAGE_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Maximum allowed member count in package.
pub const MAX_PACKAGE_MEMBER_COUNT: usize = 32;

/// Maximum allowed total publishable rows.
pub const MAX_PUBLISHABLE_ROWS: usize = 1_000_000;

/// Allow-listed member filenames.
pub const MEMBER_MANIFEST: &str = "manifest.json";
pub const MEMBER_EFFECTIVE_ROWS_CSV: &str = "effective-rows.csv";
pub const MEMBER_EFFECTIVE_ROWS_JSON: &str = "effective-rows.json";
pub const MEMBER_EXCLUSION_EVIDENCE: &str = "exclusion-evidence.json";
pub const MEMBER_ISSUE_DECISION_EVIDENCE: &str = "issue-decision-evidence.json";
pub const MEMBER_RECONCILIATION_EVIDENCE: &str = "reconciliation-evidence.json";
pub const MEMBER_APPROVAL_EVIDENCE: &str = "approval-evidence.json";
pub const MEMBER_PROVENANCE: &str = "provenance.json";

/// Non-authority disclaimer text required in manifest and approval evidence.
pub const PUBLICATION_NON_AUTHORITY_DISCLAIMER: &str = "LOCAL READINESS PACKAGE ONLY. THIS PACKAGE DOES NOT TRANSMIT DATA, AUTHORIZE OR EXECUTE A TARGET LOAD, ESTABLISH TARGET COMPATIBILITY OR ACCEPTANCE, PROVIDE AN AUTHENTICATED SIGNATURE, OR GRANT PRODUCTION APPROVAL.";

/// Publication Attempt Status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PublicationStatus {
    Planned,
    Staged,
    Complete,
    Interrupted,
    Failed,
    BoundExceeded,
    StaleInput,
    IntegrityBlocked,
    Conflicted,
}

impl PublicationStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "PLANNED",
            Self::Staged => "STAGED",
            Self::Complete => "COMPLETE",
            Self::Interrupted => "INTERRUPTED",
            Self::Failed => "FAILED",
            Self::BoundExceeded => "BOUND_EXCEEDED",
            Self::StaleInput => "STALE_INPUT",
            Self::IntegrityBlocked => "INTEGRITY_BLOCKED",
            Self::Conflicted => "CONFLICTED",
        }
    }
}

/// Bounded manifest member entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMember {
    pub filename: String,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<usize>,
    pub byte_size: u64,
    pub sha256_digest: String,
}

/// Canonical Publication Manifest (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationManifest {
    pub publication_id: String,
    pub publication_version: String,
    pub contract_version: String,
    pub application_version: String,
    pub engine_version: String,
    pub created_at: String,
    pub practitioner: String,
    pub publication_reason: String,
    pub disclaimer: String,
    pub source_package_digest: String,
    pub profile_identity: String,
    pub profile_version: String,
    pub profile_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_version: Option<String>,
    pub mapping_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_run_id: Option<String>,
    pub validation_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_snapshot_id: Option<String>,
    pub reconciliation_snapshot_hash: String,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub approval_id: String,
    pub approval_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governing_decision_count: Option<usize>,
    pub included_valid_count: usize,
    pub included_corrected_count: usize,
    pub included_waived_count: usize,
    pub included_accepted_as_is_count: usize,
    pub excluded_count: usize,
    pub blocked_unresolved_count: usize,
    pub blocked_incomplete_count: usize,
    pub blocked_conflict_count: usize,
    pub total_accepted_rows: usize,
    pub semantic_publication_hash: String,
    pub member_inventory: Vec<ManifestMember>,
}

/// Evaluation result for publication eligibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationEligibility {
    pub schema_version: String,
    pub is_eligible: bool,
    pub job_id: String,
    pub candidate_id: Option<String>,
    pub candidate_hash: Option<String>,
    pub approval_id: Option<String>,
    pub approval_hash: Option<String>,
    pub total_accepted_rows: usize,
    pub included_rows: usize,
    pub excluded_rows: usize,
    pub blocked_rows: usize,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub required_acknowledgments: Vec<String>,
    pub evaluated_at: String,
}

/// Practitioner Publication Command Request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRequest {
    pub schema_version: String,
    pub job_id: String,
    pub candidate_id: String,
    pub approval_id: String,
    pub practitioner: String,
    pub publication_reason: String,
    pub acknowledgments: Vec<String>,
    pub idempotency_key: String,
}

/// Completed Publication Record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRecord {
    pub schema_version: String,
    pub publication_id: String,
    pub job_id: String,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub approval_id: String,
    pub approval_hash: String,
    pub contract_version: String,
    pub status: PublicationStatus,
    pub practitioner: String,
    pub publication_reason: String,
    pub semantic_publication_hash: String,
    pub physical_archive_digest: String,
    pub package_size_bytes: u64,
    pub member_count: usize,
    pub included_row_count: usize,
    pub excluded_row_count: usize,
    pub archive_relative_path: String,
    pub manifest: PublicationManifest,
    pub created_at: String,
    pub completed_at: String,
}

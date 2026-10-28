//! Closed WP-006 reconciliation and local migration-result approval contracts.
//!
//! This module is deliberately persistence- and transport-agnostic. It binds
//! compact, value-free reconciliation evidence to exact inherited authority,
//! validates closed row accounting, and supplies deterministic canonical hash
//! material. Nothing here publishes data, authorizes a target-system write, or
//! changes accepted source, profile, mapping, validation, or decision history.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{CANONICAL_JSON_VERSION, canonical_hash, canonicalize_json},
};

/// Version of the WP-006 reconciliation, candidate, and action records.
pub const RECONCILIATION_SCHEMA_VERSION: u32 = 1;
/// Exact identity of the only policy contract understood by this module.
pub const RECONCILIATION_POLICY_IDENTITY: &str =
    "migration-stream/reconciliation-approval-policy/v1";
/// Exact policy version encoded by [`RECONCILIATION_POLICY_IDENTITY`].
pub const RECONCILIATION_POLICY_VERSION: u32 = 1;
/// Deterministic row-classification and closure engine identity.
pub const RECONCILIATION_ENGINE_VERSION: &str = "migration-stream-reconciliation-engine/v1";

const POLICY_CONTRACT: &str = "migration-stream-reconciliation-policy-document/v1";
const GOVERNING_INPUT_CONTRACT: &str = "migration-stream-governing-input/v1";
const ACCEPTED_ROW_SET_CONTRACT: &str = "migration-stream-accepted-row-set/v1";
const SNAPSHOT_CONTRACT: &str = "migration-stream-reconciliation-snapshot/v1";
const CANDIDATE_CONTRACT: &str = "migration-stream-migration-result-candidate/v1";
const ACTION_PAYLOAD_CONTRACT: &str = "migration-stream-result-action-payload/v1";
const ACTION_CONTRACT: &str = "migration-stream-result-action/v1";
const SHA256_HEX_BYTES: usize = 64;
const MAXIMUM_IDENTITY_BYTES: usize = 256;
const MAXIMUM_VERSION_BYTES: usize = 128;
const MAXIMUM_SOURCE_FILE_BYTES: usize = 1_024;
const MAXIMUM_CODE_BYTES: usize = 256;
const MAXIMUM_PRACTITIONER_BYTES: usize = 256;
const MAXIMUM_REASON_BYTES: usize = 4_096;
const MAXIMUM_ACKNOWLEDGEMENT_TEXT_BYTES: usize = 8_192;
const MAXIMUM_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAXIMUM_CODES: usize = 4_096;
const MAXIMUM_LINEAGE_HASHES_PER_ROW: usize = 4_096;
const MAXIMUM_GOVERNING_REFERENCES: usize = 1_000_000;

/// Closed, mutually exclusive row-reconciliation vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReconciliationCategory {
    /// Included without an active outcome selecting another included category.
    IncludedValid,
    /// Included using at least one qualifying active correction overlay.
    IncludedCorrected,
    /// Included through qualifying active waiver evidence.
    IncludedWaived,
    /// Included through qualifying active accepted-as-is evidence.
    IncludedAcceptedAsIs,
    /// Excluded through qualifying governed row-exclusion evidence.
    Excluded,
    /// A policy-blocking issue lacks a qualifying active disposition.
    BlockedUnresolved,
    /// Required evidence is absent, nonterminal, stale, or otherwise incomplete.
    BlockedIncomplete,
    /// Governing evidence is conflicting, ambiguous, invalid, or tampered.
    BlockedConflict,
}

impl ReconciliationCategory {
    /// Return the exact persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IncludedValid => "INCLUDED_VALID",
            Self::IncludedCorrected => "INCLUDED_CORRECTED",
            Self::IncludedWaived => "INCLUDED_WAIVED",
            Self::IncludedAcceptedAsIs => "INCLUDED_ACCEPTED_AS_IS",
            Self::Excluded => "EXCLUDED",
            Self::BlockedUnresolved => "BLOCKED_UNRESOLVED",
            Self::BlockedIncomplete => "BLOCKED_INCOMPLETE",
            Self::BlockedConflict => "BLOCKED_CONFLICT",
        }
    }

    /// Return all categories once in their policy-defined order.
    pub const fn all() -> [Self; 8] {
        [
            Self::IncludedValid,
            Self::IncludedCorrected,
            Self::IncludedWaived,
            Self::IncludedAcceptedAsIs,
            Self::Excluded,
            Self::BlockedUnresolved,
            Self::BlockedIncomplete,
            Self::BlockedConflict,
        ]
    }

    /// Whether this category contributes to the included candidate count.
    pub const fn is_included(self) -> bool {
        matches!(
            self,
            Self::IncludedValid
                | Self::IncludedCorrected
                | Self::IncludedWaived
                | Self::IncludedAcceptedAsIs
        )
    }

    /// Whether this category blocks migration-result approval.
    pub const fn is_blocked(self) -> bool {
        matches!(
            self,
            Self::BlockedUnresolved | Self::BlockedIncomplete | Self::BlockedConflict
        )
    }
}

/// Exact reconciliation-attempt lifecycle vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReconciliationAttemptState {
    Planned,
    Running,
    Complete,
    Interrupted,
    Failed,
    BoundExceeded,
    StaleInput,
    IntegrityBlocked,
    Conflicted,
}

impl ReconciliationAttemptState {
    /// Return the exact persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "PLANNED",
            Self::Running => "RUNNING",
            Self::Complete => "COMPLETE",
            Self::Interrupted => "INTERRUPTED",
            Self::Failed => "FAILED",
            Self::BoundExceeded => "BOUND_EXCEEDED",
            Self::StaleInput => "STALE_INPUT",
            Self::IntegrityBlocked => "INTEGRITY_BLOCKED",
            Self::Conflicted => "CONFLICTED",
        }
    }

    /// Only `COMPLETE` may own a completed snapshot hash.
    pub const fn qualifies_for_snapshot(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Immutable v1 policy document. Its lists must already be sorted and unique.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconciliationPolicyV1 {
    pub schema_version: u32,
    pub identity: String,
    pub policy_version: u32,
    pub blocking_issue_severities: Vec<String>,
    pub blocking_rule_kinds: Vec<String>,
    pub waiver_eligible_rule_kinds: Vec<String>,
    pub accepted_as_is_eligible_rule_kinds: Vec<String>,
    pub exclusion_closes_row: bool,
    pub required_reevaluation_terminal_states: Vec<String>,
    pub required_integrity_states: Vec<String>,
    pub operational_warning_codes: Vec<String>,
    pub warnings_requiring_acknowledgement: Vec<String>,
    pub approval_boundary_acknowledgement_text: String,
    pub approval_boundary_acknowledgement_version: String,
    pub canonicalization_version: String,
    pub engine_version: String,
}

impl ReconciliationPolicyV1 {
    /// Validate the explicit v1 policy without inventing business semantics.
    ///
    /// # Errors
    ///
    /// Rejects a different identity/version, implicit or unordered policy
    /// vocabulary, an unsafe boundary statement, or incompatible engine rules.
    pub fn validate(&self) -> Result<()> {
        validate_schema_version(self.schema_version, "policy")?;
        if self.identity != RECONCILIATION_POLICY_IDENTITY
            || self.policy_version != RECONCILIATION_POLICY_VERSION
        {
            return reconciliation_error("unsupported reconciliation policy identity or version");
        }
        validate_sorted_codes(
            &self.blocking_issue_severities,
            "blocking issue severities",
            false,
        )?;
        validate_sorted_codes(&self.blocking_rule_kinds, "blocking rule kinds", false)?;
        validate_sorted_codes(
            &self.waiver_eligible_rule_kinds,
            "waiver-eligible rule kinds",
            false,
        )?;
        validate_sorted_codes(
            &self.accepted_as_is_eligible_rule_kinds,
            "accepted-as-is-eligible rule kinds",
            false,
        )?;
        if !is_subset_sorted(&self.waiver_eligible_rule_kinds, &self.blocking_rule_kinds)
            || !is_subset_sorted(
                &self.accepted_as_is_eligible_rule_kinds,
                &self.blocking_rule_kinds,
            )
        {
            return reconciliation_error(
                "waiver and accepted-as-is rule kinds must be explicit blocking-rule subsets",
            );
        }
        validate_sorted_codes(
            &self.required_reevaluation_terminal_states,
            "required re-evaluation terminal states",
            true,
        )?;
        validate_sorted_codes(
            &self.required_integrity_states,
            "required integrity states",
            true,
        )?;
        validate_sorted_codes(
            &self.operational_warning_codes,
            "operational warning codes",
            false,
        )?;
        validate_sorted_codes(
            &self.warnings_requiring_acknowledgement,
            "warnings requiring acknowledgement",
            false,
        )?;
        if !is_subset_sorted(
            &self.warnings_requiring_acknowledgement,
            &self.operational_warning_codes,
        ) {
            return reconciliation_error(
                "warning acknowledgements must be a subset of operational warnings",
            );
        }
        if !self.exclusion_closes_row {
            return reconciliation_error("v1 policy requires governed exclusion to close one row");
        }
        validate_bounded_text(
            &self.approval_boundary_acknowledgement_text,
            "approval boundary acknowledgement text",
            MAXIMUM_ACKNOWLEDGEMENT_TEXT_BYTES,
            false,
        )?;
        validate_version(
            &self.approval_boundary_acknowledgement_version,
            "approval boundary acknowledgement version",
        )?;
        if self.canonicalization_version != CANONICAL_JSON_VERSION
            || self.engine_version != RECONCILIATION_ENGINE_VERSION
        {
            return reconciliation_error(
                "policy canonicalization and engine identities must match implemented v1 rules",
            );
        }
        Ok(())
    }
}

/// Exact immutable evidence identity and its canonical hash.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceIdentity {
    pub identity: String,
    pub hash: String,
}

impl EvidenceIdentity {
    fn validate(&self, label: &str) -> Result<()> {
        validate_identity(&self.identity, &format!("{label} identity"))?;
        validate_sha256(&self.hash, &format!("{label} hash"))
    }
}

/// Exact immutable versioned authority identity and hash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionedEvidenceIdentity {
    pub identity: String,
    pub version: String,
    pub hash: String,
}

impl VersionedEvidenceIdentity {
    fn validate(&self, label: &str) -> Result<()> {
        validate_identity(&self.identity, &format!("{label} identity"))?;
        validate_version(&self.version, &format!("{label} version"))?;
        validate_sha256(&self.hash, &format!("{label} hash"))
    }
}

/// Exact, value-free governing identities for one reconciliation calculation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoverningIdentities {
    pub schema_version: u32,
    pub job_id: String,
    pub accepted_source_package: EvidenceIdentity,
    pub accepted_row_set_hash: String,
    pub accepted_row_count: u64,
    pub approved_profile: VersionedEvidenceIdentity,
    pub source_mapping: VersionedEvidenceIdentity,
    pub source_validation_run: EvidenceIdentity,
    pub active_decision_heads: Vec<EvidenceIdentity>,
    pub active_correction_overlays: Vec<EvidenceIdentity>,
    pub effective_canonical_values: Vec<EvidenceIdentity>,
    pub qualifying_reevaluations: Vec<EvidenceIdentity>,
    pub policy_identity: String,
    pub policy_hash: String,
    pub engine_version: String,
    pub canonicalization_version: String,
}

impl GoverningIdentities {
    /// Validate bounded exact authority identities and canonical ordering.
    ///
    /// # Errors
    ///
    /// Rejects malformed hashes, unsupported policy/engine identities,
    /// duplicate references, or noncanonical reference ordering.
    pub fn validate(&self) -> Result<()> {
        validate_schema_version(self.schema_version, "governing identities")?;
        validate_identity(&self.job_id, "job ID")?;
        self.accepted_source_package
            .validate("accepted source package")?;
        validate_sha256(&self.accepted_row_set_hash, "accepted row-set hash")?;
        self.approved_profile.validate("approved profile")?;
        self.source_mapping.validate("source mapping")?;
        self.source_validation_run
            .validate("source validation run")?;
        validate_sorted_evidence(&self.active_decision_heads, "active decision heads")?;
        validate_sorted_evidence(
            &self.active_correction_overlays,
            "active correction overlays",
        )?;
        validate_sorted_evidence(
            &self.effective_canonical_values,
            "effective canonical values",
        )?;
        validate_sorted_evidence(&self.qualifying_reevaluations, "qualifying re-evaluations")?;
        if self.policy_identity != RECONCILIATION_POLICY_IDENTITY {
            return reconciliation_error("governing policy identity is not v1");
        }
        validate_sha256(&self.policy_hash, "governing policy hash")?;
        if self.engine_version != RECONCILIATION_ENGINE_VERSION
            || self.canonicalization_version != CANONICAL_JSON_VERSION
        {
            return reconciliation_error(
                "governing engine or canonicalization identity is unsupported",
            );
        }
        Ok(())
    }
}

/// Compact accepted-row identity. It contains no source or effective value.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedRowIdentity {
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub entity_id: String,
}

impl AcceptedRowIdentity {
    /// Validate a bounded accepted-row identity and exclude inert namespaces.
    ///
    /// # Errors
    ///
    /// Rejects invalid row positions, unsafe paths, `.part` files, tombstones,
    /// or malformed entity identities.
    pub fn validate(&self) -> Result<()> {
        validate_source_file(&self.source_file)?;
        if self.source_ordinal == 0 || self.line_start == 0 || self.line_end < self.line_start {
            return reconciliation_error(
                "accepted row ordinal/lineStart must be positive and lineEnd cannot precede it",
            );
        }
        validate_identity(&self.entity_id, "canonical entity ID")
    }
}

/// Value-free classification evidence for exactly one accepted source row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RowClassification {
    pub row: AcceptedRowIdentity,
    pub category: ReconciliationCategory,
    pub explanation_codes: Vec<String>,
    pub lineage_hashes: Vec<String>,
}

impl RowClassification {
    /// Validate compact row evidence and its category-specific minimum lineage.
    ///
    /// # Errors
    ///
    /// Rejects malformed identity, unordered/duplicate explanation evidence,
    /// malformed lineage hashes, or a non-valid outcome without an explanation.
    pub fn validate(&self) -> Result<()> {
        self.row.validate()?;
        validate_sorted_codes(&self.explanation_codes, "row explanation codes", false)?;
        validate_sorted_hashes(&self.lineage_hashes, "row lineage hashes")?;
        if self.category != ReconciliationCategory::IncludedValid
            && self.explanation_codes.is_empty()
        {
            return reconciliation_error(
                "every non-INCLUDED_VALID row requires an explicit explanation code",
            );
        }
        if matches!(
            self.category,
            ReconciliationCategory::IncludedCorrected
                | ReconciliationCategory::IncludedWaived
                | ReconciliationCategory::IncludedAcceptedAsIs
                | ReconciliationCategory::Excluded
        ) && self.lineage_hashes.is_empty()
        {
            return reconciliation_error(
                "governed included/excluded outcomes require immutable lineage hashes",
            );
        }
        Ok(())
    }
}

/// Exact counts for all eight closed reconciliation categories.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CategoryCounts {
    pub included_valid: u64,
    pub included_corrected: u64,
    pub included_waived: u64,
    pub included_accepted_as_is: u64,
    pub excluded: u64,
    pub blocked_unresolved: u64,
    pub blocked_incomplete: u64,
    pub blocked_conflict: u64,
}

impl CategoryCounts {
    /// Derive exact counts from validated row classifications.
    ///
    /// # Errors
    ///
    /// Rejects malformed row evidence or arithmetic overflow.
    pub fn from_rows(rows: &[RowClassification]) -> Result<Self> {
        let mut counts = Self::default();
        for row in rows {
            row.validate()?;
            let count = match row.category {
                ReconciliationCategory::IncludedValid => &mut counts.included_valid,
                ReconciliationCategory::IncludedCorrected => &mut counts.included_corrected,
                ReconciliationCategory::IncludedWaived => &mut counts.included_waived,
                ReconciliationCategory::IncludedAcceptedAsIs => &mut counts.included_accepted_as_is,
                ReconciliationCategory::Excluded => &mut counts.excluded,
                ReconciliationCategory::BlockedUnresolved => &mut counts.blocked_unresolved,
                ReconciliationCategory::BlockedIncomplete => &mut counts.blocked_incomplete,
                ReconciliationCategory::BlockedConflict => &mut counts.blocked_conflict,
            };
            *count = count
                .checked_add(1)
                .ok_or_else(|| reconciliation_failure("category count overflow"))?;
        }
        Ok(counts)
    }

    /// Return the count for one category.
    pub const fn get(&self, category: ReconciliationCategory) -> u64 {
        match category {
            ReconciliationCategory::IncludedValid => self.included_valid,
            ReconciliationCategory::IncludedCorrected => self.included_corrected,
            ReconciliationCategory::IncludedWaived => self.included_waived,
            ReconciliationCategory::IncludedAcceptedAsIs => self.included_accepted_as_is,
            ReconciliationCategory::Excluded => self.excluded,
            ReconciliationCategory::BlockedUnresolved => self.blocked_unresolved,
            ReconciliationCategory::BlockedIncomplete => self.blocked_incomplete,
            ReconciliationCategory::BlockedConflict => self.blocked_conflict,
        }
    }

    /// Return the total row count with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Rejects arithmetic overflow in corrupted evidence.
    pub fn total(&self) -> Result<u64> {
        checked_sum(ReconciliationCategory::all().map(|category| self.get(category)))
    }

    /// Return all included-category rows with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Rejects arithmetic overflow in corrupted evidence.
    pub fn included(&self) -> Result<u64> {
        checked_sum([
            self.included_valid,
            self.included_corrected,
            self.included_waived,
            self.included_accepted_as_is,
        ])
    }

    /// Return all blocking-category rows with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Rejects arithmetic overflow in corrupted evidence.
    pub fn blocked(&self) -> Result<u64> {
        checked_sum([
            self.blocked_unresolved,
            self.blocked_incomplete,
            self.blocked_conflict,
        ])
    }

    fn canonical_map(&self) -> BTreeMap<&'static str, u64> {
        ReconciliationCategory::all()
            .into_iter()
            .map(|category| (category.as_str(), self.get(category)))
            .collect()
    }
}

/// Construction input for one immutable completed reconciliation snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationSnapshotDraft {
    pub snapshot_id: String,
    pub attempt_id: String,
    pub governing: GoverningIdentities,
    pub rows: Vec<RowClassification>,
    pub warning_codes: Vec<String>,
    pub blocker_codes: Vec<String>,
    pub completed_at: String,
}

/// Immutable, exactly closed reconciliation snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconciliationSnapshot {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub attempt_id: String,
    pub state: ReconciliationAttemptState,
    pub governing: GoverningIdentities,
    pub governing_input_hash: String,
    pub category_counts: CategoryCounts,
    pub source_file_counts: BTreeMap<String, u64>,
    pub entity_counts: BTreeMap<String, u64>,
    pub warning_codes: Vec<String>,
    pub blocker_codes: Vec<String>,
    pub rows: Vec<RowClassification>,
    pub completed_at: String,
    pub snapshot_hash: String,
}

impl ReconciliationSnapshot {
    /// Construct a canonical `COMPLETE` snapshot after exact closure succeeds.
    ///
    /// Row insertion order is normalized to accepted-row identity order. The
    /// semantic hash deliberately excludes snapshot/attempt IDs and timestamp.
    ///
    /// # Errors
    ///
    /// Rejects missing or duplicate rows, row-set/count mismatch, malformed or
    /// stale governing identity, noncanonical evidence, and arithmetic failure.
    pub fn build(mut draft: ReconciliationSnapshotDraft) -> Result<Self> {
        draft.governing.validate()?;
        validate_identity(&draft.snapshot_id, "snapshot ID")?;
        validate_identity(&draft.attempt_id, "reconciliation attempt ID")?;
        validate_timestamp(&draft.completed_at, "snapshot completedAt")?;

        draft.rows.sort_by(|left, right| left.row.cmp(&right.row));
        ensure_unique_rows(&draft.rows)?;
        for row in &mut draft.rows {
            normalize_codes(&mut row.explanation_codes, "row explanation codes")?;
            normalize_hashes(&mut row.lineage_hashes, "row lineage hashes")?;
            row.validate()?;
        }
        normalize_codes(&mut draft.warning_codes, "snapshot warning codes")?;
        normalize_codes(&mut draft.blocker_codes, "snapshot blocker codes")?;

        let category_counts = CategoryCounts::from_rows(&draft.rows)?;
        let source_file_counts = derive_source_file_counts(&draft.rows)?;
        let entity_counts = derive_entity_counts(&draft.rows)?;
        let governing_input_hash = canonical_governing_input_hash(&draft.governing)?;
        let mut snapshot = Self {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            snapshot_id: draft.snapshot_id,
            attempt_id: draft.attempt_id,
            state: ReconciliationAttemptState::Complete,
            governing: draft.governing,
            governing_input_hash,
            category_counts,
            source_file_counts,
            entity_counts,
            warning_codes: draft.warning_codes,
            blocker_codes: draft.blocker_codes,
            rows: draft.rows,
            completed_at: draft.completed_at,
            snapshot_hash: String::new(),
        };
        validate_snapshot_without_hash(&snapshot)?;
        snapshot.snapshot_hash = canonical_snapshot_hash(&snapshot)?;
        Ok(snapshot)
    }

    /// Replay exact closure and the deterministic semantic snapshot hash.
    ///
    /// # Errors
    ///
    /// Rejects any malformed, missing, duplicate, count-mismatched, unordered,
    /// non-`COMPLETE`, governing-input-mismatched, or hash-mismatched evidence.
    pub fn validate(&self) -> Result<()> {
        validate_snapshot_without_hash(self)?;
        validate_sha256(&self.snapshot_hash, "snapshot hash")?;
        if canonical_snapshot_hash(self)? != self.snapshot_hash {
            return reconciliation_error(
                "stored snapshot hash differs from canonical semantic evidence",
            );
        }
        Ok(())
    }
}

/// Return domain-separated canonical material for the v1 policy document.
///
/// # Errors
///
/// Rejects an invalid or unsupported policy document.
pub fn canonical_policy_material(policy: &ReconciliationPolicyV1) -> Result<Value> {
    policy.validate()?;
    Ok(canonicalize_json(&json!({
        "contract": POLICY_CONTRACT,
        "policy": policy,
    })))
}

/// Hash a validated v1 policy document using canonical JSON v1.
///
/// # Errors
///
/// Rejects invalid policy data or JSON serialization failure.
pub fn canonical_policy_hash(policy: &ReconciliationPolicyV1) -> Result<String> {
    canonical_hash(&canonical_policy_material(policy)?)
}

/// Return the canonical hash of exact, sorted governing identities.
///
/// # Errors
///
/// Rejects malformed, duplicate, unordered, or unsupported identities.
pub fn canonical_governing_input_hash(governing: &GoverningIdentities) -> Result<String> {
    governing.validate()?;
    canonical_hash(&canonicalize_json(&json!({
        "contract": GOVERNING_INPUT_CONTRACT,
        "governing": governing,
    })))
}

/// Return the order-independent hash of an exact accepted-row identity set.
///
/// # Errors
///
/// Rejects malformed or duplicate accepted-row identities.
pub fn canonical_accepted_row_set_hash(rows: &[AcceptedRowIdentity]) -> Result<String> {
    let mut rows = rows.to_vec();
    rows.sort();
    for row in &rows {
        row.validate()?;
    }
    if rows.windows(2).any(|pair| pair[0] == pair[1]) {
        return reconciliation_error("accepted row set contains a duplicate row identity");
    }
    canonical_hash(&canonicalize_json(&json!({
        "contract": ACCEPTED_ROW_SET_CONTRACT,
        "rows": rows,
    })))
}

/// Return canonical semantic snapshot material, excluding nonsemantic IDs/time.
///
/// # Errors
///
/// Rejects a snapshot that does not close exactly or whose governing identity
/// and stored aggregate evidence are inconsistent.
pub fn canonical_snapshot_material(snapshot: &ReconciliationSnapshot) -> Result<Value> {
    validate_snapshot_without_hash(snapshot)?;
    Ok(canonicalize_json(&json!({
        "contract": SNAPSHOT_CONTRACT,
        "schemaVersion": snapshot.schema_version,
        "canonicalizationVersion": snapshot.governing.canonicalization_version,
        "engineVersion": snapshot.governing.engine_version,
        "governing": snapshot.governing,
        "governingInputHash": snapshot.governing_input_hash,
        "state": snapshot.state,
        "categoryCounts": snapshot.category_counts.canonical_map(),
        "warningCodes": snapshot.warning_codes,
        "blockerCodes": snapshot.blocker_codes,
        "rows": snapshot.rows,
    })))
}

/// Hash canonical semantic snapshot material.
///
/// # Errors
///
/// Rejects a snapshot that fails exact closure or serialization.
pub fn canonical_snapshot_hash(snapshot: &ReconciliationSnapshot) -> Result<String> {
    canonical_hash(&canonical_snapshot_material(snapshot)?)
}

fn validate_snapshot_without_hash(snapshot: &ReconciliationSnapshot) -> Result<()> {
    validate_schema_version(snapshot.schema_version, "snapshot")?;
    validate_identity(&snapshot.snapshot_id, "snapshot ID")?;
    validate_identity(&snapshot.attempt_id, "reconciliation attempt ID")?;
    validate_timestamp(&snapshot.completed_at, "snapshot completedAt")?;
    if !snapshot.state.qualifies_for_snapshot() {
        return reconciliation_error("only COMPLETE reconciliation can own a snapshot");
    }
    snapshot.governing.validate()?;
    let expected_governing_hash = canonical_governing_input_hash(&snapshot.governing)?;
    validate_sha256(&snapshot.governing_input_hash, "governing input hash")?;
    if snapshot.governing_input_hash != expected_governing_hash {
        return reconciliation_error("snapshot governing input hash does not replay");
    }
    validate_sorted_codes(&snapshot.warning_codes, "snapshot warning codes", false)?;
    validate_sorted_codes(&snapshot.blocker_codes, "snapshot blocker codes", false)?;
    validate_sorted_rows(&snapshot.rows)?;

    let row_count = u64::try_from(snapshot.rows.len())
        .map_err(|_| reconciliation_failure("snapshot row count exceeds u64"))?;
    if row_count != snapshot.governing.accepted_row_count {
        return reconciliation_error("snapshot row count does not equal accepted source-row count");
    }
    let identities = snapshot
        .rows
        .iter()
        .map(|classification| classification.row.clone())
        .collect::<Vec<_>>();
    if canonical_accepted_row_set_hash(&identities)? != snapshot.governing.accepted_row_set_hash {
        return reconciliation_error("snapshot row identities do not match accepted row-set hash");
    }
    let expected_counts = CategoryCounts::from_rows(&snapshot.rows)?;
    if snapshot.category_counts != expected_counts
        || snapshot.category_counts.total()? != snapshot.governing.accepted_row_count
    {
        return reconciliation_error("snapshot category counts do not close exactly");
    }
    if snapshot.source_file_counts != derive_source_file_counts(&snapshot.rows)? {
        return reconciliation_error("snapshot per-file counts do not close to row evidence");
    }
    if snapshot.entity_counts != derive_entity_counts(&snapshot.rows)? {
        return reconciliation_error("snapshot per-entity counts do not close to row evidence");
    }
    Ok(())
}

fn validate_sorted_rows(rows: &[RowClassification]) -> Result<()> {
    ensure_unique_rows(rows)?;
    if rows.windows(2).any(|pair| pair[0].row > pair[1].row) {
        return reconciliation_error("snapshot rows are not in canonical accepted-row order");
    }
    for row in rows {
        row.validate()?;
    }
    Ok(())
}

fn ensure_unique_rows(rows: &[RowClassification]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for row in rows {
        if !seen.insert(&row.row) {
            return reconciliation_error("snapshot contains a duplicate accepted-row identity");
        }
    }
    Ok(())
}

fn derive_source_file_counts(rows: &[RowClassification]) -> Result<BTreeMap<String, u64>> {
    let mut counts = BTreeMap::new();
    for row in rows {
        increment_count(&mut counts, &row.row.source_file, "source-file count")?;
    }
    Ok(counts)
}

fn derive_entity_counts(rows: &[RowClassification]) -> Result<BTreeMap<String, u64>> {
    let mut counts = BTreeMap::new();
    for row in rows {
        increment_count(&mut counts, &row.row.entity_id, "entity count")?;
    }
    Ok(counts)
}

fn increment_count(counts: &mut BTreeMap<String, u64>, key: &str, label: &str) -> Result<()> {
    let count = counts.entry(key.to_owned()).or_default();
    *count = count
        .checked_add(1)
        .ok_or_else(|| reconciliation_failure(format!("{label} overflow")))?;
    Ok(())
}

/// Candidate aggregate counts, derived from one exact snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCounts {
    pub included: u64,
    pub excluded: u64,
    pub blocked: u64,
}

impl CandidateCounts {
    fn from_categories(counts: &CategoryCounts) -> Result<Self> {
        Ok(Self {
            included: counts.included()?,
            excluded: counts.excluded,
            blocked: counts.blocked()?,
        })
    }

    fn total(&self) -> Result<u64> {
        checked_sum([self.included, self.excluded, self.blocked])
    }
}

/// Construction input for an immutable local migration-result candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationResultCandidateDraft {
    pub candidate_id: String,
    pub practitioner_id: String,
    pub preparation_reason: String,
    pub created_at: String,
    pub creation_generation: u64,
    pub required_acknowledgement_codes: Vec<String>,
}

/// Immutable local readiness candidate. Its existence grants no approval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationResultCandidate {
    pub schema_version: u32,
    pub candidate_id: String,
    pub job_id: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub governing: GoverningIdentities,
    pub governing_input_hash: String,
    pub policy_identity: String,
    pub policy_hash: String,
    pub engine_version: String,
    pub canonicalization_version: String,
    pub counts: CandidateCounts,
    pub warning_codes: Vec<String>,
    pub required_acknowledgement_codes: Vec<String>,
    pub practitioner_id: String,
    pub preparation_reason: String,
    pub created_at: String,
    pub creation_generation: u64,
    pub candidate_hash: String,
}

impl MigrationResultCandidate {
    /// Bind an immutable candidate to exactly one valid `COMPLETE` snapshot.
    ///
    /// # Errors
    ///
    /// Rejects invalid snapshot integrity, malformed attribution, duplicate or
    /// non-warning acknowledgement codes, and generation overflow conditions.
    pub fn build(
        mut draft: MigrationResultCandidateDraft,
        snapshot: &ReconciliationSnapshot,
    ) -> Result<Self> {
        snapshot.validate()?;
        validate_identity(&draft.candidate_id, "candidate ID")?;
        validate_attribution(&draft.practitioner_id, &draft.preparation_reason)?;
        validate_timestamp(&draft.created_at, "candidate createdAt")?;
        normalize_codes(
            &mut draft.required_acknowledgement_codes,
            "candidate required acknowledgement codes",
        )?;
        if !is_subset_sorted(
            &draft.required_acknowledgement_codes,
            &snapshot.warning_codes,
        ) {
            return reconciliation_error(
                "candidate required acknowledgements must be a subset of snapshot warnings",
            );
        }
        let mut candidate = Self {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            candidate_id: draft.candidate_id,
            job_id: snapshot.governing.job_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            governing: snapshot.governing.clone(),
            governing_input_hash: snapshot.governing_input_hash.clone(),
            policy_identity: snapshot.governing.policy_identity.clone(),
            policy_hash: snapshot.governing.policy_hash.clone(),
            engine_version: snapshot.governing.engine_version.clone(),
            canonicalization_version: snapshot.governing.canonicalization_version.clone(),
            counts: CandidateCounts::from_categories(&snapshot.category_counts)?,
            warning_codes: snapshot.warning_codes.clone(),
            required_acknowledgement_codes: draft.required_acknowledgement_codes,
            practitioner_id: draft.practitioner_id,
            preparation_reason: draft.preparation_reason,
            created_at: draft.created_at,
            creation_generation: draft.creation_generation,
            candidate_hash: String::new(),
        };
        validate_candidate_without_hash(&candidate)?;
        candidate.candidate_hash = canonical_candidate_hash(&candidate)?;
        Ok(candidate)
    }

    /// Replay the immutable candidate hash and internal governing bindings.
    ///
    /// # Errors
    ///
    /// Rejects malformed identity, attribution, counts, governing duplication,
    /// warning inconsistency, or canonical hash mismatch.
    pub fn validate(&self) -> Result<()> {
        validate_candidate_without_hash(self)?;
        validate_sha256(&self.candidate_hash, "candidate hash")?;
        if canonical_candidate_hash(self)? != self.candidate_hash {
            return reconciliation_error(
                "stored candidate hash differs from canonical candidate evidence",
            );
        }
        Ok(())
    }

    /// Replay that this candidate binds exactly the supplied complete snapshot.
    ///
    /// # Errors
    ///
    /// Rejects invalid candidate/snapshot evidence or any identity, hash, count,
    /// warning, policy, engine, or governing mismatch between them.
    pub fn validate_against_snapshot(&self, snapshot: &ReconciliationSnapshot) -> Result<()> {
        self.validate()?;
        snapshot.validate()?;
        if self.job_id != snapshot.governing.job_id
            || self.snapshot_id != snapshot.snapshot_id
            || self.snapshot_hash != snapshot.snapshot_hash
            || self.governing != snapshot.governing
            || self.governing_input_hash != snapshot.governing_input_hash
            || self.counts != CandidateCounts::from_categories(&snapshot.category_counts)?
            || self.warning_codes != snapshot.warning_codes
        {
            return reconciliation_error("candidate does not bind the supplied complete snapshot");
        }
        Ok(())
    }
}

/// Derived candidate currentness; it is never an asserted persistence field.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CandidateCurrentness {
    Current,
    Stale,
}

/// Deterministic, fail-closed approval eligibility result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalEligibility {
    pub currentness: CandidateCurrentness,
    pub eligible: bool,
    pub reason_codes: Vec<String>,
}

/// Return canonical candidate material excluding only its canonical hash.
///
/// # Errors
///
/// Rejects malformed or internally inconsistent candidate evidence.
pub fn canonical_candidate_material(candidate: &MigrationResultCandidate) -> Result<Value> {
    validate_candidate_without_hash(candidate)?;
    let mut record = serde_json::to_value(candidate)?;
    let object = record
        .as_object_mut()
        .ok_or_else(|| reconciliation_failure("candidate must serialize as a JSON object"))?;
    object.remove("candidateHash").ok_or_else(|| {
        reconciliation_failure("candidate canonical material lacks candidateHash")
    })?;
    Ok(canonicalize_json(&json!({
        "contract": CANDIDATE_CONTRACT,
        "candidate": record,
    })))
}

/// Hash canonical immutable candidate material.
///
/// # Errors
///
/// Rejects invalid candidate evidence or serialization failure.
pub fn canonical_candidate_hash(candidate: &MigrationResultCandidate) -> Result<String> {
    canonical_hash(&canonical_candidate_material(candidate)?)
}

/// Derive whether a candidate still matches exact current governing inputs.
///
/// # Errors
///
/// Rejects invalid candidate or current governing evidence. A valid hash
/// mismatch is reported as `STALE`, not mutated into candidate history.
pub fn derive_candidate_currentness(
    candidate: &MigrationResultCandidate,
    current_governing: &GoverningIdentities,
) -> Result<CandidateCurrentness> {
    candidate.validate()?;
    current_governing.validate()?;
    let current_hash = canonical_governing_input_hash(current_governing)?;
    if candidate.governing == *current_governing && candidate.governing_input_hash == current_hash {
        Ok(CandidateCurrentness::Current)
    } else {
        Ok(CandidateCurrentness::Stale)
    }
}

/// Evaluate approval eligibility against exact snapshot/current authority.
///
/// Integrity failure returns an error. Valid but stale, blocked, unacknowledged,
/// or boundary-unacknowledged evidence returns a deterministic ineligible result.
///
/// # Errors
///
/// Rejects tampered candidate/snapshot/governing evidence or malformed warning
/// acknowledgement codes before any approval authority can be considered.
pub fn evaluate_approval_eligibility(
    candidate: &MigrationResultCandidate,
    snapshot: &ReconciliationSnapshot,
    current_governing: &GoverningIdentities,
    warning_acknowledgements: &[String],
    authority_boundary_acknowledged: bool,
) -> Result<ApprovalEligibility> {
    candidate.validate_against_snapshot(snapshot)?;
    let currentness = derive_candidate_currentness(candidate, current_governing)?;
    let (acknowledgements, acknowledgement_duplicates) =
        normalized_codes_for_comparison(warning_acknowledgements, "warning acknowledgements")?;
    let mut reasons = BTreeSet::new();
    if currentness == CandidateCurrentness::Stale {
        reasons.insert("STALE_GOVERNING_INPUTS".to_owned());
    }
    if snapshot.category_counts.blocked_unresolved != 0 {
        reasons.insert("BLOCKED_UNRESOLVED".to_owned());
    }
    if snapshot.category_counts.blocked_incomplete != 0 {
        reasons.insert("BLOCKED_INCOMPLETE".to_owned());
    }
    if snapshot.category_counts.blocked_conflict != 0 {
        reasons.insert("BLOCKED_CONFLICT".to_owned());
    }
    if acknowledgement_duplicates || acknowledgements != candidate.required_acknowledgement_codes {
        reasons.insert("WARNING_ACKNOWLEDGEMENT_MISMATCH".to_owned());
    }
    if !authority_boundary_acknowledged {
        reasons.insert("AUTHORITY_BOUNDARY_NOT_ACKNOWLEDGED".to_owned());
    }
    let reason_codes = reasons.into_iter().collect::<Vec<_>>();
    Ok(ApprovalEligibility {
        currentness,
        eligible: reason_codes.is_empty(),
        reason_codes,
    })
}

fn validate_candidate_without_hash(candidate: &MigrationResultCandidate) -> Result<()> {
    validate_schema_version(candidate.schema_version, "candidate")?;
    validate_identity(&candidate.candidate_id, "candidate ID")?;
    validate_identity(&candidate.job_id, "candidate job ID")?;
    validate_identity(&candidate.snapshot_id, "candidate snapshot ID")?;
    validate_sha256(&candidate.snapshot_hash, "candidate snapshot hash")?;
    candidate.governing.validate()?;
    validate_sha256(
        &candidate.governing_input_hash,
        "candidate governing input hash",
    )?;
    if candidate.job_id != candidate.governing.job_id
        || candidate.governing_input_hash != canonical_governing_input_hash(&candidate.governing)?
    {
        return reconciliation_error("candidate governing identity does not replay");
    }
    if candidate.policy_identity != candidate.governing.policy_identity
        || candidate.policy_hash != candidate.governing.policy_hash
        || candidate.engine_version != candidate.governing.engine_version
        || candidate.canonicalization_version != candidate.governing.canonicalization_version
    {
        return reconciliation_error("candidate policy/engine identity is inconsistent");
    }
    validate_sorted_codes(&candidate.warning_codes, "candidate warning codes", false)?;
    validate_sorted_codes(
        &candidate.required_acknowledgement_codes,
        "candidate required acknowledgement codes",
        false,
    )?;
    if !is_subset_sorted(
        &candidate.required_acknowledgement_codes,
        &candidate.warning_codes,
    ) {
        return reconciliation_error(
            "candidate required acknowledgements are not a subset of warnings",
        );
    }
    validate_attribution(&candidate.practitioner_id, &candidate.preparation_reason)?;
    validate_timestamp(&candidate.created_at, "candidate createdAt")?;
    if candidate.creation_generation == 0 {
        return reconciliation_error("candidate creation generation must be positive");
    }
    if candidate.counts.total()? != candidate.governing.accepted_row_count {
        return reconciliation_error("candidate included/excluded/blocked counts do not close");
    }
    Ok(())
}

/// Exact append-only migration-result action vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResultActionKind {
    Approved,
    Rejected,
    ApprovalRevoked,
    ApprovalSuperseded,
}

impl ResultActionKind {
    /// Return the exact persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
            Self::ApprovalRevoked => "APPROVAL_REVOKED",
            Self::ApprovalSuperseded => "APPROVAL_SUPERSEDED",
        }
    }

    /// Whether this action can establish authority over a candidate.
    pub const fn requires_eligible_candidate(self) -> bool {
        matches!(self, Self::Approved | Self::ApprovalSuperseded)
    }

    /// Whether an active approval predecessor is structurally mandatory.
    pub const fn requires_predecessor(self) -> bool {
        matches!(self, Self::ApprovalRevoked | Self::ApprovalSuperseded)
    }
}

/// Canonical, idempotency-bound action request payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultActionPayload {
    pub schema_version: u32,
    pub kind: ResultActionKind,
    pub candidate_id: String,
    pub candidate_hash: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub actor: String,
    pub reason: String,
    pub warning_acknowledgements: Vec<String>,
    pub authority_boundary_acknowledged: bool,
    pub authority_boundary_acknowledgement_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor_action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor_action_hash: Option<String>,
    pub observed_generation: u64,
    pub idempotency_key: String,
}

impl ResultActionPayload {
    /// Validate bounded action shape and exact predecessor/generation semantics.
    ///
    /// # Errors
    ///
    /// Rejects malformed attribution, missing boundary acknowledgement,
    /// unordered warnings, incomplete predecessor links, or unsafe generation.
    pub fn validate(&self) -> Result<()> {
        validate_schema_version(self.schema_version, "result action payload")?;
        validate_identity(&self.candidate_id, "action candidate ID")?;
        validate_sha256(&self.candidate_hash, "action candidate hash")?;
        validate_identity(&self.snapshot_id, "action snapshot ID")?;
        validate_sha256(&self.snapshot_hash, "action snapshot hash")?;
        validate_attribution(&self.actor, &self.reason)?;
        validate_sorted_codes(
            &self.warning_acknowledgements,
            "action warning acknowledgements",
            false,
        )?;
        if !self.authority_boundary_acknowledged {
            return reconciliation_error(
                "result action requires explicit local-authority boundary acknowledgement",
            );
        }
        validate_version(
            &self.authority_boundary_acknowledgement_version,
            "authority boundary acknowledgement version",
        )?;
        validate_idempotency_key(&self.idempotency_key)?;
        validate_predecessor_shape(self)
    }
}

/// Immutable committed result action and its canonical chain evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultAction {
    pub action_id: String,
    pub payload: ResultActionPayload,
    pub payload_hash: String,
    pub generation: u64,
    pub action_timestamp: String,
    pub action_hash: String,
}

impl ResultAction {
    /// Construct one canonical append-only result action.
    ///
    /// # Errors
    ///
    /// Rejects malformed payload/identity/time or generation overflow.
    pub fn build(
        action_id: String,
        action_timestamp: String,
        payload: ResultActionPayload,
    ) -> Result<Self> {
        payload.validate()?;
        validate_identity(&action_id, "result action ID")?;
        validate_timestamp(&action_timestamp, "result action timestamp")?;
        let payload_hash = canonical_action_payload_hash(&payload)?;
        let generation = payload
            .observed_generation
            .checked_add(1)
            .ok_or_else(|| reconciliation_failure("result action generation overflow"))?;
        let mut action = Self {
            action_id,
            payload,
            payload_hash,
            generation,
            action_timestamp,
            action_hash: String::new(),
        };
        validate_action_without_hash(&action)?;
        action.action_hash = canonical_action_hash(&action)?;
        Ok(action)
    }

    /// Replay payload, generation, and committed canonical action hashes.
    ///
    /// # Errors
    ///
    /// Rejects malformed or tampered immutable action evidence.
    pub fn validate(&self) -> Result<()> {
        validate_action_without_hash(self)?;
        validate_sha256(&self.action_hash, "result action hash")?;
        if canonical_action_hash(self)? != self.action_hash {
            return reconciliation_error(
                "stored result action hash differs from canonical action evidence",
            );
        }
        Ok(())
    }

    /// Verify exact candidate and snapshot attribution without granting authority.
    ///
    /// # Errors
    ///
    /// Rejects invalid action/candidate/snapshot evidence or any binding mismatch.
    pub fn validate_binding(
        &self,
        candidate: &MigrationResultCandidate,
        snapshot: &ReconciliationSnapshot,
    ) -> Result<()> {
        self.validate()?;
        candidate.validate_against_snapshot(snapshot)?;
        if self.payload.candidate_id != candidate.candidate_id
            || self.payload.candidate_hash != candidate.candidate_hash
            || self.payload.snapshot_id != snapshot.snapshot_id
            || self.payload.snapshot_hash != snapshot.snapshot_hash
        {
            return reconciliation_error(
                "result action does not bind the exact candidate snapshot",
            );
        }
        Ok(())
    }
}

/// Return canonical domain-separated action request material.
///
/// # Errors
///
/// Rejects a malformed action payload.
pub fn canonical_action_payload_material(payload: &ResultActionPayload) -> Result<Value> {
    payload.validate()?;
    Ok(canonicalize_json(&json!({
        "contract": ACTION_PAYLOAD_CONTRACT,
        "payload": payload,
    })))
}

/// Hash canonical action request material for idempotency matching.
///
/// # Errors
///
/// Rejects malformed action payload or serialization failure.
pub fn canonical_action_payload_hash(payload: &ResultActionPayload) -> Result<String> {
    canonical_hash(&canonical_action_payload_material(payload)?)
}

/// Return canonical committed-action material excluding its own action hash.
///
/// # Errors
///
/// Rejects malformed or internally inconsistent action evidence.
pub fn canonical_action_material(action: &ResultAction) -> Result<Value> {
    validate_action_without_hash(action)?;
    let mut record = serde_json::to_value(action)?;
    let object = record
        .as_object_mut()
        .ok_or_else(|| reconciliation_failure("result action must serialize as a JSON object"))?;
    object.remove("actionHash").ok_or_else(|| {
        reconciliation_failure("result action canonical material lacks actionHash")
    })?;
    Ok(canonicalize_json(&json!({
        "contract": ACTION_CONTRACT,
        "action": record,
    })))
}

/// Hash canonical committed result-action material.
///
/// # Errors
///
/// Rejects malformed action evidence or serialization failure.
pub fn canonical_action_hash(action: &ResultAction) -> Result<String> {
    canonical_hash(&canonical_action_material(action)?)
}

fn validate_action_without_hash(action: &ResultAction) -> Result<()> {
    validate_identity(&action.action_id, "result action ID")?;
    action.payload.validate()?;
    validate_sha256(&action.payload_hash, "result action payload hash")?;
    if canonical_action_payload_hash(&action.payload)? != action.payload_hash {
        return reconciliation_error("result action payload hash does not replay");
    }
    let expected_generation = action
        .payload
        .observed_generation
        .checked_add(1)
        .ok_or_else(|| reconciliation_failure("result action generation overflow"))?;
    if action.generation != expected_generation {
        return reconciliation_error(
            "result action generation does not follow observed generation",
        );
    }
    validate_timestamp(&action.action_timestamp, "result action timestamp")
}

fn validate_predecessor_shape(payload: &ResultActionPayload) -> Result<()> {
    match (
        payload.predecessor_action_id.as_deref(),
        payload.predecessor_action_hash.as_deref(),
    ) {
        (None, None) if payload.observed_generation == 0 => {}
        (Some(identity), Some(hash)) if payload.observed_generation > 0 => {
            validate_identity(identity, "predecessor action ID")?;
            validate_sha256(hash, "predecessor action hash")?;
        }
        (None, None) => {
            return reconciliation_error(
                "nonzero observed generation requires an exact predecessor action",
            );
        }
        _ => {
            return reconciliation_error(
                "predecessor action ID and hash must be both present or both absent",
            );
        }
    }
    if payload.kind.requires_predecessor() && payload.predecessor_action_id.is_none() {
        return reconciliation_error(format!(
            "{} requires the active approval predecessor",
            payload.kind.as_str()
        ));
    }
    Ok(())
}

fn validate_schema_version(version: u32, label: &str) -> Result<()> {
    if version != RECONCILIATION_SCHEMA_VERSION {
        return reconciliation_error(format!(
            "{label} schemaVersion must be {RECONCILIATION_SCHEMA_VERSION}; found {version}"
        ));
    }
    Ok(())
}

fn validate_identity(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return reconciliation_error(format!(
            "{label} must be a bounded ASCII letter/digit/._-: token"
        ));
    }
    Ok(())
}

fn validate_version(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_VERSION_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return reconciliation_error(format!(
            "{label} must be a bounded ASCII letter/digit/._-/ token"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return reconciliation_error(format!(
            "{label} must be 64 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn validate_source_file(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_SOURCE_FILE_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return reconciliation_error("accepted source file must be a bounded safe relative path");
    }
    for segment in value.split('/') {
        let reserved_extension = Path::new(segment)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("part")
                    || extension.eq_ignore_ascii_case("tombstone")
            });
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.trim() != segment
            || reserved_extension
        {
            return reconciliation_error(
                "accepted source file cannot use traversal, live-part, or tombstone names",
            );
        }
    }
    Ok(())
}

fn validate_timestamp(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || value.chars().any(char::is_control)
        || DateTime::parse_from_rfc3339(value).is_err()
    {
        return reconciliation_error(format!("{label} must be a bounded RFC 3339 timestamp"));
    }
    Ok(())
}

fn validate_attribution(practitioner: &str, reason: &str) -> Result<()> {
    validate_bounded_text(
        practitioner,
        "practitioner/actor identity",
        MAXIMUM_PRACTITIONER_BYTES,
        true,
    )?;
    validate_bounded_text(reason, "mandatory reason", MAXIMUM_REASON_BYTES, false)
}

fn validate_bounded_text(value: &str, label: &str, maximum: usize, exact_trim: bool) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim().is_empty()
        || (exact_trim && value.trim() != value)
        || value.chars().any(char::is_control)
    {
        return reconciliation_error(format!(
            "{label} must be nonblank, control-free, and at most {maximum} UTF-8 bytes"
        ));
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDEMPOTENCY_KEY_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return reconciliation_error(
            "idempotency key must be a bounded ASCII letter/digit/._-: token",
        );
    }
    Ok(())
}

fn validate_sorted_evidence(values: &[EvidenceIdentity], label: &str) -> Result<()> {
    if values.len() > MAXIMUM_GOVERNING_REFERENCES {
        return reconciliation_error(format!(
            "{label} exceeds the bounded governing-reference limit"
        ));
    }
    for value in values {
        value.validate(label)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return reconciliation_error(format!(
            "{label} must be strictly sorted and contain no duplicate identity/hash"
        ));
    }
    let mut identities = BTreeSet::new();
    if values
        .iter()
        .any(|value| !identities.insert(value.identity.as_str()))
    {
        return reconciliation_error(format!("{label} contains a duplicate identity"));
    }
    Ok(())
}

fn validate_sorted_codes(values: &[String], label: &str, required: bool) -> Result<()> {
    if values.len() > MAXIMUM_CODES || (required && values.is_empty()) {
        return reconciliation_error(format!("{label} is empty or exceeds its bounded limit"));
    }
    for value in values {
        validate_code(value, label)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return reconciliation_error(format!(
            "{label} must be strictly sorted and contain no duplicates"
        ));
    }
    Ok(())
}

fn validate_code(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_CODE_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return reconciliation_error(format!(
            "{label} must contain bounded ASCII letter/digit/._-: codes"
        ));
    }
    Ok(())
}

fn validate_sorted_hashes(values: &[String], label: &str) -> Result<()> {
    if values.len() > MAXIMUM_LINEAGE_HASHES_PER_ROW {
        return reconciliation_error(format!("{label} exceeds its bounded limit"));
    }
    for value in values {
        validate_sha256(value, label)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return reconciliation_error(format!(
            "{label} must be strictly sorted and contain no duplicates"
        ));
    }
    Ok(())
}

fn normalize_codes(values: &mut [String], label: &str) -> Result<()> {
    values.sort();
    validate_sorted_codes(values, label, false)
}

fn normalize_hashes(values: &mut [String], label: &str) -> Result<()> {
    values.sort();
    validate_sorted_hashes(values, label)
}

fn normalized_codes_for_comparison(values: &[String], label: &str) -> Result<(Vec<String>, bool)> {
    if values.len() > MAXIMUM_CODES {
        return reconciliation_error(format!("{label} exceeds its bounded limit"));
    }
    for value in values {
        validate_code(value, label)?;
    }
    let mut normalized = values.to_vec();
    normalized.sort();
    let duplicates = normalized.windows(2).any(|pair| pair[0] == pair[1]);
    Ok((normalized, duplicates))
}

fn is_subset_sorted(subset: &[String], complete: &[String]) -> bool {
    subset
        .iter()
        .all(|value| complete.binary_search(value).is_ok())
}

fn checked_sum(values: impl IntoIterator<Item = u64>) -> Result<u64> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| reconciliation_failure("reconciliation count overflow"))
    })
}

fn reconciliation_error<T>(message: impl Into<String>) -> Result<T> {
    Err(reconciliation_failure(message))
}

fn reconciliation_failure(message: impl Into<String>) -> KernelError {
    KernelError::Reconciliation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::sha256_hex;

    fn digest(label: &str) -> String {
        sha256_hex(label.as_bytes())
    }

    fn policy() -> ReconciliationPolicyV1 {
        ReconciliationPolicyV1 {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            identity: RECONCILIATION_POLICY_IDENTITY.to_owned(),
            policy_version: RECONCILIATION_POLICY_VERSION,
            blocking_issue_severities: vec!["ERROR".to_owned()],
            blocking_rule_kinds: vec!["REQUIRED".to_owned(), "TYPE".to_owned()],
            waiver_eligible_rule_kinds: vec!["TYPE".to_owned()],
            accepted_as_is_eligible_rule_kinds: vec!["TYPE".to_owned()],
            exclusion_closes_row: true,
            required_reevaluation_terminal_states: vec!["COMPLETE".to_owned()],
            required_integrity_states: vec!["VERIFIED".to_owned()],
            operational_warning_codes: vec!["REVIEW_REQUIRED".to_owned()],
            warnings_requiring_acknowledgement: vec!["REVIEW_REQUIRED".to_owned()],
            approval_boundary_acknowledgement_text:
                "Local readiness only; no publication or target authority.".to_owned(),
            approval_boundary_acknowledgement_version: "v1".to_owned(),
            canonicalization_version: CANONICAL_JSON_VERSION.to_owned(),
            engine_version: RECONCILIATION_ENGINE_VERSION.to_owned(),
        }
    }

    fn row(ordinal: u64, category: ReconciliationCategory) -> RowClassification {
        let requires_governed_lineage = matches!(
            category,
            ReconciliationCategory::IncludedCorrected
                | ReconciliationCategory::IncludedWaived
                | ReconciliationCategory::IncludedAcceptedAsIs
                | ReconciliationCategory::Excluded
        );
        RowClassification {
            row: AcceptedRowIdentity {
                source_file: "customers.csv".to_owned(),
                source_ordinal: ordinal,
                line_start: ordinal + 1,
                line_end: ordinal + 1,
                entity_id: "Customer".to_owned(),
            },
            category,
            explanation_codes: if category == ReconciliationCategory::IncludedValid {
                Vec::new()
            } else {
                vec![format!("{}_EVIDENCE", category.as_str())]
            },
            lineage_hashes: if requires_governed_lineage {
                vec![digest(&format!("lineage-{ordinal}"))]
            } else {
                Vec::new()
            },
        }
    }

    fn governing_for(rows: &[RowClassification]) -> GoverningIdentities {
        let identities = rows
            .iter()
            .map(|value| value.row.clone())
            .collect::<Vec<_>>();
        GoverningIdentities {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            job_id: "job-006".to_owned(),
            accepted_source_package: EvidenceIdentity {
                identity: "source-package-1".to_owned(),
                hash: digest("source-package"),
            },
            accepted_row_set_hash: canonical_accepted_row_set_hash(&identities)
                .expect("accepted row-set hash"),
            accepted_row_count: u64::try_from(rows.len()).expect("row count"),
            approved_profile: VersionedEvidenceIdentity {
                identity: "CustomerProfile".to_owned(),
                version: "1.0.0".to_owned(),
                hash: digest("profile"),
            },
            source_mapping: VersionedEvidenceIdentity {
                identity: "CustomerMapping".to_owned(),
                version: "1.0.0".to_owned(),
                hash: digest("mapping"),
            },
            source_validation_run: EvidenceIdentity {
                identity: "validation-run-1".to_owned(),
                hash: digest("validation"),
            },
            active_decision_heads: vec![EvidenceIdentity {
                identity: "decision-1".to_owned(),
                hash: digest("decision"),
            }],
            active_correction_overlays: Vec::new(),
            effective_canonical_values: vec![EvidenceIdentity {
                identity: "effective-row-set".to_owned(),
                hash: digest("effective-row-set"),
            }],
            qualifying_reevaluations: Vec::new(),
            policy_identity: RECONCILIATION_POLICY_IDENTITY.to_owned(),
            policy_hash: canonical_policy_hash(&policy()).expect("policy hash"),
            engine_version: RECONCILIATION_ENGINE_VERSION.to_owned(),
            canonicalization_version: CANONICAL_JSON_VERSION.to_owned(),
        }
    }

    fn snapshot_with(
        rows: Vec<RowClassification>,
        warnings: Vec<String>,
    ) -> ReconciliationSnapshot {
        let governing = governing_for(&rows);
        ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "snapshot-1".to_owned(),
            attempt_id: "attempt-1".to_owned(),
            governing,
            rows,
            warning_codes: warnings,
            blocker_codes: Vec::new(),
            completed_at: "2026-07-22T01:02:03Z".to_owned(),
        })
        .expect("complete snapshot")
    }

    fn candidate_for(
        snapshot: &ReconciliationSnapshot,
        required_acknowledgements: Vec<String>,
    ) -> MigrationResultCandidate {
        MigrationResultCandidate::build(
            MigrationResultCandidateDraft {
                candidate_id: "candidate-1".to_owned(),
                practitioner_id: "practitioner-1".to_owned(),
                preparation_reason: "Prepare exact local readiness evidence.".to_owned(),
                created_at: "2026-07-22T02:03:04Z".to_owned(),
                creation_generation: 1,
                required_acknowledgement_codes: required_acknowledgements,
            },
            snapshot,
        )
        .expect("candidate")
    }

    fn approved_payload(candidate: &MigrationResultCandidate) -> ResultActionPayload {
        ResultActionPayload {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            kind: ResultActionKind::Approved,
            candidate_id: candidate.candidate_id.clone(),
            candidate_hash: candidate.candidate_hash.clone(),
            snapshot_id: candidate.snapshot_id.clone(),
            snapshot_hash: candidate.snapshot_hash.clone(),
            actor: "approver-1".to_owned(),
            reason: "Approve this exact local readiness candidate.".to_owned(),
            warning_acknowledgements: candidate.required_acknowledgement_codes.clone(),
            authority_boundary_acknowledged: true,
            authority_boundary_acknowledgement_version: "v1".to_owned(),
            predecessor_action_id: None,
            predecessor_action_hash: None,
            observed_generation: 0,
            idempotency_key: "approve-candidate-1".to_owned(),
        }
    }

    #[test]
    fn exact_closed_vocabularies_serialize_without_fallbacks() {
        let categories = ReconciliationCategory::all();
        let expected_categories = [
            "INCLUDED_VALID",
            "INCLUDED_CORRECTED",
            "INCLUDED_WAIVED",
            "INCLUDED_ACCEPTED_AS_IS",
            "EXCLUDED",
            "BLOCKED_UNRESOLVED",
            "BLOCKED_INCOMPLETE",
            "BLOCKED_CONFLICT",
        ];
        assert_eq!(
            categories.map(ReconciliationCategory::as_str),
            expected_categories
        );
        for (category, expected) in categories.into_iter().zip(expected_categories) {
            assert_eq!(
                serde_json::to_string(&category).expect("serialize category"),
                format!("\"{expected}\"")
            );
        }

        let states = [
            ReconciliationAttemptState::Planned,
            ReconciliationAttemptState::Running,
            ReconciliationAttemptState::Complete,
            ReconciliationAttemptState::Interrupted,
            ReconciliationAttemptState::Failed,
            ReconciliationAttemptState::BoundExceeded,
            ReconciliationAttemptState::StaleInput,
            ReconciliationAttemptState::IntegrityBlocked,
            ReconciliationAttemptState::Conflicted,
        ];
        let expected_states = [
            "PLANNED",
            "RUNNING",
            "COMPLETE",
            "INTERRUPTED",
            "FAILED",
            "BOUND_EXCEEDED",
            "STALE_INPUT",
            "INTEGRITY_BLOCKED",
            "CONFLICTED",
        ];
        assert_eq!(
            states.map(ReconciliationAttemptState::as_str),
            expected_states
        );

        let actions = [
            ResultActionKind::Approved,
            ResultActionKind::Rejected,
            ResultActionKind::ApprovalRevoked,
            ResultActionKind::ApprovalSuperseded,
        ];
        assert_eq!(
            actions.map(ResultActionKind::as_str),
            [
                "APPROVED",
                "REJECTED",
                "APPROVAL_REVOKED",
                "APPROVAL_SUPERSEDED"
            ]
        );
    }

    #[test]
    fn policy_is_explicit_hash_bound_and_canonically_ordered() {
        let policy = policy();
        policy.validate().expect("valid policy");
        assert_eq!(
            canonical_policy_hash(&policy).expect("first hash"),
            canonical_policy_hash(&policy).expect("second hash")
        );

        let mut unordered = policy.clone();
        unordered.blocking_rule_kinds.reverse();
        assert!(unordered.validate().is_err());

        let mut implicit_exclusion = policy;
        implicit_exclusion.exclusion_closes_row = false;
        assert!(implicit_exclusion.validate().is_err());
    }

    #[test]
    fn all_eight_categories_close_once_and_only_once() {
        let rows = ReconciliationCategory::all()
            .into_iter()
            .enumerate()
            .map(|(index, category)| row(u64::try_from(index + 1).expect("ordinal"), category))
            .rev()
            .collect::<Vec<_>>();
        let snapshot = snapshot_with(rows, Vec::new());
        snapshot.validate().expect("snapshot integrity");
        for category in ReconciliationCategory::all() {
            assert_eq!(snapshot.category_counts.get(category), 1);
        }
        assert_eq!(snapshot.category_counts.total().expect("total"), 8);
        assert_eq!(snapshot.category_counts.included().expect("included"), 4);
        assert_eq!(snapshot.category_counts.excluded, 1);
        assert_eq!(snapshot.category_counts.blocked().expect("blocked"), 3);
        assert_eq!(snapshot.source_file_counts["customers.csv"], 8);
        assert_eq!(snapshot.entity_counts["Customer"], 8);
    }

    #[test]
    fn missing_duplicate_and_row_set_mismatch_fail_closed() {
        let authoritative = vec![
            row(1, ReconciliationCategory::IncludedValid),
            row(2, ReconciliationCategory::IncludedValid),
        ];
        let governing = governing_for(&authoritative);
        let missing = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "missing".to_owned(),
            attempt_id: "attempt-missing".to_owned(),
            governing: governing.clone(),
            rows: vec![authoritative[0].clone()],
            warning_codes: Vec::new(),
            blocker_codes: Vec::new(),
            completed_at: "2026-07-22T01:00:00Z".to_owned(),
        });
        assert!(missing.is_err());

        let duplicate = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "duplicate".to_owned(),
            attempt_id: "attempt-duplicate".to_owned(),
            governing,
            rows: vec![authoritative[0].clone(), authoritative[0].clone()],
            warning_codes: Vec::new(),
            blocker_codes: Vec::new(),
            completed_at: "2026-07-22T01:00:00Z".to_owned(),
        });
        assert!(duplicate.is_err());

        let mut mismatch_governing = governing_for(&authoritative);
        mismatch_governing.accepted_row_set_hash = digest("different-row-set");
        let mismatch = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "mismatch".to_owned(),
            attempt_id: "attempt-mismatch".to_owned(),
            governing: mismatch_governing,
            rows: authoritative,
            warning_codes: Vec::new(),
            blocker_codes: Vec::new(),
            completed_at: "2026-07-22T01:00:00Z".to_owned(),
        });
        assert!(mismatch.is_err());
    }

    #[test]
    fn aggregate_count_state_and_order_tampering_fail_closed() {
        let rows = vec![
            row(1, ReconciliationCategory::IncludedValid),
            row(2, ReconciliationCategory::Excluded),
        ];
        let snapshot = snapshot_with(rows, Vec::new());

        let mut category_mismatch = snapshot.clone();
        category_mismatch.category_counts.included_valid += 1;
        assert!(category_mismatch.validate().is_err());

        let mut file_mismatch = snapshot.clone();
        file_mismatch
            .source_file_counts
            .insert("customers.csv".to_owned(), 99);
        assert!(file_mismatch.validate().is_err());

        let mut entity_mismatch = snapshot.clone();
        entity_mismatch
            .entity_counts
            .insert("Customer".to_owned(), 99);
        assert!(entity_mismatch.validate().is_err());

        let mut noncomplete = snapshot.clone();
        noncomplete.state = ReconciliationAttemptState::Interrupted;
        assert!(noncomplete.validate().is_err());

        let mut unordered = snapshot;
        unordered.rows.reverse();
        assert!(unordered.validate().is_err());
    }

    #[test]
    fn semantic_snapshot_hash_excludes_ids_time_and_insertion_order() {
        let rows = vec![
            row(1, ReconciliationCategory::IncludedValid),
            row(2, ReconciliationCategory::IncludedCorrected),
        ];
        let governing = governing_for(&rows);
        let first = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "snapshot-a".to_owned(),
            attempt_id: "attempt-a".to_owned(),
            governing: governing.clone(),
            rows: rows.clone(),
            warning_codes: vec!["SECOND".to_owned(), "FIRST".to_owned()],
            blocker_codes: Vec::new(),
            completed_at: "2026-07-22T01:00:00Z".to_owned(),
        })
        .expect("first snapshot");
        let second = ReconciliationSnapshot::build(ReconciliationSnapshotDraft {
            snapshot_id: "snapshot-b".to_owned(),
            attempt_id: "attempt-b".to_owned(),
            governing,
            rows: rows.into_iter().rev().collect(),
            warning_codes: vec!["FIRST".to_owned(), "SECOND".to_owned()],
            blocker_codes: Vec::new(),
            completed_at: "2026-07-23T04:05:06Z".to_owned(),
        })
        .expect("second snapshot");
        assert_eq!(first.snapshot_hash, second.snapshot_hash);
        assert_eq!(
            canonical_snapshot_material(&first).expect("first material"),
            canonical_snapshot_material(&second).expect("second material")
        );
    }

    #[test]
    fn current_unblocked_candidate_requires_exact_warning_and_boundary_acknowledgement() {
        let snapshot = snapshot_with(
            vec![row(1, ReconciliationCategory::IncludedValid)],
            vec!["REVIEW_REQUIRED".to_owned()],
        );
        let candidate = candidate_for(&snapshot, vec!["REVIEW_REQUIRED".to_owned()]);
        candidate
            .validate_against_snapshot(&snapshot)
            .expect("candidate binding");

        let eligible = evaluate_approval_eligibility(
            &candidate,
            &snapshot,
            &snapshot.governing,
            &["REVIEW_REQUIRED".to_owned()],
            true,
        )
        .expect("eligibility");
        assert!(eligible.eligible);
        assert_eq!(eligible.currentness, CandidateCurrentness::Current);
        assert_eq!(eligible.reason_codes, [] as [String; 0]);

        let unacknowledged =
            evaluate_approval_eligibility(&candidate, &snapshot, &snapshot.governing, &[], true)
                .expect("unacknowledged result");
        assert!(!unacknowledged.eligible);
        assert!(
            unacknowledged
                .reason_codes
                .contains(&"WARNING_ACKNOWLEDGEMENT_MISMATCH".to_owned())
        );

        let no_boundary = evaluate_approval_eligibility(
            &candidate,
            &snapshot,
            &snapshot.governing,
            &["REVIEW_REQUIRED".to_owned()],
            false,
        )
        .expect("boundary result");
        assert!(!no_boundary.eligible);
        assert!(
            no_boundary
                .reason_codes
                .contains(&"AUTHORITY_BOUNDARY_NOT_ACKNOWLEDGED".to_owned())
        );
    }

    #[test]
    fn stale_and_each_blocked_category_are_ineligible() {
        for (category, expected_reason) in [
            (
                ReconciliationCategory::BlockedUnresolved,
                "BLOCKED_UNRESOLVED",
            ),
            (
                ReconciliationCategory::BlockedIncomplete,
                "BLOCKED_INCOMPLETE",
            ),
            (ReconciliationCategory::BlockedConflict, "BLOCKED_CONFLICT"),
        ] {
            let snapshot = snapshot_with(vec![row(1, category)], Vec::new());
            let candidate = candidate_for(&snapshot, Vec::new());
            let result = evaluate_approval_eligibility(
                &candidate,
                &snapshot,
                &snapshot.governing,
                &[],
                true,
            )
            .expect("blocked result");
            assert!(!result.eligible);
            assert!(result.reason_codes.contains(&expected_reason.to_owned()));
        }

        let snapshot = snapshot_with(
            vec![row(1, ReconciliationCategory::IncludedValid)],
            Vec::new(),
        );
        let candidate = candidate_for(&snapshot, Vec::new());
        let mut current = snapshot.governing.clone();
        current.policy_hash = digest("new-current-policy");
        assert_eq!(
            derive_candidate_currentness(&candidate, &current).expect("currentness"),
            CandidateCurrentness::Stale
        );
        let stale = evaluate_approval_eligibility(&candidate, &snapshot, &current, &[], true)
            .expect("stale result");
        assert!(!stale.eligible);
        assert!(
            stale
                .reason_codes
                .contains(&"STALE_GOVERNING_INPUTS".to_owned())
        );
    }

    #[test]
    fn candidate_hash_binds_attribution_counts_and_snapshot() {
        let snapshot = snapshot_with(
            vec![row(1, ReconciliationCategory::IncludedValid)],
            Vec::new(),
        );
        let candidate = candidate_for(&snapshot, Vec::new());
        candidate.validate().expect("candidate integrity");

        let mut changed_reason = candidate.clone();
        changed_reason.preparation_reason = "Different preparation reason.".to_owned();
        assert!(changed_reason.validate().is_err());

        let mut changed_count = candidate.clone();
        changed_count.counts.included = 0;
        assert!(changed_count.validate().is_err());

        let mut changed_snapshot = snapshot;
        changed_snapshot.snapshot_id = "different-snapshot".to_owned();
        assert!(
            candidate
                .validate_against_snapshot(&changed_snapshot)
                .is_err()
        );
    }

    #[test]
    fn action_payload_and_record_hashes_are_deterministic_and_tamper_evident() {
        let snapshot = snapshot_with(
            vec![row(1, ReconciliationCategory::IncludedValid)],
            Vec::new(),
        );
        let candidate = candidate_for(&snapshot, Vec::new());
        let payload = approved_payload(&candidate);
        let first = ResultAction::build(
            "action-1".to_owned(),
            "2026-07-22T03:04:05Z".to_owned(),
            payload.clone(),
        )
        .expect("first action");
        let second = ResultAction::build(
            "action-2".to_owned(),
            "2026-07-22T03:04:06Z".to_owned(),
            payload,
        )
        .expect("second action");
        assert_eq!(first.payload_hash, second.payload_hash);
        assert_ne!(first.action_hash, second.action_hash);
        first
            .validate_binding(&candidate, &snapshot)
            .expect("action binding");

        let mut tampered = first;
        tampered.payload.actor = "different-actor".to_owned();
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn predecessor_and_generation_semantics_fail_closed() {
        let snapshot = snapshot_with(
            vec![row(1, ReconciliationCategory::IncludedValid)],
            Vec::new(),
        );
        let candidate = candidate_for(&snapshot, Vec::new());

        let mut revoke_without_predecessor = approved_payload(&candidate);
        revoke_without_predecessor.kind = ResultActionKind::ApprovalRevoked;
        assert!(revoke_without_predecessor.validate().is_err());

        let mut partial_predecessor = approved_payload(&candidate);
        partial_predecessor.observed_generation = 1;
        partial_predecessor.predecessor_action_id = Some("action-1".to_owned());
        assert!(partial_predecessor.validate().is_err());

        let mut revoke = approved_payload(&candidate);
        revoke.kind = ResultActionKind::ApprovalRevoked;
        revoke.observed_generation = 1;
        revoke.predecessor_action_id = Some("action-1".to_owned());
        revoke.predecessor_action_hash = Some(digest("action-1"));
        let action = ResultAction::build(
            "action-2".to_owned(),
            "2026-07-22T04:05:06Z".to_owned(),
            revoke,
        )
        .expect("revocation action");
        assert_eq!(action.generation, 2);
        action.validate().expect("revocation integrity");
    }

    #[test]
    fn live_partial_and_tombstone_names_never_enter_row_evidence() {
        for source_file in ["attempt.part", "internal.tombstone", "dir/attempt.PART"] {
            let mut invalid = row(1, ReconciliationCategory::IncludedValid);
            invalid.row.source_file = source_file.to_owned();
            assert!(invalid.validate().is_err());
        }
    }
}

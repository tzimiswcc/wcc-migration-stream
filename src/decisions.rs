//! Closed WP-005 issue-decision and non-destructive correction contracts.
//!
//! These types describe practitioner evidence only. They cannot modify source
//! evidence, approve migration results, publish data, or grant target-system
//! authority. Persistence and authority checks remain application-service
//! responsibilities; this module supplies strict bounded input contracts and
//! deterministic canonical hash material.

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonicalize_json},
};

/// Current version of the issue-decision and correction-overlay contracts.
pub const DECISION_SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 byte length of a practitioner identifier.
pub const MAXIMUM_PRACTITIONER_BYTES: usize = 256;
/// Maximum UTF-8 byte length of a mandatory decision reason.
pub const MAXIMUM_REASON_BYTES: usize = 4_096;
/// Maximum UTF-8 byte length of an optional evidence reference.
pub const MAXIMUM_EVIDENCE_REFERENCE_BYTES: usize = 2_048;
/// Maximum byte length of an idempotency key.
pub const MAXIMUM_IDEMPOTENCY_KEY_BYTES: usize = 128;
/// Maximum byte length of any internal identity token.
pub const MAXIMUM_IDENTITY_BYTES: usize = 256;
/// Maximum byte length of an accepted archive-relative source file identity.
pub const MAXIMUM_SOURCE_FILE_BYTES: usize = 1_024;
/// Absolute UTF-8 byte ceiling for one textual correction value.
pub const MAXIMUM_CORRECTION_VALUE_BYTES: usize = 1024 * 1024;
/// Absolute Unicode scalar-value ceiling for one textual correction value.
pub const MAXIMUM_CORRECTION_VALUE_CHARS: usize = 1024 * 1024;

const SHA256_HEX_BYTES: usize = 64;
const REQUEST_CONTRACT: &str = "migration-stream-issue-decision-request/v1";
const DECISION_CONTRACT: &str = "migration-stream-issue-decision/v1";
const OVERLAY_CONTRACT: &str = "migration-stream-correction-overlay/v1";

/// Closed WP-005 issue disposition vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueDisposition {
    /// An eligible overlay and qualifying re-evaluation became effective.
    Corrected,
    /// The issue is acknowledged under an attributable reason.
    Waived,
    /// The current effective value is deliberately retained.
    AcceptedAsIs,
    /// The exact accepted source row is excluded from a future candidate.
    Excluded,
}

impl IssueDisposition {
    /// Return the stable persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Corrected => "CORRECTED",
            Self::Waived => "WAIVED",
            Self::AcceptedAsIs => "ACCEPTED_AS_IS",
            Self::Excluded => "EXCLUDED",
        }
    }
}

/// Closed field-level correction operation vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CorrectionOperation {
    /// Replace the effective value with the exact supplied text.
    ReplaceEffectiveText,
    /// Set the effective value to present-but-empty text.
    SetEffectiveEmpty,
    /// Set the effective value to JSON/SQL null.
    SetEffectiveNull,
}

impl CorrectionOperation {
    /// Return the stable persisted spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceEffectiveText => "REPLACE_EFFECTIVE_TEXT",
            Self::SetEffectiveEmpty => "SET_EFFECTIVE_EMPTY",
            Self::SetEffectiveNull => "SET_EFFECTIVE_NULL",
        }
    }
}

/// Conservative logical-type classification derived from the approved profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CorrectionLogicalType {
    /// Exact profile `logicalType: text`.
    Text,
    /// Every non-text or unrecognized profile logical type.
    NonText,
}

/// Immutable outcome represented by a completed issue-decision record.
///
/// Failed or incomplete commands are attempts, not `IssueDecision` records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionLifecycleOutcome {
    /// The immutable decision was atomically committed as one head generation.
    Effective,
}

/// Exact immutable issue, accepted-row, profile, and mapping authority scope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionScope {
    pub job_id: String,
    pub validation_run_id: String,
    pub issue_id: String,
    pub issue_hash: String,
    pub accepted_source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub canonical_entity_id: String,
    pub canonical_field_id: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
}

impl DecisionScope {
    /// Validate bounded syntax without claiming that the referenced evidence exists.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, live-part, tombstone, or inconsistent row
    /// identities. Repository services must additionally replay authority.
    pub fn validate(&self) -> Result<()> {
        validate_identity(&self.job_id, "job ID")?;
        validate_identity(&self.validation_run_id, "validation run ID")?;
        validate_identity(&self.issue_id, "issue ID")?;
        validate_sha256(&self.issue_hash, "issue hash")?;
        validate_source_file(&self.accepted_source_file)?;
        if self.source_ordinal == 0 || self.line_start == 0 || self.line_end < self.line_start {
            return decision_error(
                "accepted row sourceOrdinal/lineStart must be positive and lineEnd must not precede it",
            );
        }
        validate_identity(&self.canonical_entity_id, "canonical entity ID")?;
        validate_identity(&self.canonical_field_id, "canonical field ID")?;
        validate_identity(&self.profile_id, "profile ID")?;
        validate_version(&self.profile_version, "profile version")?;
        validate_sha256(&self.profile_hash, "profile hash")?;
        validate_identity(&self.mapping_id, "mapping ID")?;
        validate_version(&self.mapping_version, "mapping version")?;
        validate_sha256(&self.mapping_hash, "mapping hash")
    }
}

/// Exact optimistic-concurrency evidence supplied with every write.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionConcurrency {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_active_decision_id: Option<String>,
    pub expected_active_decision_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_effective_value_hash: Option<String>,
}

impl DecisionConcurrency {
    /// Validate the empty-head/generation relationship and optional value hash.
    ///
    /// # Errors
    ///
    /// Rejects a missing predecessor for a nonzero generation, a predecessor at
    /// generation zero, or a malformed effective-value hash.
    pub fn validate(&self) -> Result<()> {
        match (
            self.expected_active_decision_generation,
            self.expected_active_decision_id.as_deref(),
        ) {
            (0, None) => {}
            (0, Some(_)) => {
                return decision_error(
                    "expected active decision ID must be absent at generation zero",
                );
            }
            (_, None) => {
                return decision_error(
                    "expected active decision ID is required after generation zero",
                );
            }
            (_, Some(decision_id)) => {
                validate_identity(decision_id, "expected active decision ID")?;
            }
        }
        if let Some(hash) = &self.expected_effective_value_hash {
            validate_sha256(hash, "expected effective value hash")?;
        }
        Ok(())
    }
}

/// Profile-derived allow-list facts for one exact correction target.
///
/// A service may construct this only after replaying exact profile, mapping,
/// issue, row, and integrity authority. Merely deserializing this type grants no
/// authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct TextualCorrectionEligibility {
    pub exact_scope_match: bool,
    pub root_entity: bool,
    pub singular: bool,
    pub logical_type: CorrectionLogicalType,
    pub sensitive: bool,
    pub required: bool,
    pub maximum_length: Option<usize>,
}

impl TextualCorrectionEligibility {
    fn validate(&self) -> Result<()> {
        if !self.exact_scope_match
            || !self.root_entity
            || !self.singular
            || self.logical_type != CorrectionLogicalType::Text
            || self.sensitive
        {
            return decision_error(
                "correction target must be an exact, root, singular, nonsensitive text field",
            );
        }
        if matches!(self.maximum_length, Some(0))
            || self
                .maximum_length
                .is_some_and(|maximum| maximum > MAXIMUM_CORRECTION_VALUE_CHARS)
        {
            return decision_error(format!(
                "text-field maximumLength must be in 1..={MAXIMUM_CORRECTION_VALUE_CHARS}"
            ));
        }
        Ok(())
    }
}

/// Client-supplied correction operation and exact textual payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CorrectionOverlayRequest {
    pub operation: CorrectionOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_text: Option<String>,
}

impl CorrectionOverlayRequest {
    /// Validate operation/payload shape and absolute textual bounds.
    ///
    /// # Errors
    ///
    /// Rejects implicit empty/null semantics, controls, or oversized text.
    pub fn validate(&self) -> Result<()> {
        match self.operation {
            CorrectionOperation::ReplaceEffectiveText => {
                let value = self.replacement_text.as_deref().ok_or_else(|| {
                    invalid_decision("REPLACE_EFFECTIVE_TEXT requires replacementText".to_owned())
                })?;
                if value.is_empty() {
                    return decision_error(
                        "REPLACE_EFFECTIVE_TEXT cannot encode empty; use SET_EFFECTIVE_EMPTY",
                    );
                }
                validate_text_value(value, None)?;
            }
            CorrectionOperation::SetEffectiveEmpty | CorrectionOperation::SetEffectiveNull => {
                if self.replacement_text.is_some() {
                    return decision_error(
                        "SET_EFFECTIVE_EMPTY and SET_EFFECTIVE_NULL forbid replacementText",
                    );
                }
            }
        }
        Ok(())
    }

    /// Validate this operation against exact profile-derived text eligibility.
    ///
    /// # Errors
    ///
    /// Rejects non-text, sensitive, non-root, plural, mismatched, required-null/
    /// empty, or above-profile-length corrections.
    pub fn validate_for_eligibility(
        &self,
        eligibility: &TextualCorrectionEligibility,
    ) -> Result<()> {
        self.validate()?;
        eligibility.validate()?;
        match self.operation {
            CorrectionOperation::ReplaceEffectiveText => validate_text_value(
                self.replacement_text.as_deref().ok_or_else(|| {
                    invalid_decision("replacementText disappeared after validation".to_owned())
                })?,
                eligibility.maximum_length,
            ),
            CorrectionOperation::SetEffectiveEmpty | CorrectionOperation::SetEffectiveNull
                if eligibility.required =>
            {
                decision_error("required text fields cannot be set empty or null")
            }
            CorrectionOperation::SetEffectiveEmpty | CorrectionOperation::SetEffectiveNull => {
                Ok(())
            }
        }
    }

    /// Derive the exact effective textual/null value without normalization.
    pub fn effective_value(&self) -> Option<&str> {
        match self.operation {
            CorrectionOperation::ReplaceEffectiveText => self.replacement_text.as_deref(),
            CorrectionOperation::SetEffectiveEmpty => Some(""),
            CorrectionOperation::SetEffectiveNull => None,
        }
    }
}

/// Strict, bounded command contract for one issue disposition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueDecisionRequest {
    pub schema_version: u32,
    pub idempotency_key: String,
    pub disposition: IssueDisposition,
    pub scope: DecisionScope,
    pub concurrency: DecisionConcurrency,
    pub practitioner_id: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_reference: Option<String>,
    pub authority_separation_acknowledged: bool,
    pub row_scope_exclusion_acknowledged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_overlay: Option<CorrectionOverlayRequest>,
}

impl IssueDecisionRequest {
    /// Validate closed disposition semantics and all bounded request evidence.
    ///
    /// # Errors
    ///
    /// Rejects unknown versions, malformed identity/concurrency, missing
    /// attribution/reason, invalid acknowledgments, or cross-disposition data.
    pub fn validate(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_idempotency_key(&self.idempotency_key)?;
        self.scope.validate()?;
        self.concurrency.validate()?;
        validate_attribution(
            &self.practitioner_id,
            &self.reason,
            self.evidence_reference.as_deref(),
        )?;
        if !self.authority_separation_acknowledged {
            return decision_error(
                "decision must acknowledge that it grants no result, publication, or target authority",
            );
        }

        match self.disposition {
            IssueDisposition::Corrected => {
                if self.row_scope_exclusion_acknowledged {
                    return decision_error("CORRECTED cannot carry a row-exclusion acknowledgment");
                }
                require_effective_hash(&self.concurrency, self.disposition)?;
                self.correction_overlay
                    .as_ref()
                    .ok_or_else(|| {
                        invalid_decision("CORRECTED requires correctionOverlay".to_owned())
                    })?
                    .validate()?;
            }
            IssueDisposition::AcceptedAsIs => {
                reject_overlay(self)?;
                reject_row_acknowledgment(self)?;
                require_effective_hash(&self.concurrency, self.disposition)?;
            }
            IssueDisposition::Waived => {
                reject_overlay(self)?;
                reject_row_acknowledgment(self)?;
                reject_effective_hash(self)?;
            }
            IssueDisposition::Excluded => {
                reject_overlay(self)?;
                reject_effective_hash(self)?;
                if !self.row_scope_exclusion_acknowledged {
                    return decision_error("EXCLUDED requires explicit row-scope acknowledgment");
                }
            }
        }
        Ok(())
    }

    /// Validate a `CORRECTED` request against exact profile-derived eligibility.
    ///
    /// # Errors
    ///
    /// Rejects non-correction dispositions and any ineligible textual target.
    pub fn validate_correction_eligibility(
        &self,
        eligibility: &TextualCorrectionEligibility,
    ) -> Result<()> {
        self.validate()?;
        if self.disposition != IssueDisposition::Corrected {
            return decision_error("textual eligibility applies only to CORRECTED");
        }
        self.correction_overlay
            .as_ref()
            .ok_or_else(|| invalid_decision("CORRECTED requires correctionOverlay".to_owned()))?
            .validate_for_eligibility(eligibility)
    }
}

/// Immutable persisted correction overlay. It remains inert until effectivation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CorrectionOverlay {
    pub schema_version: u32,
    pub overlay_id: String,
    pub decision_request_hash: String,
    pub scope: DecisionScope,
    pub prior_effective_value_hash: String,
    pub operation: CorrectionOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_text: Option<String>,
    pub practitioner_id: String,
    pub reason: String,
    pub created_at: String,
    pub overlay_hash: String,
}

impl CorrectionOverlay {
    /// Validate immutable overlay bytes and their canonical hash.
    ///
    /// # Errors
    ///
    /// Rejects invalid identity/scope/attribution/value/time/hash or a mismatch
    /// between stored and replayed canonical overlay evidence.
    pub fn validate(&self, eligibility: &TextualCorrectionEligibility) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_identity(&self.overlay_id, "overlay ID")?;
        validate_sha256(&self.decision_request_hash, "decision request hash")?;
        self.scope.validate()?;
        validate_sha256(
            &self.prior_effective_value_hash,
            "prior effective value hash",
        )?;
        validate_attribution(&self.practitioner_id, &self.reason, None)?;
        validate_timestamp(&self.created_at, "overlay createdAt")?;
        CorrectionOverlayRequest {
            operation: self.operation,
            replacement_text: self.replacement_text.clone(),
        }
        .validate_for_eligibility(eligibility)?;
        validate_sha256(&self.overlay_hash, "overlay hash")?;
        if canonical_overlay_hash(self)? != self.overlay_hash {
            return decision_error("stored overlay hash differs from canonical overlay bytes");
        }
        Ok(())
    }

    /// Derive the exact effective textual/null value without normalization.
    pub fn effective_value(&self) -> Option<&str> {
        match self.operation {
            CorrectionOperation::ReplaceEffectiveText => self.replacement_text.as_deref(),
            CorrectionOperation::SetEffectiveEmpty => Some(""),
            CorrectionOperation::SetEffectiveNull => None,
        }
    }
}

/// Immutable completed issue-decision evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueDecision {
    pub schema_version: u32,
    pub decision_id: String,
    pub request_hash: String,
    pub idempotency_key: String,
    pub disposition: IssueDisposition,
    pub scope: DecisionScope,
    pub concurrency: DecisionConcurrency,
    pub practitioner_id: String,
    pub application_timestamp: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_reference: Option<String>,
    pub authority_separation_acknowledged: bool,
    pub row_scope_exclusion_acknowledged: bool,
    pub active_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_overlay_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reevaluation_id: Option<String>,
    pub lifecycle_outcome: DecisionLifecycleOutcome,
    pub decision_hash: String,
}

impl IssueDecision {
    /// Validate completed decision evidence and replay its canonical hash.
    ///
    /// # Errors
    ///
    /// Rejects malformed, inconsistent, incomplete, or hash-mismatched records.
    pub fn validate(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_identity(&self.decision_id, "decision ID")?;
        validate_sha256(&self.request_hash, "decision request hash")?;
        validate_idempotency_key(&self.idempotency_key)?;
        self.scope.validate()?;
        self.concurrency.validate()?;
        validate_attribution(
            &self.practitioner_id,
            &self.reason,
            self.evidence_reference.as_deref(),
        )?;
        validate_timestamp(&self.application_timestamp, "application timestamp")?;
        if !self.authority_separation_acknowledged {
            return decision_error("completed decision lacks authority-separation acknowledgment");
        }
        let expected_generation = self
            .concurrency
            .expected_active_decision_generation
            .checked_add(1)
            .ok_or_else(|| invalid_decision("active decision generation overflow".to_owned()))?;
        if self.active_generation != expected_generation {
            return decision_error(format!(
                "activeGeneration must be {expected_generation} for the supplied concurrency evidence"
            ));
        }

        match self.disposition {
            IssueDisposition::Corrected => {
                reject_row_acknowledgment_fields(self.row_scope_exclusion_acknowledged)?;
                require_optional_hash(
                    self.concurrency.expected_effective_value_hash.as_deref(),
                    "CORRECTED expected effective value hash",
                )?;
                validate_required_identity(
                    self.correction_overlay_id.as_deref(),
                    "CORRECTED correction overlay ID",
                )?;
                validate_required_identity(
                    self.reevaluation_id.as_deref(),
                    "CORRECTED re-evaluation ID",
                )?;
            }
            IssueDisposition::AcceptedAsIs => {
                reject_row_acknowledgment_fields(self.row_scope_exclusion_acknowledged)?;
                require_optional_hash(
                    self.concurrency.expected_effective_value_hash.as_deref(),
                    "ACCEPTED_AS_IS expected effective value hash",
                )?;
                reject_completion_links(self)?;
            }
            IssueDisposition::Waived => {
                reject_row_acknowledgment_fields(self.row_scope_exclusion_acknowledged)?;
                reject_record_effective_hash(self)?;
                reject_completion_links(self)?;
            }
            IssueDisposition::Excluded => {
                if !self.row_scope_exclusion_acknowledged {
                    return decision_error(
                        "EXCLUDED decision lacks explicit row-scope acknowledgment",
                    );
                }
                reject_record_effective_hash(self)?;
                reject_completion_links(self)?;
            }
        }
        validate_sha256(&self.decision_hash, "decision hash")?;
        if canonical_decision_hash(self)? != self.decision_hash {
            return decision_error("stored decision hash differs from canonical decision bytes");
        }
        Ok(())
    }
}

/// Return canonical, domain-separated request material.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_decision_request_material(request: &IssueDecisionRequest) -> Result<Value> {
    Ok(canonicalize_json(&json!({
        "contract": REQUEST_CONTRACT,
        "request": request,
    })))
}

/// Hash canonical, domain-separated request material.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_decision_request_hash(request: &IssueDecisionRequest) -> Result<String> {
    canonical_hash(&canonical_decision_request_material(request)?)
}

/// Return canonical, domain-separated overlay material, excluding its hash.
///
/// # Errors
///
/// Rejects the impossible case where a serialized overlay is not an object.
pub fn canonical_overlay_material(overlay: &CorrectionOverlay) -> Result<Value> {
    canonical_material_without_hash(OVERLAY_CONTRACT, overlay, "overlayHash")
}

/// Hash canonical, domain-separated overlay material.
///
/// # Errors
///
/// Returns serialization or material-shape errors through the crate contract.
pub fn canonical_overlay_hash(overlay: &CorrectionOverlay) -> Result<String> {
    canonical_hash(&canonical_overlay_material(overlay)?)
}

/// Return canonical, domain-separated decision material, excluding its hash.
///
/// # Errors
///
/// Rejects the impossible case where a serialized decision is not an object.
pub fn canonical_decision_material(decision: &IssueDecision) -> Result<Value> {
    canonical_material_without_hash(DECISION_CONTRACT, decision, "decisionHash")
}

/// Hash canonical, domain-separated decision material.
///
/// # Errors
///
/// Returns serialization or material-shape errors through the crate contract.
pub fn canonical_decision_hash(decision: &IssueDecision) -> Result<String> {
    canonical_hash(&canonical_decision_material(decision)?)
}

fn canonical_material_without_hash<T: Serialize>(
    contract: &str,
    record: &T,
    hash_field: &str,
) -> Result<Value> {
    let mut record = serde_json::to_value(record)?;
    let object = record.as_object_mut().ok_or_else(|| {
        invalid_decision("canonical decision evidence must serialize as an object".to_owned())
    })?;
    object.remove(hash_field).ok_or_else(|| {
        invalid_decision(format!("canonical decision evidence lacks {hash_field}"))
    })?;
    Ok(canonicalize_json(&json!({
        "contract": contract,
        "record": record,
    })))
}

fn validate_schema_version(schema_version: u32) -> Result<()> {
    if schema_version != DECISION_SCHEMA_VERSION {
        return decision_error(format!(
            "decision schemaVersion must be {DECISION_SCHEMA_VERSION}; found {schema_version}"
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
        return decision_error(format!(
            "{label} must be a bounded ASCII letter/digit/._-: token"
        ));
    }
    Ok(())
}

fn validate_version(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return decision_error(format!(
            "{label} must be a bounded ASCII letter/digit/._- token"
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
        return decision_error(format!(
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
        return decision_error("accepted source file must be a bounded safe relative path");
    }
    for segment in value.split('/') {
        let is_live_or_inert_internal = Path::new(segment)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("part")
                    || extension.eq_ignore_ascii_case("tombstone")
            });
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.trim() != segment
            || segment.eq_ignore_ascii_case(".part")
            || segment.eq_ignore_ascii_case(".tombstone")
            || is_live_or_inert_internal
        {
            return decision_error(
                "accepted source file cannot use traversal, live-part, or tombstone names",
            );
        }
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
        return decision_error("idempotency key must be a bounded ASCII letter/digit/._-: token");
    }
    Ok(())
}

fn validate_attribution(
    practitioner_id: &str,
    reason: &str,
    evidence_reference: Option<&str>,
) -> Result<()> {
    validate_bounded_text(
        practitioner_id,
        "practitioner ID",
        MAXIMUM_PRACTITIONER_BYTES,
        true,
    )?;
    validate_bounded_text(reason, "reason", MAXIMUM_REASON_BYTES, false)?;
    if let Some(reference) = evidence_reference {
        validate_bounded_text(
            reference,
            "evidence reference",
            MAXIMUM_EVIDENCE_REFERENCE_BYTES,
            false,
        )?;
    }
    Ok(())
}

fn validate_bounded_text(value: &str, label: &str, maximum: usize, exact_trim: bool) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim().is_empty()
        || (exact_trim && value.trim() != value)
        || value.chars().any(char::is_control)
    {
        return decision_error(format!(
            "{label} must be nonblank, control-free, and at most {maximum} UTF-8 bytes"
        ));
    }
    Ok(())
}

fn validate_text_value(value: &str, profile_maximum: Option<usize>) -> Result<()> {
    if value.len() > MAXIMUM_CORRECTION_VALUE_BYTES {
        return decision_error(format!(
            "replacement text exceeds {MAXIMUM_CORRECTION_VALUE_BYTES} UTF-8 bytes"
        ));
    }
    let character_count = value.chars().count();
    if character_count > MAXIMUM_CORRECTION_VALUE_CHARS
        || profile_maximum.is_some_and(|maximum| character_count > maximum)
    {
        return decision_error("replacement text exceeds its bounded profile length");
    }
    if value.chars().any(char::is_control) {
        return decision_error("replacement text contains a control character");
    }
    Ok(())
}

fn validate_timestamp(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || value.chars().any(char::is_control)
        || DateTime::parse_from_rfc3339(value).is_err()
    {
        return decision_error(format!("{label} must be a bounded RFC 3339 timestamp"));
    }
    Ok(())
}

fn require_effective_hash(
    concurrency: &DecisionConcurrency,
    disposition: IssueDisposition,
) -> Result<()> {
    require_optional_hash(
        concurrency.expected_effective_value_hash.as_deref(),
        &format!("{} expected effective value hash", disposition.as_str()),
    )
}

fn require_optional_hash(value: Option<&str>, label: &str) -> Result<()> {
    let value = value.ok_or_else(|| invalid_decision(format!("{label} is required")))?;
    validate_sha256(value, label)
}

fn reject_overlay(request: &IssueDecisionRequest) -> Result<()> {
    if request.correction_overlay.is_some() {
        return decision_error(format!(
            "{} cannot carry correctionOverlay",
            request.disposition.as_str()
        ));
    }
    Ok(())
}

fn reject_row_acknowledgment(request: &IssueDecisionRequest) -> Result<()> {
    reject_row_acknowledgment_fields(request.row_scope_exclusion_acknowledged)
}

fn reject_row_acknowledgment_fields(acknowledged: bool) -> Result<()> {
    if acknowledged {
        return decision_error("row-scope acknowledgment is reserved for EXCLUDED");
    }
    Ok(())
}

fn reject_effective_hash(request: &IssueDecisionRequest) -> Result<()> {
    if request.concurrency.expected_effective_value_hash.is_some() {
        return decision_error(format!(
            "{} cannot carry expectedEffectiveValueHash",
            request.disposition.as_str()
        ));
    }
    Ok(())
}

fn reject_record_effective_hash(decision: &IssueDecision) -> Result<()> {
    if decision.concurrency.expected_effective_value_hash.is_some() {
        return decision_error(format!(
            "{} cannot carry expectedEffectiveValueHash",
            decision.disposition.as_str()
        ));
    }
    Ok(())
}

fn validate_required_identity(value: Option<&str>, label: &str) -> Result<()> {
    let value = value.ok_or_else(|| invalid_decision(format!("{label} is required")))?;
    validate_identity(value, label)
}

fn reject_completion_links(decision: &IssueDecision) -> Result<()> {
    if decision.correction_overlay_id.is_some() || decision.reevaluation_id.is_some() {
        return decision_error(format!(
            "{} cannot carry correction or re-evaluation identity",
            decision.disposition.as_str()
        ));
    }
    Ok(())
}

fn invalid_decision(message: String) -> KernelError {
    KernelError::DecisionInvalid(message)
}

fn decision_error<T>(message: impl Into<String>) -> Result<T> {
    Err(invalid_decision(message.into()))
}

#[cfg(test)]
mod tests {
    use jsonschema::PatternOptions;
    use serde_json::{Value, json};

    use super::*;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn scope() -> DecisionScope {
        DecisionScope {
            job_id: "job-1".to_owned(),
            validation_run_id: "validation-1".to_owned(),
            issue_id: "issue-a".to_owned(),
            issue_hash: HASH_A.to_owned(),
            accepted_source_file: "customers.csv".to_owned(),
            source_ordinal: 7,
            line_start: 9,
            line_end: 9,
            canonical_entity_id: "Customer".to_owned(),
            canonical_field_id: "ExternalId".to_owned(),
            profile_id: "customer-account-profile".to_owned(),
            profile_version: "1.0.0".to_owned(),
            profile_hash: HASH_B.to_owned(),
            mapping_id: "customer-account-mapping".to_owned(),
            mapping_version: "1.0.0".to_owned(),
            mapping_hash: HASH_C.to_owned(),
        }
    }

    fn concurrency(with_value: bool) -> DecisionConcurrency {
        DecisionConcurrency {
            expected_active_decision_id: None,
            expected_active_decision_generation: 0,
            expected_effective_value_hash: with_value.then(|| HASH_A.to_owned()),
        }
    }

    fn eligibility() -> TextualCorrectionEligibility {
        TextualCorrectionEligibility {
            exact_scope_match: true,
            root_entity: true,
            singular: true,
            logical_type: CorrectionLogicalType::Text,
            sensitive: false,
            required: false,
            maximum_length: Some(128),
        }
    }

    fn corrected_request(value: &str) -> IssueDecisionRequest {
        IssueDecisionRequest {
            schema_version: DECISION_SCHEMA_VERSION,
            idempotency_key: "idem-0001".to_owned(),
            disposition: IssueDisposition::Corrected,
            scope: scope(),
            concurrency: concurrency(true),
            practitioner_id: "practitioner@example.test".to_owned(),
            reason: "Correct synthetic identifier while preserving source evidence".to_owned(),
            evidence_reference: Some("synthetic-case-1".to_owned()),
            authority_separation_acknowledged: true,
            row_scope_exclusion_acknowledged: false,
            correction_overlay: Some(CorrectionOverlayRequest {
                operation: CorrectionOperation::ReplaceEffectiveText,
                replacement_text: Some(value.to_owned()),
            }),
        }
    }

    fn sealed_overlay(value: &str) -> CorrectionOverlay {
        let mut overlay = CorrectionOverlay {
            schema_version: DECISION_SCHEMA_VERSION,
            overlay_id: "overlay-1".to_owned(),
            decision_request_hash: HASH_A.to_owned(),
            scope: scope(),
            prior_effective_value_hash: HASH_B.to_owned(),
            operation: CorrectionOperation::ReplaceEffectiveText,
            replacement_text: Some(value.to_owned()),
            practitioner_id: "practitioner@example.test".to_owned(),
            reason: "Correct synthetic identifier while preserving source evidence".to_owned(),
            created_at: "2026-07-22T12:00:00Z".to_owned(),
            overlay_hash: HASH_C.to_owned(),
        };
        overlay.overlay_hash = canonical_overlay_hash(&overlay).expect("hash overlay");
        overlay
    }

    fn sealed_decision(disposition: IssueDisposition) -> IssueDecision {
        let uses_value = matches!(
            disposition,
            IssueDisposition::Corrected | IssueDisposition::AcceptedAsIs
        );
        let corrected = disposition == IssueDisposition::Corrected;
        let excluded = disposition == IssueDisposition::Excluded;
        let mut decision = IssueDecision {
            schema_version: DECISION_SCHEMA_VERSION,
            decision_id: "decision-1".to_owned(),
            request_hash: HASH_A.to_owned(),
            idempotency_key: "idem-0001".to_owned(),
            disposition,
            scope: scope(),
            concurrency: concurrency(uses_value),
            practitioner_id: "practitioner@example.test".to_owned(),
            application_timestamp: "2026-07-22T12:00:01Z".to_owned(),
            reason: "Bounded synthetic practitioner decision".to_owned(),
            evidence_reference: Some("synthetic-case-1".to_owned()),
            authority_separation_acknowledged: true,
            row_scope_exclusion_acknowledged: excluded,
            active_generation: 1,
            correction_overlay_id: corrected.then(|| "overlay-1".to_owned()),
            reevaluation_id: corrected.then(|| "reevaluation-1".to_owned()),
            lifecycle_outcome: DecisionLifecycleOutcome::Effective,
            decision_hash: HASH_C.to_owned(),
        };
        decision.decision_hash = canonical_decision_hash(&decision).expect("hash decision");
        decision
    }

    #[test]
    fn closed_disposition_and_operation_vocabulary_rejects_unknown_values() {
        for disposition in [
            "DELETE_SOURCE",
            "AUTO_ACCEPT",
            "MASS_WAIVE",
            "CUSTOM",
            "corrected",
        ] {
            assert!(
                serde_json::from_value::<IssueDisposition>(json!(disposition)).is_err(),
                "unexpectedly accepted {disposition}"
            );
        }
        for operation in [
            "SQL",
            "REGEX_REPLACE",
            "AUTO_CORRECT",
            "replace_effective_text",
        ] {
            assert!(
                serde_json::from_value::<CorrectionOperation>(json!(operation)).is_err(),
                "unexpectedly accepted {operation}"
            );
        }
    }

    #[test]
    fn strict_serde_rejects_unknown_fields_at_every_contract_layer() {
        let mut request = serde_json::to_value(corrected_request("000123")).expect("serialize");
        request["unexpected"] = json!(true);
        assert!(serde_json::from_value::<IssueDecisionRequest>(request).is_err());

        let mut request = serde_json::to_value(corrected_request("000123")).expect("serialize");
        request["scope"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<IssueDecisionRequest>(request).is_err());

        let mut request = serde_json::to_value(corrected_request("000123")).expect("serialize");
        request["correctionOverlay"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<IssueDecisionRequest>(request).is_err());
    }

    #[test]
    fn all_four_dispositions_enforce_distinct_semantics() {
        let corrected = corrected_request("000123");
        corrected.validate().expect("valid correction structure");
        corrected
            .validate_correction_eligibility(&eligibility())
            .expect("eligible correction");

        let mut accepted = corrected_request("unused");
        accepted.disposition = IssueDisposition::AcceptedAsIs;
        accepted.correction_overlay = None;
        accepted.validate().expect("accepted as-is");

        let mut waived = accepted.clone();
        waived.disposition = IssueDisposition::Waived;
        waived.concurrency.expected_effective_value_hash = None;
        waived.validate().expect("waived");

        let mut excluded = waived;
        excluded.disposition = IssueDisposition::Excluded;
        excluded.row_scope_exclusion_acknowledged = true;
        excluded.validate().expect("excluded");

        let mut invalid = excluded;
        invalid.row_scope_exclusion_acknowledged = false;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn attribution_identity_idempotency_and_value_bounds_fail_closed() {
        let mut request = corrected_request("value");
        request.practitioner_id = " ".to_owned();
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.reason = "unsafe\nreason".to_owned();
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.evidence_reference = Some("x".repeat(MAXIMUM_EVIDENCE_REFERENCE_BYTES + 1));
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.idempotency_key = "../unsafe".to_owned();
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.scope.accepted_source_file = "attempt.part".to_owned();
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.scope.accepted_source_file = "internal.tombstone".to_owned();
        assert!(request.validate().is_err());

        let mut request = corrected_request("value");
        request.correction_overlay = Some(CorrectionOverlayRequest {
            operation: CorrectionOperation::ReplaceEffectiveText,
            replacement_text: Some("x".repeat(MAXIMUM_CORRECTION_VALUE_BYTES + 1)),
        });
        assert!(request.validate().is_err());
    }

    #[test]
    fn textual_eligibility_is_conservative_and_operation_specific() {
        let request = corrected_request("abcd");
        let mut policy = eligibility();
        policy.maximum_length = Some(3);
        assert!(request.validate_correction_eligibility(&policy).is_err());

        for mutate in [
            |policy: &mut TextualCorrectionEligibility| policy.exact_scope_match = false,
            |policy: &mut TextualCorrectionEligibility| policy.root_entity = false,
            |policy: &mut TextualCorrectionEligibility| policy.singular = false,
            |policy: &mut TextualCorrectionEligibility| policy.sensitive = true,
            |policy: &mut TextualCorrectionEligibility| {
                policy.logical_type = CorrectionLogicalType::NonText;
            },
        ] {
            let mut policy = eligibility();
            mutate(&mut policy);
            assert!(request.validate_correction_eligibility(&policy).is_err());
        }

        let required = TextualCorrectionEligibility {
            required: true,
            ..eligibility()
        };
        for operation in [
            CorrectionOperation::SetEffectiveEmpty,
            CorrectionOperation::SetEffectiveNull,
        ] {
            let mut request = corrected_request("unused");
            request.correction_overlay = Some(CorrectionOverlayRequest {
                operation,
                replacement_text: None,
            });
            assert!(request.validate_correction_eligibility(&required).is_err());
        }
    }

    #[test]
    fn textual_fidelity_preserves_identifiers_unicode_and_html_like_text() {
        let values = [
            "0000000000000123",
            "18446744073709551616000000000000000001",
            "1.2300E+04",
            "2026-07-22",
            " Αθήνα — 東京 — 😀 ",
            "<script>alert(&quot;x&quot;)</script>",
        ];
        let policy = TextualCorrectionEligibility {
            maximum_length: Some(256),
            ..eligibility()
        };
        for value in values {
            let request = corrected_request(value);
            request
                .validate_correction_eligibility(&policy)
                .expect("text remains eligible");
            let overlay = request.correction_overlay.as_ref().expect("overlay");
            assert_eq!(overlay.effective_value(), Some(value));
            let round_trip: IssueDecisionRequest = serde_json::from_value(
                serde_json::to_value(&request).expect("serialize decision request"),
            )
            .expect("deserialize decision request");
            assert_eq!(
                round_trip
                    .correction_overlay
                    .as_ref()
                    .expect("round-trip overlay")
                    .effective_value(),
                Some(value)
            );
        }

        let empty = CorrectionOverlayRequest {
            operation: CorrectionOperation::SetEffectiveEmpty,
            replacement_text: None,
        };
        empty
            .validate_for_eligibility(&eligibility())
            .expect("explicit empty");
        assert_eq!(empty.effective_value(), Some(""));

        let null = CorrectionOverlayRequest {
            operation: CorrectionOperation::SetEffectiveNull,
            replacement_text: None,
        };
        null.validate_for_eligibility(&eligibility())
            .expect("explicit null");
        assert_eq!(null.effective_value(), None);
    }

    #[test]
    fn canonical_material_is_domain_separated_deterministic_and_tamper_evident() {
        let request = corrected_request("000123");
        let first = canonical_decision_request_hash(&request).expect("hash request");
        assert_eq!(
            first, "9ff58cd503651baa370d9111bc4ba9b90e4adce6088ddc57651e63790861a52f",
            "the checked-in synthetic decision-request golden must not drift"
        );
        let round_trip: IssueDecisionRequest =
            serde_json::from_value(serde_json::to_value(&request).expect("serialize request"))
                .expect("deserialize request");
        assert_eq!(
            first,
            canonical_decision_request_hash(&round_trip).expect("rehash request")
        );

        let mut changed = request;
        changed.reason.push('!');
        assert_ne!(
            first,
            canonical_decision_request_hash(&changed).expect("hash changed request")
        );

        let overlay = sealed_overlay("000123");
        overlay.validate(&eligibility()).expect("valid overlay");
        assert_eq!(
            overlay.overlay_hash,
            "f0341f874b843661b131af5057cdc43498d655935c38e4864c9fd17d470884f1",
            "the checked-in synthetic overlay golden must not drift"
        );
        assert_ne!(
            canonical_overlay_hash(&overlay).expect("hash overlay"),
            first,
            "domain separation must distinguish request and overlay hashes"
        );

        for disposition in [
            IssueDisposition::Corrected,
            IssueDisposition::Waived,
            IssueDisposition::AcceptedAsIs,
            IssueDisposition::Excluded,
        ] {
            let decision = sealed_decision(disposition);
            if disposition == IssueDisposition::Corrected {
                assert_eq!(
                    decision.decision_hash,
                    "730dcc6d203ab506784708bc1e3603a284f760a905058372b96bc576a70c20d3",
                    "the checked-in synthetic corrected-decision golden must not drift"
                );
            }
            decision.validate().expect("valid sealed decision");
        }
    }

    fn schema(path: &str) -> Value {
        serde_json::from_str(match path {
            "issue" => include_str!("../schemas/decisions/issue-decision.schema.json"),
            "overlay" => include_str!("../schemas/decisions/correction-overlay.schema.json"),
            _ => unreachable!("known schema"),
        })
        .expect("parse JSON Schema")
    }

    fn assert_schema(instance: &Value, schema: &Value) {
        jsonschema::draft202012::meta::validate(schema).expect("valid draft 2020-12 schema");
        let validator = jsonschema::draft202012::options()
            .with_pattern_options(PatternOptions::regex())
            .build(schema)
            .expect("compile schema");
        assert!(
            validator.is_valid(instance),
            "schema errors: {:?}",
            validator.iter_errors(instance).collect::<Vec<_>>()
        );
    }

    #[test]
    fn committed_contracts_satisfy_strict_json_schemas() {
        let issue_schema = schema("issue");
        for disposition in [
            IssueDisposition::Corrected,
            IssueDisposition::Waived,
            IssueDisposition::AcceptedAsIs,
            IssueDisposition::Excluded,
        ] {
            let decision =
                serde_json::to_value(sealed_decision(disposition)).expect("decision JSON");
            assert_schema(&decision, &issue_schema);
        }

        let decision = serde_json::to_value(sealed_decision(IssueDisposition::Corrected))
            .expect("decision JSON");

        let overlay_schema = schema("overlay");
        let replacement = sealed_overlay("000123");
        assert_schema(
            &serde_json::to_value(&replacement).expect("overlay JSON"),
            &overlay_schema,
        );
        for operation in [
            CorrectionOperation::SetEffectiveEmpty,
            CorrectionOperation::SetEffectiveNull,
        ] {
            let mut overlay = replacement.clone();
            overlay.operation = operation;
            overlay.replacement_text = None;
            overlay.overlay_hash = canonical_overlay_hash(&overlay).expect("rehash overlay");
            overlay.validate(&eligibility()).expect("valid overlay");
            assert_schema(
                &serde_json::to_value(overlay).expect("overlay JSON"),
                &overlay_schema,
            );
        }

        let mut unknown = decision.clone();
        unknown["unknown"] = json!(true);
        let validator = jsonschema::draft202012::options()
            .with_pattern_options(PatternOptions::regex())
            .build(&issue_schema)
            .expect("compile issue schema");
        assert!(!validator.is_valid(&unknown));

        let mut unsupported = decision;
        unsupported["disposition"] = json!("AUTO_ACCEPT");
        assert!(!validator.is_valid(&unsupported));

        let overlay_validator = jsonschema::draft202012::options()
            .with_pattern_options(PatternOptions::regex())
            .build(&overlay_schema)
            .expect("compile overlay schema");
        let mut unsupported_overlay =
            serde_json::to_value(replacement).expect("replacement overlay JSON");
        unsupported_overlay["operation"] = json!("REGEX_REPLACE");
        assert!(!overlay_validator.is_valid(&unsupported_overlay));
    }
}

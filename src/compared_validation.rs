//! Pure compared-validation finding identities and set-difference evidence.
//!
//! This module deliberately knows nothing about `SQLite`, engagement state, or
//! source-package storage. Its caller must first replay the exact profile,
//! mapping, validation policy, engine, projected rows, and validation issues.
//! Received canonical values enter only the input-side subject material. The
//! durable outputs retain keyed fingerprints and immutable issue/row evidence,
//! never those values.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_bytes, canonical_json_string},
};

pub const COMPARED_FINDING_AUTHORITY_SCHEMA: &str =
    "migration-stream/compared-validation/authority/v1";
pub const COMPARED_FINDING_SET_INPUT_SCHEMA: &str =
    "migration-stream/compared-validation/finding-set-input/v1";
pub const COMPARED_FINDING_SET_SCHEMA: &str = "migration-stream/compared-validation/finding-set/v1";
pub const COMPARED_VALIDATION_INPUT_SCHEMA: &str = "migration-stream/compared-validation/input/v1";
pub const COMPARED_VALIDATION_SCHEMA: &str = "migration-stream/compared-validation/result/v1";

const RECORD_KEY_CONTRACT: &str = "migration-stream/compared-validation/entity-record-key/v1";
const PROJECTED_ROW_CONTRACT: &str =
    "migration-stream/compared-validation/projected-row-fallback/v1";
const UNIQUE_SCOPE_CONTRACT: &str = "migration-stream/compared-validation/unique-scope/v1";
const RELATIONSHIP_CONTRACT: &str = "migration-stream/compared-validation/relationship-subject/v1";
const FINDING_FINGERPRINT_CONTRACT: &str =
    "migration-stream/compared-validation/finding-fingerprint/v1";
const NAMESPACE_CONTRACT: &str = "migration-stream/compared-validation/pseudonym-namespace/v1";
const FINDING_MEMBERS_CONTRACT: &str = "migration-stream/compared-validation/finding-members/v1";
const FINDING_GROUP_CONTRACT: &str = "migration-stream/compared-validation/finding-group/v1";
const FINDING_SET_HASH_CONTRACT: &str = "migration-stream/compared-validation/finding-set-hash/v1";
const COMPARISON_INPUT_HASH_CONTRACT: &str =
    "migration-stream/compared-validation/comparison-input-hash/v1";
const COMPARISON_RESULT_HASH_CONTRACT: &str =
    "migration-stream/compared-validation/comparison-result-hash/v1";

const MAXIMUM_ISSUES: usize = 100_000;
const MAXIMUM_KEY_FIELDS: usize = 64;
const MAXIMUM_RELATIONSHIP_KEY_PAIRS: usize = 16;
const MAXIMUM_IDENTIFIER_BYTES: usize = 256;
const MAXIMUM_PATH_BYTES: usize = 1_024;
const MAXIMUM_MESSAGE_BYTES: usize = 4_096;
const MAXIMUM_REASON_BYTES: usize = 4_096;
const MAXIMUM_KEY_VALUE_BYTES: usize = 64 * 1024;
const MAXIMUM_TOTAL_SUBJECT_BYTES: usize = 64 * 1024 * 1024;
const SHA256_HEX_BYTES: usize = 64;
const HMAC_BLOCK_BYTES: usize = 64;

/// Session-local pseudonymization key. The integration layer must generate,
/// persist, and replay one exact random key per engagement. It must never
/// serialize this value into a read contract.
pub type ComparedValidationNamespaceKey = [u8; 32];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingAuthority {
    pub schema_version: String,
    pub engagement_id: String,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub rule_set_hash: String,
    pub validation_policy_hash: String,
    pub validation_engine_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalFieldValueInput {
    pub field_id: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordLocatorInput {
    /// Profile-declared entity key fields in their exact declared order.
    pub declared_key: Vec<CanonicalFieldValueInput>,
    /// Complete canonical projection, sorted by qualified field ID.
    pub projected_row: Vec<CanonicalFieldValueInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipKeyValueInput {
    pub from_field_id: String,
    pub to_field_id: String,
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationshipRole {
    Parent,
    Child,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "subjectKind",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum ComparedIssueSubjectInput {
    RuleRecord {
        record: RecordLocatorInput,
    },
    UniqueGroup {
        /// Exact normalized `scopeFields` tuple in profile-declared order.
        scope_values: Vec<CanonicalFieldValueInput>,
    },
    RelationshipRecord {
        record: RecordLocatorInput,
        relationship_id: String,
        role: RelationshipRole,
        cardinality: String,
        /// Exact declared key-pair identity and normalized role-side values.
        key_pairs: Vec<RelationshipKeyValueInput>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedIssueInput {
    pub issue_id: String,
    pub issue_hash: String,
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
    pub subject: ComparedIssueSubjectInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingSetInput {
    pub schema_version: String,
    pub authority: ComparedFindingAuthority,
    pub tour_id: String,
    pub validation_run_id: String,
    pub validation_result_hash: String,
    pub source_hash: String,
    pub manifest_hash: String,
    pub issues: Vec<ComparedIssueInput>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComparedSubjectKind {
    RuleRecord,
    UniqueGroup,
    RelationshipRecord,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComparedSubjectAlgorithm {
    EntityKeyHmacSha256V1,
    ProjectedRowHmacSha256V1,
    UniqueScopeHmacSha256V1,
    RelationshipEntityKeyHmacSha256V1,
    RelationshipProjectedRowHmacSha256V1,
}

/// Value-suppressed explanation of the stable identity material used to
/// compare one finding multiset across source revisions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComparedIdentityBasis {
    /// A private digest of the complete profile-declared record key.
    DeclaredKey,
    /// A private digest of the full canonical projected row because its
    /// declared key was incomplete.
    FullRowFallback,
    /// A private digest of the unique rule's ordered scope tuple.
    UniqueScope,
}

impl ComparedSubjectAlgorithm {
    /// Return the broad, value-free identity basis represented by this exact
    /// comparison algorithm.
    #[must_use]
    pub const fn identity_basis(self) -> ComparedIdentityBasis {
        match self {
            Self::EntityKeyHmacSha256V1 | Self::RelationshipEntityKeyHmacSha256V1 => {
                ComparedIdentityBasis::DeclaredKey
            }
            Self::ProjectedRowHmacSha256V1 | Self::RelationshipProjectedRowHmacSha256V1 => {
                ComparedIdentityBasis::FullRowFallback
            }
            Self::UniqueScopeHmacSha256V1 => ComparedIdentityBasis::UniqueScope,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingOccurrence {
    pub issue_id: String,
    pub issue_hash: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingGroup {
    pub fingerprint: String,
    pub subject_kind: ComparedSubjectKind,
    pub subject_algorithm: ComparedSubjectAlgorithm,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_role: Option<RelationshipRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_cardinality: Option<String>,
    pub occurrence_count: u64,
    pub occurrences: Vec<ComparedFindingOccurrence>,
    pub occurrence_set_hash: String,
    pub group_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingSet {
    pub schema_version: String,
    pub authority: ComparedFindingAuthority,
    pub authority_hash: String,
    /// Opaque equality guard proving both sets used the same secret namespace.
    pub pseudonym_namespace_id: String,
    pub tour_id: String,
    pub validation_run_id: String,
    pub validation_result_hash: String,
    pub source_hash: String,
    pub manifest_hash: String,
    pub finding_count: u64,
    pub issue_occurrence_count: u64,
    pub findings: Vec<ComparedFindingGroup>,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedValidationAttribution {
    pub actor: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedValidationInput {
    pub schema_version: String,
    pub prior: ComparedFindingSet,
    pub current: ComparedFindingSet,
    pub attribution: ComparedValidationAttribution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingSetReference {
    pub tour_id: String,
    pub validation_run_id: String,
    pub validation_result_hash: String,
    pub source_hash: String,
    pub manifest_hash: String,
    pub finding_set_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedFindingDelta {
    pub fingerprint: String,
    pub subject_kind: ComparedSubjectKind,
    pub subject_algorithm: ComparedSubjectAlgorithm,
    pub severity: String,
    pub rule_id: String,
    pub issue_code: String,
    pub display_message: String,
    pub entity_id: String,
    pub field_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_role: Option<RelationshipRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_cardinality: Option<String>,
    pub prior_occurrence_count: u64,
    pub current_occurrence_count: u64,
    pub closed_count: u64,
    pub remaining_count: u64,
    pub new_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_group_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_group_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedValidationCounts {
    pub closed: u64,
    pub remaining: u64,
    pub new: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparedValidation {
    pub schema_version: String,
    pub authority: ComparedFindingAuthority,
    pub authority_hash: String,
    pub pseudonym_namespace_id: String,
    pub prior: ComparedFindingSetReference,
    pub current: ComparedFindingSetReference,
    pub attribution: ComparedValidationAttribution,
    pub input_hash: String,
    pub counts: ComparedValidationCounts,
    pub findings: Vec<ComparedFindingDelta>,
    pub canonical_hash: String,
}

#[derive(Clone)]
struct PendingFinding {
    secret_material_json: String,
    group: ComparedFindingGroup,
}

struct SubjectFingerprint {
    secret_material: Value,
    subject_kind: ComparedSubjectKind,
    algorithm: ComparedSubjectAlgorithm,
    relationship_id: Option<String>,
    relationship_role: Option<RelationshipRole>,
    relationship_cardinality: Option<String>,
}

/// Build one value-suppressed finding multiset from independently verified
/// issue and projection facts.
///
/// # Errors
///
/// Rejects malformed authority, hashes, row evidence, incomplete fallback
/// projections, unsupported issue/subject combinations, bound excess, or a
/// cryptographic fingerprint collision with different canonical material.
pub fn build_compared_finding_set(
    namespace_key: &ComparedValidationNamespaceKey,
    input: &ComparedFindingSetInput,
) -> Result<ComparedFindingSet> {
    validate_finding_set_input(input)?;
    let authority_hash = authority_hash(&input.authority)?;
    let namespace_material = json!({
        "contract": NAMESPACE_CONTRACT,
        "engagementId": input.authority.engagement_id,
        "authorityHash": authority_hash,
    });
    let pseudonym_namespace_id = hmac_canonical(namespace_key, &namespace_material)?;
    let mut total_subject_bytes = 0_usize;
    let mut issue_ids = HashSet::with_capacity(input.issues.len());
    let mut pending = BTreeMap::<String, PendingFinding>::new();

    for issue in &input.issues {
        if !issue_ids.insert(issue.issue_id.as_str()) {
            return invalid("finding-set input repeats an issue ID");
        }
        let subject = subject_fingerprint(&input.authority, issue)?;
        let secret_material_json = canonical_json_string(&subject.secret_material)?;
        total_subject_bytes = total_subject_bytes
            .checked_add(secret_material_json.len())
            .ok_or_else(|| invalid_error("subject material byte count overflow"))?;
        if total_subject_bytes > MAXIMUM_TOTAL_SUBJECT_BYTES {
            return bound("compared finding subject material exceeds its cumulative bound");
        }
        let fingerprint = hmac_canonical(namespace_key, &subject.secret_material)?;
        let occurrence = ComparedFindingOccurrence {
            issue_id: issue.issue_id.clone(),
            issue_hash: issue.issue_hash.clone(),
            source_file: issue.source_file.clone(),
            source_ordinal: issue.source_ordinal,
            line_start: issue.line_start,
            line_end: issue.line_end,
        };
        if let Some(existing) = pending.get_mut(&fingerprint) {
            if existing.secret_material_json != secret_material_json {
                return integrity(
                    "one HMAC fingerprint resolved to different canonical subject material",
                );
            }
            existing.group.occurrences.push(occurrence);
            continue;
        }
        let group = ComparedFindingGroup {
            fingerprint: fingerprint.clone(),
            subject_kind: subject.subject_kind,
            subject_algorithm: subject.algorithm,
            severity: issue.severity.clone(),
            rule_id: issue.rule_id.clone(),
            issue_code: issue.issue_code.clone(),
            display_message: issue.display_message.clone(),
            entity_id: issue.entity_id.clone(),
            field_id: issue.field_id.clone(),
            relationship_id: subject.relationship_id,
            relationship_role: subject.relationship_role,
            relationship_cardinality: subject.relationship_cardinality,
            occurrence_count: 0,
            occurrences: vec![occurrence],
            occurrence_set_hash: String::new(),
            group_hash: String::new(),
        };
        pending.insert(
            fingerprint,
            PendingFinding {
                secret_material_json,
                group,
            },
        );
    }

    let mut findings = Vec::with_capacity(pending.len());
    for (_, mut pending_finding) in pending {
        pending_finding.group.occurrences.sort_by(occurrence_order);
        ensure_unique_occurrences(&pending_finding.group.occurrences)?;
        pending_finding.group.occurrence_count =
            u64::try_from(pending_finding.group.occurrences.len())
                .map_err(|_| bound_error("finding occurrence count does not fit u64"))?;
        pending_finding.group.occurrence_set_hash = occurrence_set_hash(&pending_finding.group)?;
        pending_finding.group.group_hash = finding_group_hash(&pending_finding.group)?;
        findings.push(pending_finding.group);
    }

    let finding_count =
        u64::try_from(findings.len()).map_err(|_| bound_error("finding count does not fit u64"))?;
    let issue_occurrence_count = u64::try_from(input.issues.len())
        .map_err(|_| bound_error("issue occurrence count does not fit u64"))?;
    let mut output = ComparedFindingSet {
        schema_version: COMPARED_FINDING_SET_SCHEMA.to_owned(),
        authority: input.authority.clone(),
        authority_hash,
        pseudonym_namespace_id,
        tour_id: input.tour_id.clone(),
        validation_run_id: input.validation_run_id.clone(),
        validation_result_hash: input.validation_result_hash.clone(),
        source_hash: input.source_hash.clone(),
        manifest_hash: input.manifest_hash.clone(),
        finding_count,
        issue_occurrence_count,
        findings,
        canonical_hash: String::new(),
    };
    output.canonical_hash = finding_set_hash(&output)?;
    validate_finding_set(&output)?;
    Ok(output)
}

/// Rebuild one finding set from its independently replayed inputs and require
/// byte-for-byte semantic equality with stored output.
///
/// # Errors
///
/// Fails on any authority, secret, algorithm, membership, count, link, or hash
/// drift.
pub fn verify_compared_finding_set_replay(
    namespace_key: &ComparedValidationNamespaceKey,
    input: &ComparedFindingSetInput,
    stored: &ComparedFindingSet,
) -> Result<()> {
    let expected = build_compared_finding_set(namespace_key, input)?;
    if &expected != stored {
        return integrity("stored compared finding set differs from independent replay");
    }
    Ok(())
}

/// Compare two immutable finding multisets under one exact semantic authority.
/// Each fingerprint contributes `min(prior,current)` REMAINING units and only
/// the cardinality delta contributes CLOSED or NEW units. No row-to-row match
/// or invented lineage is produced.
///
/// # Errors
///
/// Rejects malformed sets, authority/namespace drift, same-tour inputs,
/// conflicting fingerprint descriptors, invalid attribution, or bounds.
#[allow(clippy::too_many_lines)]
pub fn compare_finding_sets(input: &ComparedValidationInput) -> Result<ComparedValidation> {
    if input.schema_version != COMPARED_VALIDATION_INPUT_SCHEMA {
        return invalid("comparison input schema version is unsupported");
    }
    validate_finding_set(&input.prior)?;
    validate_finding_set(&input.current)?;
    validate_attribution(&input.attribution)?;
    if input.prior.authority != input.current.authority
        || input.prior.authority_hash != input.current.authority_hash
    {
        return conflict("finding sets use different profile/mapping/rule/policy/engine authority");
    }
    if input.prior.pseudonym_namespace_id != input.current.pseudonym_namespace_id {
        return conflict("finding sets use different pseudonym namespaces");
    }
    if input.prior.tour_id == input.current.tour_id
        || input.prior.validation_run_id == input.current.validation_run_id
    {
        return conflict("comparison requires distinct immutable tours and validation runs");
    }
    if input.prior.source_hash == input.current.source_hash {
        return conflict("comparison requires an explicitly revised source package");
    }

    let prior = input
        .prior
        .findings
        .iter()
        .map(|finding| (finding.fingerprint.as_str(), finding))
        .collect::<BTreeMap<_, _>>();
    let current = input
        .current
        .findings
        .iter()
        .map(|finding| (finding.fingerprint.as_str(), finding))
        .collect::<BTreeMap<_, _>>();
    let fingerprints = prior
        .keys()
        .chain(current.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    if fingerprints.len() > MAXIMUM_ISSUES {
        return bound("comparison finding count exceeds its absolute bound");
    }

    let mut counts = ComparedValidationCounts {
        closed: 0,
        remaining: 0,
        new: 0,
    };
    let mut findings = Vec::with_capacity(fingerprints.len());
    for fingerprint in fingerprints {
        let before = prior.get(fingerprint).copied();
        let after = current.get(fingerprint).copied();
        let descriptor = match (before, after) {
            (Some(before), Some(after)) => {
                if finding_descriptor(before) != finding_descriptor(after) {
                    return integrity(
                        "one finding fingerprint has conflicting public descriptors across tours",
                    );
                }
                before
            }
            (Some(before), None) => before,
            (None, Some(after)) => after,
            (None, None) => unreachable!("fingerprint union has one source"),
        };
        let prior_count = before.map_or(0, |finding| finding.occurrence_count);
        let current_count = after.map_or(0, |finding| finding.occurrence_count);
        let remaining_count = prior_count.min(current_count);
        let closed_count = prior_count.saturating_sub(current_count);
        let new_count = current_count.saturating_sub(prior_count);
        counts.closed = checked_count(counts.closed, closed_count, "closed finding count")?;
        counts.remaining =
            checked_count(counts.remaining, remaining_count, "remaining finding count")?;
        counts.new = checked_count(counts.new, new_count, "new finding count")?;
        findings.push(ComparedFindingDelta {
            fingerprint: fingerprint.to_owned(),
            subject_kind: descriptor.subject_kind,
            subject_algorithm: descriptor.subject_algorithm,
            severity: descriptor.severity.clone(),
            rule_id: descriptor.rule_id.clone(),
            issue_code: descriptor.issue_code.clone(),
            display_message: descriptor.display_message.clone(),
            entity_id: descriptor.entity_id.clone(),
            field_id: descriptor.field_id.clone(),
            relationship_id: descriptor.relationship_id.clone(),
            relationship_role: descriptor.relationship_role,
            relationship_cardinality: descriptor.relationship_cardinality.clone(),
            prior_occurrence_count: prior_count,
            current_occurrence_count: current_count,
            closed_count,
            remaining_count,
            new_count,
            prior_group_hash: before.map(|finding| finding.group_hash.clone()),
            current_group_hash: after.map(|finding| finding.group_hash.clone()),
        });
    }

    let authority = input.prior.authority.clone();
    let authority_hash = input.prior.authority_hash.clone();
    let prior_reference = finding_set_reference(&input.prior);
    let current_reference = finding_set_reference(&input.current);
    let input_hash = canonical_hash(&json!({
        "contract": COMPARISON_INPUT_HASH_CONTRACT,
        "authorityHash": authority_hash,
        "pseudonymNamespaceId": input.prior.pseudonym_namespace_id,
        "prior": prior_reference,
        "current": current_reference,
        "attribution": input.attribution,
    }))?;
    let mut output = ComparedValidation {
        schema_version: COMPARED_VALIDATION_SCHEMA.to_owned(),
        authority,
        authority_hash,
        pseudonym_namespace_id: input.prior.pseudonym_namespace_id.clone(),
        prior: prior_reference,
        current: current_reference,
        attribution: input.attribution.clone(),
        input_hash,
        counts,
        findings,
        canonical_hash: String::new(),
    };
    output.canonical_hash = comparison_hash(&output)?;
    validate_comparison(&output)?;
    Ok(output)
}

/// Independently recompute a compared result and require exact stored output.
///
/// # Errors
///
/// Fails on any count, group-link, algorithm, authority, attribution, input, or
/// canonical-hash drift.
pub fn verify_compared_validation_replay(
    input: &ComparedValidationInput,
    stored: &ComparedValidation,
) -> Result<()> {
    let expected = compare_finding_sets(input)?;
    if &expected != stored {
        return integrity("stored compared validation differs from independent replay");
    }
    Ok(())
}

fn subject_fingerprint(
    authority: &ComparedFindingAuthority,
    issue: &ComparedIssueInput,
) -> Result<SubjectFingerprint> {
    let common = json!({
        "authority": authority,
        "severity": issue.severity,
        "ruleId": issue.rule_id,
        "issueCode": issue.issue_code,
        "displayMessage": issue.display_message,
        "entityId": issue.entity_id,
        "fieldId": issue.field_id,
    });
    match &issue.subject {
        ComparedIssueSubjectInput::RuleRecord { record } => {
            if issue.issue_code == "UNIQUE_DUPLICATE"
                || issue.issue_code.starts_with("RELATIONSHIP_")
            {
                return invalid("rule-record subject does not match its issue code");
            }
            let (algorithm, locator) = record_locator_material(&issue.entity_id, record, false)?;
            Ok(SubjectFingerprint {
                secret_material: json!({
                    "contract": FINDING_FINGERPRINT_CONTRACT,
                    "subjectKind": "RULE_RECORD",
                    "common": common,
                    "recordLocator": locator,
                }),
                subject_kind: ComparedSubjectKind::RuleRecord,
                algorithm,
                relationship_id: None,
                relationship_role: None,
                relationship_cardinality: None,
            })
        }
        ComparedIssueSubjectInput::UniqueGroup { scope_values } => {
            if issue.issue_code != "UNIQUE_DUPLICATE" {
                return invalid("unique-group subject requires UNIQUE_DUPLICATE");
            }
            validate_nonempty_field_tuple(scope_values, "unique scope", true)?;
            Ok(SubjectFingerprint {
                secret_material: json!({
                    "contract": FINDING_FINGERPRINT_CONTRACT,
                    "subjectKind": "UNIQUE_GROUP",
                    "common": common,
                    "uniqueScope": {
                        "contract": UNIQUE_SCOPE_CONTRACT,
                        "entityId": issue.entity_id,
                        "values": scope_values,
                    },
                }),
                subject_kind: ComparedSubjectKind::UniqueGroup,
                algorithm: ComparedSubjectAlgorithm::UniqueScopeHmacSha256V1,
                relationship_id: None,
                relationship_role: None,
                relationship_cardinality: None,
            })
        }
        ComparedIssueSubjectInput::RelationshipRecord {
            record,
            relationship_id,
            role,
            cardinality,
            key_pairs,
        } => {
            if !issue.issue_code.starts_with("RELATIONSHIP_") {
                return invalid("relationship subject requires a relationship issue code");
            }
            validate_identifier(relationship_id, "relationship ID")?;
            validate_identifier(cardinality, "relationship cardinality")?;
            validate_relationship_pairs(key_pairs)?;
            let (algorithm, locator) = record_locator_material(&issue.entity_id, record, true)?;
            Ok(SubjectFingerprint {
                secret_material: json!({
                    "contract": FINDING_FINGERPRINT_CONTRACT,
                    "subjectKind": "RELATIONSHIP_RECORD",
                    "common": common,
                    "relationship": {
                        "contract": RELATIONSHIP_CONTRACT,
                        "relationshipId": relationship_id,
                        "role": role,
                        "cardinality": cardinality,
                        "keyPairs": key_pairs,
                    },
                    "recordLocator": locator,
                }),
                subject_kind: ComparedSubjectKind::RelationshipRecord,
                algorithm,
                relationship_id: Some(relationship_id.clone()),
                relationship_role: Some(*role),
                relationship_cardinality: Some(cardinality.clone()),
            })
        }
    }
}

fn record_locator_material(
    entity_id: &str,
    record: &RecordLocatorInput,
    relationship: bool,
) -> Result<(ComparedSubjectAlgorithm, Value)> {
    validate_record_locator(record)?;
    let complete_key = record.declared_key.iter().all(|field| {
        field
            .value
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    });
    if complete_key {
        let algorithm = if relationship {
            ComparedSubjectAlgorithm::RelationshipEntityKeyHmacSha256V1
        } else {
            ComparedSubjectAlgorithm::EntityKeyHmacSha256V1
        };
        Ok((
            algorithm,
            json!({
                "contract": RECORD_KEY_CONTRACT,
                "algorithm": algorithm,
                "entityId": entity_id,
                "values": record.declared_key,
            }),
        ))
    } else {
        let algorithm = if relationship {
            ComparedSubjectAlgorithm::RelationshipProjectedRowHmacSha256V1
        } else {
            ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1
        };
        Ok((
            algorithm,
            json!({
                "contract": PROJECTED_ROW_CONTRACT,
                "algorithm": algorithm,
                "entityId": entity_id,
                "values": record.projected_row,
            }),
        ))
    }
}

fn validate_finding_set_input(input: &ComparedFindingSetInput) -> Result<()> {
    if input.schema_version != COMPARED_FINDING_SET_INPUT_SCHEMA {
        return invalid("finding-set input schema version is unsupported");
    }
    validate_authority(&input.authority)?;
    validate_identifier(&input.tour_id, "tour ID")?;
    validate_identifier(&input.validation_run_id, "validation run ID")?;
    validate_hash(&input.validation_result_hash, "validation result hash")?;
    validate_hash(&input.source_hash, "source hash")?;
    validate_hash(&input.manifest_hash, "manifest hash")?;
    if input.issues.len() > MAXIMUM_ISSUES {
        return bound("finding-set input exceeds the maximum issue count");
    }
    for issue in &input.issues {
        validate_issue_input(issue)?;
    }
    Ok(())
}

fn validate_authority(authority: &ComparedFindingAuthority) -> Result<()> {
    if authority.schema_version != COMPARED_FINDING_AUTHORITY_SCHEMA {
        return invalid("compared authority schema version is unsupported");
    }
    for (value, label) in [
        (&authority.engagement_id, "engagement ID"),
        (&authority.profile_id, "profile ID"),
        (&authority.profile_version, "profile version"),
        (&authority.mapping_id, "mapping ID"),
        (&authority.mapping_version, "mapping version"),
        (
            &authority.validation_engine_version,
            "validation engine version",
        ),
    ] {
        validate_identifier(value, label)?;
    }
    for (value, label) in [
        (&authority.profile_hash, "profile hash"),
        (&authority.mapping_hash, "mapping hash"),
        (&authority.rule_set_hash, "rule-set hash"),
        (&authority.validation_policy_hash, "validation policy hash"),
    ] {
        validate_hash(value, label)?;
    }
    Ok(())
}

fn validate_issue_input(issue: &ComparedIssueInput) -> Result<()> {
    for (value, label) in [
        (&issue.issue_id, "issue ID"),
        (&issue.severity, "issue severity"),
        (&issue.rule_id, "rule ID"),
        (&issue.issue_code, "issue code"),
        (&issue.entity_id, "entity ID"),
        (&issue.field_id, "field ID"),
    ] {
        validate_identifier(value, label)?;
    }
    validate_hash(&issue.issue_hash, "issue hash")?;
    validate_bounded_text(
        &issue.display_message,
        "issue display message",
        MAXIMUM_MESSAGE_BYTES,
    )?;
    validate_bounded_text(&issue.source_file, "source file", MAXIMUM_PATH_BYTES)?;
    if issue.source_ordinal == 0 || issue.line_start == 0 || issue.line_end < issue.line_start {
        return invalid("issue row evidence is not a positive valid range");
    }
    Ok(())
}

fn validate_record_locator(record: &RecordLocatorInput) -> Result<()> {
    if record.declared_key.is_empty() {
        return invalid("record locator has no profile-declared entity key");
    }
    validate_field_tuple(&record.declared_key, "entity key", false)?;
    validate_field_tuple(&record.projected_row, "projected row", false)?;
    if record.projected_row.is_empty() {
        return invalid("record locator has no complete projected-row tuple");
    }
    let projected_fields = record
        .projected_row
        .iter()
        .map(|field| field.field_id.as_str())
        .collect::<HashSet<_>>();
    if record
        .declared_key
        .iter()
        .any(|field| !projected_fields.contains(field.field_id.as_str()))
    {
        return invalid("projected-row fallback omits a declared entity-key field");
    }
    let sorted = record
        .projected_row
        .windows(2)
        .all(|pair| pair[0].field_id < pair[1].field_id);
    if record.projected_row.len() > 1 && !sorted {
        return invalid("projected-row fallback fields are not strictly sorted");
    }
    Ok(())
}

fn validate_nonempty_field_tuple(
    values: &[CanonicalFieldValueInput],
    label: &str,
    require_nonempty_values: bool,
) -> Result<()> {
    if values.is_empty() {
        return invalid(&format!("{label} is empty"));
    }
    validate_field_tuple(values, label, require_nonempty_values)
}

fn validate_field_tuple(
    values: &[CanonicalFieldValueInput],
    label: &str,
    require_nonempty_values: bool,
) -> Result<()> {
    if values.len() > MAXIMUM_KEY_FIELDS {
        return bound(&format!("{label} exceeds its field-count bound"));
    }
    let mut fields = HashSet::with_capacity(values.len());
    for field in values {
        validate_identifier(&field.field_id, &format!("{label} field ID"))?;
        if !fields.insert(field.field_id.as_str()) {
            return invalid(&format!("{label} repeats a field ID"));
        }
        if let Some(value) = &field.value {
            validate_optional_subject_value(
                value,
                &format!("{label} value"),
                require_nonempty_values,
            )?;
        }
        if require_nonempty_values && field.value.as_deref().is_none_or(str::is_empty) {
            return invalid(&format!("{label} contains an empty value"));
        }
    }
    Ok(())
}

fn validate_relationship_pairs(values: &[RelationshipKeyValueInput]) -> Result<()> {
    if values.is_empty() || values.len() > MAXIMUM_RELATIONSHIP_KEY_PAIRS {
        return invalid("relationship key-pair count is outside the supported bound");
    }
    let mut pairs = HashSet::with_capacity(values.len());
    for pair in values {
        validate_identifier(&pair.from_field_id, "relationship from-field ID")?;
        validate_identifier(&pair.to_field_id, "relationship to-field ID")?;
        if !pairs.insert((pair.from_field_id.as_str(), pair.to_field_id.as_str())) {
            return invalid("relationship key identity repeats a field pair");
        }
        if let Some(value) = &pair.value {
            validate_optional_subject_value(value, "relationship key value", false)?;
        }
    }
    Ok(())
}

fn validate_optional_subject_value(value: &str, label: &str, require_nonempty: bool) -> Result<()> {
    if value.len() > MAXIMUM_KEY_VALUE_BYTES || value.chars().any(char::is_control) {
        return invalid(&format!(
            "{label} must contain at most {MAXIMUM_KEY_VALUE_BYTES} non-control UTF-8 bytes"
        ));
    }
    if require_nonempty && value.is_empty() {
        return invalid(&format!("{label} must not be empty"));
    }
    Ok(())
}

fn validate_finding_set(set: &ComparedFindingSet) -> Result<()> {
    if set.schema_version != COMPARED_FINDING_SET_SCHEMA {
        return integrity("stored finding-set schema version is unsupported");
    }
    validate_authority(&set.authority)?;
    if authority_hash(&set.authority)? != set.authority_hash {
        return integrity("stored finding-set authority hash differs from authority");
    }
    validate_hash(&set.authority_hash, "finding-set authority hash")?;
    validate_hash(
        &set.pseudonym_namespace_id,
        "finding-set pseudonym namespace",
    )?;
    validate_identifier(&set.tour_id, "finding-set tour ID")?;
    validate_identifier(&set.validation_run_id, "finding-set validation run ID")?;
    validate_hash(
        &set.validation_result_hash,
        "finding-set validation result hash",
    )?;
    validate_hash(&set.source_hash, "finding-set source hash")?;
    validate_hash(&set.manifest_hash, "finding-set manifest hash")?;
    if set.findings.len() > MAXIMUM_ISSUES
        || u64::try_from(set.findings.len()).ok() != Some(set.finding_count)
    {
        return integrity("stored finding-set count is invalid");
    }
    let mut prior_fingerprint = "";
    let mut occurrence_count = 0_u64;
    let mut issue_ids = HashSet::new();
    for finding in &set.findings {
        if !prior_fingerprint.is_empty() && prior_fingerprint >= finding.fingerprint.as_str() {
            return integrity("stored findings are not strictly fingerprint-sorted");
        }
        prior_fingerprint = &finding.fingerprint;
        validate_finding_group(finding)?;
        occurrence_count = checked_count(
            occurrence_count,
            finding.occurrence_count,
            "stored occurrence count",
        )?;
        for occurrence in &finding.occurrences {
            if !issue_ids.insert(occurrence.issue_id.as_str()) {
                return integrity("one issue occurs in multiple stored finding groups");
            }
        }
    }
    if occurrence_count != set.issue_occurrence_count {
        return integrity("stored finding-set occurrence total is invalid");
    }
    if finding_set_hash(set)? != set.canonical_hash {
        return integrity("stored finding-set canonical hash is invalid");
    }
    Ok(())
}

fn validate_finding_group(group: &ComparedFindingGroup) -> Result<()> {
    validate_hash(&group.fingerprint, "finding fingerprint")?;
    for (value, label) in [
        (&group.severity, "finding severity"),
        (&group.rule_id, "finding rule ID"),
        (&group.issue_code, "finding issue code"),
        (&group.entity_id, "finding entity ID"),
        (&group.field_id, "finding field ID"),
    ] {
        validate_identifier(value, label)?;
    }
    validate_bounded_text(
        &group.display_message,
        "finding display message",
        MAXIMUM_MESSAGE_BYTES,
    )?;
    match group.subject_kind {
        ComparedSubjectKind::RelationshipRecord => {
            if group.relationship_id.is_none()
                || group.relationship_role.is_none()
                || group.relationship_cardinality.is_none()
                || !matches!(
                    group.subject_algorithm,
                    ComparedSubjectAlgorithm::RelationshipEntityKeyHmacSha256V1
                        | ComparedSubjectAlgorithm::RelationshipProjectedRowHmacSha256V1
                )
            {
                return integrity("relationship finding descriptor is incomplete");
            }
        }
        ComparedSubjectKind::RuleRecord => {
            if group.relationship_id.is_some()
                || group.relationship_role.is_some()
                || group.relationship_cardinality.is_some()
                || !matches!(
                    group.subject_algorithm,
                    ComparedSubjectAlgorithm::EntityKeyHmacSha256V1
                        | ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1
                )
            {
                return integrity("rule-record finding descriptor is inconsistent");
            }
        }
        ComparedSubjectKind::UniqueGroup => {
            if group.relationship_id.is_some()
                || group.relationship_role.is_some()
                || group.relationship_cardinality.is_some()
                || group.subject_algorithm != ComparedSubjectAlgorithm::UniqueScopeHmacSha256V1
            {
                return integrity("unique-group finding descriptor is inconsistent");
            }
        }
    }
    if group.occurrences.is_empty()
        || u64::try_from(group.occurrences.len()).ok() != Some(group.occurrence_count)
    {
        return integrity("finding occurrence count is invalid");
    }
    let mut sorted = group.occurrences.clone();
    sorted.sort_by(occurrence_order);
    if sorted != group.occurrences {
        return integrity("finding occurrences are not canonically sorted");
    }
    ensure_unique_occurrences(&group.occurrences)?;
    if occurrence_set_hash(group)? != group.occurrence_set_hash {
        return integrity("finding occurrence-set hash is invalid");
    }
    if finding_group_hash(group)? != group.group_hash {
        return integrity("finding group hash is invalid");
    }
    Ok(())
}

fn validate_comparison(comparison: &ComparedValidation) -> Result<()> {
    if comparison.schema_version != COMPARED_VALIDATION_SCHEMA {
        return integrity("stored comparison schema version is unsupported");
    }
    validate_authority(&comparison.authority)?;
    if authority_hash(&comparison.authority)? != comparison.authority_hash {
        return integrity("stored comparison authority hash is invalid");
    }
    validate_hash(
        &comparison.pseudonym_namespace_id,
        "comparison pseudonym namespace",
    )?;
    validate_attribution(&comparison.attribution)?;
    validate_hash(&comparison.input_hash, "comparison input hash")?;
    let mut prior = "";
    let mut counts = ComparedValidationCounts {
        closed: 0,
        remaining: 0,
        new: 0,
    };
    for finding in &comparison.findings {
        if !prior.is_empty() && prior >= finding.fingerprint.as_str() {
            return integrity("compared findings are not strictly fingerprint-sorted");
        }
        prior = &finding.fingerprint;
        validate_hash(&finding.fingerprint, "compared finding fingerprint")?;
        if finding.closed_count
            != finding
                .prior_occurrence_count
                .saturating_sub(finding.current_occurrence_count)
            || finding.remaining_count
                != finding
                    .prior_occurrence_count
                    .min(finding.current_occurrence_count)
            || finding.new_count
                != finding
                    .current_occurrence_count
                    .saturating_sub(finding.prior_occurrence_count)
        {
            return integrity("compared finding multiset counts are inconsistent");
        }
        if (finding.prior_occurrence_count == 0) != finding.prior_group_hash.is_none()
            || (finding.current_occurrence_count == 0) != finding.current_group_hash.is_none()
        {
            return integrity("compared finding group links are inconsistent");
        }
        if let Some(hash) = &finding.prior_group_hash {
            validate_hash(hash, "prior finding group hash")?;
        }
        if let Some(hash) = &finding.current_group_hash {
            validate_hash(hash, "current finding group hash")?;
        }
        counts.closed = checked_count(counts.closed, finding.closed_count, "closed count")?;
        counts.remaining =
            checked_count(counts.remaining, finding.remaining_count, "remaining count")?;
        counts.new = checked_count(counts.new, finding.new_count, "new count")?;
    }
    if counts != comparison.counts {
        return integrity("stored comparison totals differ from finding deltas");
    }
    if comparison_hash(comparison)? != comparison.canonical_hash {
        return integrity("stored comparison canonical hash is invalid");
    }
    Ok(())
}

fn authority_hash(authority: &ComparedFindingAuthority) -> Result<String> {
    canonical_hash(&serde_json::to_value(authority)?)
}

fn occurrence_set_hash(group: &ComparedFindingGroup) -> Result<String> {
    canonical_hash(&json!({
        "contract": FINDING_MEMBERS_CONTRACT,
        "fingerprint": group.fingerprint,
        "occurrences": group.occurrences,
    }))
}

fn finding_group_hash(group: &ComparedFindingGroup) -> Result<String> {
    canonical_hash(&json!({
        "contract": FINDING_GROUP_CONTRACT,
        "fingerprint": group.fingerprint,
        "subjectKind": group.subject_kind,
        "subjectAlgorithm": group.subject_algorithm,
        "severity": group.severity,
        "ruleId": group.rule_id,
        "issueCode": group.issue_code,
        "displayMessage": group.display_message,
        "entityId": group.entity_id,
        "fieldId": group.field_id,
        "relationshipId": group.relationship_id,
        "relationshipRole": group.relationship_role,
        "relationshipCardinality": group.relationship_cardinality,
        "occurrenceCount": group.occurrence_count,
        "occurrenceSetHash": group.occurrence_set_hash,
    }))
}

fn finding_set_hash(set: &ComparedFindingSet) -> Result<String> {
    canonical_hash(&json!({
        "contract": FINDING_SET_HASH_CONTRACT,
        "schemaVersion": set.schema_version,
        "authority": set.authority,
        "authorityHash": set.authority_hash,
        "pseudonymNamespaceId": set.pseudonym_namespace_id,
        "tourId": set.tour_id,
        "validationRunId": set.validation_run_id,
        "validationResultHash": set.validation_result_hash,
        "sourceHash": set.source_hash,
        "manifestHash": set.manifest_hash,
        "findingCount": set.finding_count,
        "issueOccurrenceCount": set.issue_occurrence_count,
        "findings": set.findings,
    }))
}

fn comparison_hash(comparison: &ComparedValidation) -> Result<String> {
    canonical_hash(&json!({
        "contract": COMPARISON_RESULT_HASH_CONTRACT,
        "schemaVersion": comparison.schema_version,
        "authority": comparison.authority,
        "authorityHash": comparison.authority_hash,
        "pseudonymNamespaceId": comparison.pseudonym_namespace_id,
        "prior": comparison.prior,
        "current": comparison.current,
        "attribution": comparison.attribution,
        "inputHash": comparison.input_hash,
        "counts": comparison.counts,
        "findings": comparison.findings,
    }))
}

fn finding_descriptor(group: &ComparedFindingGroup) -> Value {
    json!({
        "subjectKind": group.subject_kind,
        "subjectAlgorithm": group.subject_algorithm,
        "severity": group.severity,
        "ruleId": group.rule_id,
        "issueCode": group.issue_code,
        "displayMessage": group.display_message,
        "entityId": group.entity_id,
        "fieldId": group.field_id,
        "relationshipId": group.relationship_id,
        "relationshipRole": group.relationship_role,
        "relationshipCardinality": group.relationship_cardinality,
    })
}

fn finding_set_reference(set: &ComparedFindingSet) -> ComparedFindingSetReference {
    ComparedFindingSetReference {
        tour_id: set.tour_id.clone(),
        validation_run_id: set.validation_run_id.clone(),
        validation_result_hash: set.validation_result_hash.clone(),
        source_hash: set.source_hash.clone(),
        manifest_hash: set.manifest_hash.clone(),
        finding_set_hash: set.canonical_hash.clone(),
    }
}

fn occurrence_order(
    left: &ComparedFindingOccurrence,
    right: &ComparedFindingOccurrence,
) -> std::cmp::Ordering {
    (
        &left.issue_id,
        &left.issue_hash,
        &left.source_file,
        left.source_ordinal,
        left.line_start,
        left.line_end,
    )
        .cmp(&(
            &right.issue_id,
            &right.issue_hash,
            &right.source_file,
            right.source_ordinal,
            right.line_start,
            right.line_end,
        ))
}

fn ensure_unique_occurrences(occurrences: &[ComparedFindingOccurrence]) -> Result<()> {
    if occurrences
        .windows(2)
        .any(|pair| occurrence_order(&pair[0], &pair[1]).is_eq())
    {
        return integrity("finding group repeats exact issue/row membership evidence");
    }
    Ok(())
}

fn validate_attribution(attribution: &ComparedValidationAttribution) -> Result<()> {
    validate_identifier(&attribution.actor, "comparison actor")?;
    validate_bounded_text(
        &attribution.reason,
        "comparison reason",
        MAXIMUM_REASON_BYTES,
    )
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    validate_bounded_text(value, label, MAXIMUM_IDENTIFIER_BYTES)
}

fn validate_bounded_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return invalid(&format!(
            "{label} must contain 1..={maximum} non-control UTF-8 bytes"
        ));
    }
    Ok(())
}

fn validate_hash(value: &str, label: &str) -> Result<()> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid(&format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

fn checked_count(current: u64, addition: u64, label: &str) -> Result<u64> {
    let next = current
        .checked_add(addition)
        .ok_or_else(|| bound_error(&format!("{label} overflow")))?;
    if next > u64::try_from(MAXIMUM_ISSUES).unwrap_or(u64::MAX) {
        return bound(&format!("{label} exceeds the absolute issue bound"));
    }
    Ok(next)
}

fn hmac_canonical(key: &ComparedValidationNamespaceKey, material: &Value) -> Result<String> {
    Ok(hex_sha256(hmac_sha256(
        key,
        &canonical_json_bytes(material)?,
    )))
}

/// RFC 2104/4231 HMAC using SHA-256's 64-byte compression block.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut normalized = [0_u8; HMAC_BLOCK_BYTES];
    if key.len() > HMAC_BLOCK_BYTES {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; HMAC_BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; HMAC_BLOCK_BYTES];
    for ((inner, outer), key_byte) in inner_pad
        .iter_mut()
        .zip(outer_pad.iter_mut())
        .zip(normalized)
    {
        *inner ^= key_byte;
        *outer ^= key_byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn hex_sha256(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(SHA256_HEX_BYTES);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn invalid<T>(detail: &str) -> Result<T> {
    Err(invalid_error(detail))
}

fn invalid_error(detail: &str) -> KernelError {
    KernelError::ValidationInvalid(format!("compared validation: {detail}"))
}

fn integrity<T>(detail: &str) -> Result<T> {
    Err(KernelError::IntegrityFailed(format!(
        "compared validation: {detail}"
    )))
}

fn conflict<T>(detail: &str) -> Result<T> {
    Err(KernelError::Conflict(format!(
        "compared validation: {detail}"
    )))
}

fn bound<T>(detail: &str) -> Result<T> {
    Err(bound_error(detail))
}

fn bound_error(detail: &str) -> KernelError {
    KernelError::ValidationBound(format!("compared validation: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: ComparedValidationNamespaceKey = [0x2a; 32];
    const OTHER_KEY: ComparedValidationNamespaceKey = [0x19; 32];

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, SHA256_HEX_BYTES).collect()
    }

    fn authority() -> ComparedFindingAuthority {
        ComparedFindingAuthority {
            schema_version: COMPARED_FINDING_AUTHORITY_SCHEMA.to_owned(),
            engagement_id: "engagement-tour-proof".to_owned(),
            profile_id: "synthetic.sales-document.readiness".to_owned(),
            profile_version: "1.0.0".to_owned(),
            profile_hash: hash('a'),
            mapping_id: "synthetic.sales-document.mapping".to_owned(),
            mapping_version: "1.0.0".to_owned(),
            mapping_hash: hash('b'),
            rule_set_hash: hash('c'),
            validation_policy_hash: hash('d'),
            validation_engine_version: "migration-stream-validation/v1".to_owned(),
        }
    }

    fn field(field_id: &str, value: Option<&str>) -> CanonicalFieldValueInput {
        CanonicalFieldValueInput {
            field_id: field_id.to_owned(),
            value: value.map(str::to_owned),
        }
    }

    fn item_record(parent: Option<&str>, item: &str, quantity: &str) -> RecordLocatorInput {
        RecordLocatorInput {
            declared_key: vec![
                field("parentDocumentId", parent),
                field("sourceItemId", Some(item)),
            ],
            projected_row: vec![
                field("parentDocumentId", parent),
                field("productId", Some("SKU-PRIVATE")),
                field("quantity", Some(quantity)),
                field("sourceItemId", Some(item)),
            ],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn issue(
        issue_id: &str,
        issue_hash_byte: char,
        rule_id: &str,
        issue_code: &str,
        field_id: &str,
        ordinal: u64,
        subject: ComparedIssueSubjectInput,
    ) -> ComparedIssueInput {
        ComparedIssueInput {
            issue_id: issue_id.to_owned(),
            issue_hash: hash(issue_hash_byte),
            severity: "ERROR".to_owned(),
            rule_id: rule_id.to_owned(),
            issue_code: issue_code.to_owned(),
            display_message: format!("{rule_id} failed."),
            entity_id: "SalesDocumentItem".to_owned(),
            field_id: field_id.to_owned(),
            source_file: "deal_detail.csv".to_owned(),
            source_ordinal: ordinal,
            line_start: ordinal + 1,
            line_end: ordinal + 1,
            subject,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rule_issue(
        issue_id: &str,
        hash_byte: char,
        rule_id: &str,
        field_id: &str,
        ordinal: u64,
        parent: Option<&str>,
        item: &str,
        quantity: &str,
    ) -> ComparedIssueInput {
        issue(
            issue_id,
            hash_byte,
            rule_id,
            "PATTERN_MISMATCH",
            field_id,
            ordinal,
            ComparedIssueSubjectInput::RuleRecord {
                record: item_record(parent, item, quantity),
            },
        )
    }

    fn unique_issue(
        issue_id: &str,
        hash_byte: char,
        ordinal: u64,
        parent: &str,
        item: &str,
    ) -> ComparedIssueInput {
        issue(
            issue_id,
            hash_byte,
            "SD-ITEM-UNIQUE-KEY",
            "UNIQUE_DUPLICATE",
            "sourceItemId",
            ordinal,
            ComparedIssueSubjectInput::UniqueGroup {
                scope_values: vec![
                    field("parentDocumentId", Some(parent)),
                    field("sourceItemId", Some(item)),
                ],
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn relationship_issue(
        issue_id: &str,
        hash_byte: char,
        ordinal: u64,
        code: &str,
        suffix: &str,
        role: RelationshipRole,
        parent: Option<&str>,
        item: &str,
    ) -> ComparedIssueInput {
        issue(
            issue_id,
            hash_byte,
            &format!("relationship.sales-document.items.{suffix}"),
            code,
            "parentDocumentId",
            ordinal,
            ComparedIssueSubjectInput::RelationshipRecord {
                record: item_record(parent, item, "1"),
                relationship_id: "sales-document.items".to_owned(),
                role,
                cardinality: "1..*".to_owned(),
                key_pairs: vec![RelationshipKeyValueInput {
                    from_field_id: "sourceDocumentId".to_owned(),
                    to_field_id: "parentDocumentId".to_owned(),
                    value: parent.map(str::to_owned),
                }],
            },
        )
    }

    fn set_input(
        tour: &str,
        source_byte: char,
        issues: Vec<ComparedIssueInput>,
    ) -> ComparedFindingSetInput {
        ComparedFindingSetInput {
            schema_version: COMPARED_FINDING_SET_INPUT_SCHEMA.to_owned(),
            authority: authority(),
            tour_id: tour.to_owned(),
            validation_run_id: format!("run-{tour}"),
            validation_result_hash: hash(if tour == "tour-1" { 'e' } else { 'f' }),
            source_hash: hash(source_byte),
            manifest_hash: hash(if tour == "tour-1" { '1' } else { '2' }),
            issues,
        }
    }

    fn comparison_input(
        prior: ComparedFindingSet,
        current: ComparedFindingSet,
    ) -> ComparedValidationInput {
        ComparedValidationInput {
            schema_version: COMPARED_VALIDATION_INPUT_SCHEMA.to_owned(),
            prior,
            current,
            attribution: ComparedValidationAttribution {
                actor: "local-operator".to_owned(),
                reason: "Compare the exact accepted revised source validation.".to_owned(),
            },
        }
    }

    #[test]
    fn hmac_matches_rfc_4231_sha256_case_one() {
        let key = [0x0b_u8; 20];
        let actual = hex_sha256(hmac_sha256(&key, b"Hi There"));
        assert_eq!(
            actual,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn row_reorder_and_valid_insertion_leave_finding_remaining() {
        let prior_issue = rule_issue(
            "issue-prior",
            '3',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            6,
            Some("SD-1001"),
            "0030",
            "0",
        );
        let current_issue = rule_issue(
            "issue-current",
            '4',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            2,
            Some("SD-1001"),
            "0030",
            "0",
        );
        let prior = build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![prior_issue]))
            .expect("prior finding set");
        let current =
            build_compared_finding_set(&KEY, &set_input("tour-2", '6', vec![current_issue]))
                .expect("current finding set after unrelated valid insertion and reorder");
        assert_eq!(
            prior.findings[0].fingerprint,
            current.findings[0].fingerprint
        );
        assert_ne!(prior.findings[0].group_hash, current.findings[0].group_hash);
        let result = compare_finding_sets(&comparison_input(prior, current)).expect("comparison");
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 0,
                remaining: 1,
                new: 0,
            }
        );
    }

    #[test]
    fn unique_members_group_and_multiset_cardinality_deltas_are_honest() {
        let three = vec![
            unique_issue("issue-a", '1', 2, "SD-1002", "0010"),
            unique_issue("issue-b", '2', 5, "SD-1002", "0010"),
            unique_issue("issue-c", '3', 9, "SD-1002", "0010"),
        ];
        let two = vec![
            unique_issue("issue-d", '4', 1, "SD-1002", "0010"),
            unique_issue("issue-e", '5', 7, "SD-1002", "0010"),
        ];
        let prior =
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', three)).expect("three");
        let current =
            build_compared_finding_set(&KEY, &set_input("tour-2", '6', two)).expect("two");
        assert_eq!(prior.findings.len(), 1);
        assert_eq!(prior.findings[0].occurrence_count, 3);
        let reduced = compare_finding_sets(&comparison_input(prior.clone(), current.clone()))
            .expect("3 to 2");
        assert_eq!(
            reduced.counts,
            ComparedValidationCounts {
                closed: 1,
                remaining: 2,
                new: 0,
            }
        );
        let expanded = compare_finding_sets(&comparison_input(current, prior)).expect("2 to 3");
        assert_eq!(
            expanded.counts,
            ComparedValidationCounts {
                closed: 0,
                remaining: 2,
                new: 1,
            }
        );
    }

    #[test]
    fn incomplete_key_uses_conservative_projected_row_fallback() {
        let prior_issue = rule_issue(
            "issue-missing-key-prior",
            '3',
            "SD-ITEM-REQ-PARENT",
            "parentDocumentId",
            3,
            None,
            "0010",
            "1",
        );
        let current_issue = rule_issue(
            "issue-missing-key-current",
            '4',
            "SD-ITEM-REQ-PARENT",
            "parentDocumentId",
            8,
            None,
            "0010",
            "1",
        );
        let prior = build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![prior_issue]))
            .expect("fallback prior");
        let current =
            build_compared_finding_set(&KEY, &set_input("tour-2", '6', vec![current_issue]))
                .expect("fallback current");
        assert_eq!(
            prior.findings[0].subject_algorithm,
            ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1
        );
        assert_eq!(
            prior.findings[0].subject_algorithm.identity_basis(),
            ComparedIdentityBasis::FullRowFallback
        );
        assert_eq!(
            prior.findings[0].fingerprint,
            current.findings[0].fingerprint
        );
    }

    #[test]
    fn empty_declared_keys_use_projected_row_fallback() {
        let rule = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![rule_issue(
                    "issue-empty-rule",
                    '3',
                    "SD-ITEM-REQ-PARENT",
                    "parentDocumentId",
                    3,
                    Some(""),
                    "0010",
                    "1",
                )],
            ),
        )
        .expect("incomplete rule key must use the projected-row fallback");
        assert_eq!(
            rule.findings[0].subject_algorithm,
            ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1
        );
        assert_eq!(
            rule.findings[0].subject_algorithm.identity_basis(),
            ComparedIdentityBasis::FullRowFallback
        );

        let relationship = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![relationship_issue(
                    "issue-empty-relationship",
                    '4',
                    3,
                    "RELATIONSHIP_KEY_MISSING",
                    "parent-key",
                    RelationshipRole::Child,
                    Some(""),
                    "0010",
                )],
            ),
        )
        .expect("incomplete relationship key must use the projected-row fallback");
        assert_eq!(
            relationship.findings[0].subject_algorithm,
            ComparedSubjectAlgorithm::RelationshipProjectedRowHmacSha256V1
        );
        assert_eq!(
            relationship.findings[0].subject_algorithm.identity_basis(),
            ComparedIdentityBasis::FullRowFallback
        );
    }

    #[test]
    fn exact_nonempty_whitespace_keys_and_unique_scopes_remain_comparable() {
        let prior_rule = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![rule_issue(
                    "issue-whitespace-rule-prior",
                    '3',
                    "SD-ITEM-REQ-PARENT",
                    "parentDocumentId",
                    2,
                    Some("   "),
                    "0010",
                    "1",
                )],
            ),
        )
        .expect("exact nonempty whitespace key is a valid declared-key subject");
        assert_eq!(
            prior_rule.findings[0].subject_algorithm,
            ComparedSubjectAlgorithm::EntityKeyHmacSha256V1
        );

        let prior_unique = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![
                    unique_issue("issue-whitespace-a", '1', 2, "   ", "0010"),
                    unique_issue("issue-whitespace-b", '2', 3, "   ", "0010"),
                ],
            ),
        )
        .expect("whitespace duplicate scope remains comparable");
        let current_unique = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-2",
                '6',
                vec![
                    unique_issue("issue-whitespace-c", '3', 7, "   ", "0010"),
                    unique_issue("issue-whitespace-d", '4', 8, "   ", "0010"),
                ],
            ),
        )
        .expect("revised whitespace duplicate scope remains comparable");
        let comparison = compare_finding_sets(&comparison_input(prior_unique, current_unique))
            .expect("whitespace duplicate comparison");
        assert_eq!(
            comparison.counts,
            ComparedValidationCounts {
                closed: 0,
                remaining: 2,
                new: 0,
            }
        );
    }

    #[test]
    fn filling_an_incomplete_key_is_closed_plus_new() {
        let prior = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![rule_issue(
                    "issue-missing-key",
                    '3',
                    "SD-ITEM-REQ-PARENT",
                    "parentDocumentId",
                    1,
                    Some(""),
                    "0030",
                    "1",
                )],
            ),
        )
        .expect("fallback-identified prior finding");
        let current = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-2",
                '6',
                vec![rule_issue(
                    "issue-complete-key",
                    '4',
                    "SD-ITEM-REQ-PARENT",
                    "parentDocumentId",
                    1,
                    Some("SD-1001"),
                    "0030",
                    "1",
                )],
            ),
        )
        .expect("declared-key current finding");
        assert_eq!(
            prior.findings[0].subject_algorithm.identity_basis(),
            ComparedIdentityBasis::FullRowFallback
        );
        assert_eq!(
            current.findings[0].subject_algorithm.identity_basis(),
            ComparedIdentityBasis::DeclaredKey
        );
        let result = compare_finding_sets(&comparison_input(prior, current))
            .expect("incomplete-key repair comparison");
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 1,
                remaining: 0,
                new: 1,
            }
        );
    }

    #[test]
    fn unique_scope_nonempty_and_subject_control_bounds_stay_enforced() {
        let empty_unique = unique_issue("empty-unique", '3', 1, "", "0010");
        assert!(
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![empty_unique]))
                .is_err(),
            "unique scope values remain exactly nonempty"
        );

        let control_value = rule_issue(
            "control-value",
            '4',
            "SD-ITEM-REQ-PARENT",
            "parentDocumentId",
            1,
            Some("\n"),
            "0010",
            "1",
        );
        assert!(
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![control_value]))
                .is_err(),
            "fallback subject values remain bounded to non-control text"
        );
    }

    #[test]
    fn key_edit_is_closed_plus_new() {
        let prior = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![rule_issue(
                    "issue-prior",
                    '3',
                    "SD-ITEM-POSITIVE-QUANTITY",
                    "quantity",
                    1,
                    Some("SD-1001"),
                    "0030",
                    "0",
                )],
            ),
        )
        .expect("prior");
        let current = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-2",
                '6',
                vec![rule_issue(
                    "issue-current",
                    '4',
                    "SD-ITEM-POSITIVE-QUANTITY",
                    "quantity",
                    1,
                    Some("SD-1001"),
                    "0040",
                    "0",
                )],
            ),
        )
        .expect("current");
        let result =
            compare_finding_sets(&comparison_input(prior, current)).expect("key edit comparison");
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 1,
                remaining: 0,
                new: 1,
            }
        );
    }

    #[test]
    fn orphan_and_minimum_child_close_when_relationship_is_repaired() {
        let orphan = relationship_issue(
            "issue-orphan",
            '7',
            5,
            "RELATIONSHIP_PARENT_NOT_FOUND",
            "parent-exists",
            RelationshipRole::Child,
            Some("SD-9999"),
            "0010",
        );
        let minimum = relationship_issue(
            "issue-minimum",
            '8',
            3,
            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
            "minimum-child",
            RelationshipRole::Parent,
            Some("SD-1003"),
            "HEADER",
        );
        let prior =
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![orphan, minimum]))
                .expect("relationship findings");
        let current =
            build_compared_finding_set(&KEY, &set_input("tour-2", '6', vec![])).expect("repaired");
        let result = compare_finding_sets(&comparison_input(prior, current)).expect("comparison");
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 2,
                remaining: 0,
                new: 0,
            }
        );
    }

    #[test]
    fn relationship_roles_never_collide() {
        let parent = relationship_issue(
            "issue-parent",
            '7',
            1,
            "RELATIONSHIP_KEY_MISSING",
            "parent-key",
            RelationshipRole::Parent,
            None,
            "0010",
        );
        let child = relationship_issue(
            "issue-child",
            '8',
            2,
            "RELATIONSHIP_KEY_MISSING",
            "child-key",
            RelationshipRole::Child,
            None,
            "0010",
        );
        let set = build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![parent, child]))
            .expect("role-distinct relationships");
        assert_eq!(set.findings.len(), 2);
        assert_ne!(set.findings[0].fingerprint, set.findings[1].fingerprint);
    }

    #[test]
    fn preferred_fixture_shape_is_four_closed_one_remaining_one_new() {
        let header_duplicate = unique_issue("header-duplicate", '1', 4, "SD-1002", "HEADER");
        let item_duplicate = unique_issue("item-duplicate", '2', 4, "SD-1002", "0010");
        let orphan = relationship_issue(
            "orphan",
            '3',
            5,
            "RELATIONSHIP_PARENT_NOT_FOUND",
            "parent-exists",
            RelationshipRole::Child,
            Some("SD-9999"),
            "0010",
        );
        let minimum = relationship_issue(
            "minimum",
            '4',
            3,
            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
            "minimum-child",
            RelationshipRole::Parent,
            Some("SD-1003"),
            "HEADER",
        );
        let remaining_prior = rule_issue(
            "quantity-prior",
            '7',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            6,
            Some("SD-1001"),
            "0030",
            "0",
        );
        let remaining_current = rule_issue(
            "quantity-current",
            '8',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            2,
            Some("SD-1001"),
            "0030",
            "0",
        );
        let new_date = rule_issue(
            "date-current",
            '9',
            "SD-ITEM-REQUESTED-DATE-SHAPE",
            "requestedDate",
            6,
            Some("SD-1001"),
            "0020",
            "1",
        );
        let prior = build_compared_finding_set(
            &KEY,
            &set_input(
                "tour-1",
                '5',
                vec![
                    header_duplicate,
                    item_duplicate,
                    orphan,
                    minimum,
                    remaining_prior,
                ],
            ),
        )
        .expect("tour 1");
        let current = build_compared_finding_set(
            &KEY,
            &set_input("tour-2", '6', vec![remaining_current, new_date]),
        )
        .expect("tour 2");
        let result =
            compare_finding_sets(&comparison_input(prior, current)).expect("4/1/1 comparison");
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 4,
                remaining: 1,
                new: 1,
            }
        );
    }

    #[test]
    fn empty_finding_sets_compare_to_zero_without_special_cases() {
        let prior =
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![])).expect("prior");
        let current =
            build_compared_finding_set(&KEY, &set_input("tour-2", '6', vec![])).expect("current");
        let result =
            compare_finding_sets(&comparison_input(prior, current)).expect("empty comparison");
        assert_eq!(result.findings, [] as [ComparedFindingDelta; 0]);
        assert_eq!(
            result.counts,
            ComparedValidationCounts {
                closed: 0,
                remaining: 0,
                new: 0,
            }
        );
    }

    #[test]
    fn serialized_outputs_contain_no_raw_keys_rows_or_secret() {
        let raw_parent = "PRIVATE-DOCUMENT-778899";
        let raw_item = "PRIVATE-ITEM-0042";
        let raw_product = "SKU-PRIVATE";
        let issue = rule_issue(
            "private-issue",
            '3',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            1,
            Some(raw_parent),
            raw_item,
            "0",
        );
        let set = build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![issue]))
            .expect("private finding set");
        let serialized = serde_json::to_string(&set).expect("serialize");
        for private in [raw_parent, raw_item, raw_product, &hex_sha256(KEY)] {
            assert!(!serialized.contains(private), "output leaked {private}");
        }
    }

    #[test]
    fn authority_or_namespace_drift_yields_no_comparison() {
        let prior =
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![])).expect("prior");
        let mut drifted_input = set_input("tour-2", '6', vec![]);
        drifted_input.authority.validation_policy_hash = hash('9');
        let drifted =
            build_compared_finding_set(&KEY, &drifted_input).expect("drifted authority set");
        assert!(compare_finding_sets(&comparison_input(prior.clone(), drifted)).is_err());

        let other_namespace =
            build_compared_finding_set(&OTHER_KEY, &set_input("tour-2", '6', vec![]))
                .expect("other namespace");
        assert!(compare_finding_sets(&comparison_input(prior, other_namespace)).is_err());
    }

    #[test]
    fn malformed_and_collision_shaped_inputs_fail_closed() {
        let mut malformed = rule_issue(
            "malformed",
            '3',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            1,
            Some("SD-1001"),
            "0030",
            "0",
        );
        malformed.issue_hash = "ABC".to_owned();
        assert!(
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![malformed])).is_err()
        );

        let issue = rule_issue(
            "valid",
            '3',
            "SD-ITEM-POSITIVE-QUANTITY",
            "quantity",
            1,
            Some("SD-1001"),
            "0030",
            "0",
        );
        let mut set =
            build_compared_finding_set(&KEY, &set_input("tour-1", '5', vec![issue])).expect("set");
        let mut collision = set.findings[0].clone();
        collision.rule_id = "DIFFERENT-RULE".to_owned();
        collision.group_hash = finding_group_hash(&collision).expect("collision group hash");
        set.findings.push(collision);
        set.finding_count = 2;
        set.canonical_hash = finding_set_hash(&set).expect("collision set hash");
        assert!(validate_finding_set(&set).is_err());
    }

    #[test]
    fn replay_rejects_membership_count_algorithm_link_and_secret_tampering() {
        let prior_input = set_input(
            "tour-1",
            '5',
            vec![rule_issue(
                "issue-prior",
                '3',
                "SD-ITEM-POSITIVE-QUANTITY",
                "quantity",
                6,
                Some("SD-1001"),
                "0030",
                "0",
            )],
        );
        let current_input = set_input(
            "tour-2",
            '6',
            vec![rule_issue(
                "issue-current",
                '4',
                "SD-ITEM-POSITIVE-QUANTITY",
                "quantity",
                2,
                Some("SD-1001"),
                "0030",
                "0",
            )],
        );
        let prior = build_compared_finding_set(&KEY, &prior_input).expect("prior");
        let current = build_compared_finding_set(&KEY, &current_input).expect("current");

        let mut membership_tamper = prior.clone();
        membership_tamper.findings[0].occurrences[0].issue_id = "tampered".to_owned();
        assert!(
            verify_compared_finding_set_replay(&KEY, &prior_input, &membership_tamper).is_err()
        );

        let mut count_tamper = prior.clone();
        count_tamper.findings[0].occurrence_count = 2;
        assert!(verify_compared_finding_set_replay(&KEY, &prior_input, &count_tamper).is_err());

        let mut algorithm_tamper = prior.clone();
        algorithm_tamper.findings[0].subject_algorithm =
            ComparedSubjectAlgorithm::ProjectedRowHmacSha256V1;
        assert!(verify_compared_finding_set_replay(&KEY, &prior_input, &algorithm_tamper).is_err());
        assert!(verify_compared_finding_set_replay(&OTHER_KEY, &prior_input, &prior).is_err());

        let comparison_input = comparison_input(prior, current);
        let comparison = compare_finding_sets(&comparison_input).expect("comparison");
        let mut link_tamper = comparison.clone();
        link_tamper.findings[0].prior_group_hash = Some(hash('9'));
        assert!(verify_compared_validation_replay(&comparison_input, &link_tamper).is_err());
    }
}

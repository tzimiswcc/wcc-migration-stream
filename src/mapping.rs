//! Declarative, bounded source-to-canonical mapping.
//!
//! Mapping reads only reconciled `source_records` belonging to a completed
//! intake job. It never opens an archive, live partial, or quarantine artifact.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, Metadata},
    io::Read,
    path::Path,
};

use jsonschema::PatternOptions;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json, sha256_hex},
    profile::assert_runtime_authority,
    profile_registry::{
        inspect_profile_connection, load_approved_profile, load_verified_profile_document,
    },
    state::{
        DATABASE_FILENAME, SCHEMA_VERSION, WorkspaceWriter, ensure_package_cleanup_not_conflicted,
        now_rfc3339, open_read_only, sqlite_integer,
    },
};

/// Version of the persisted mapping/read contract.
pub const MAPPING_SCHEMA_VERSION: u32 = 1;

const MAXIMUM_MAPPING_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_MAPPING_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_BATCH_ROWS: usize = 10_000;
const MAXIMUM_MAPPING_HEADER_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_MAPPING_COVERAGE_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_MAPPING_COVERAGE_ITEMS: usize = 32_768;
const MAXIMUM_MAPPING_METADATA_BYTES: usize = 64 * 1024;
const MAXIMUM_ATTEMPT_ERRORS_BYTES: usize = 16 * 1024;
const MAXIMUM_ATTEMPT_METADATA_BYTES: usize = 16 * 1024;
const MAXIMUM_GOVERNED_SOURCE_COLUMNS: usize = 4_096;
const MAXIMUM_GOVERNED_HEADER_CHARS: usize = 1_024;
const MAXIMUM_GOVERNED_SOURCE_VALUE_CHARS: usize = 1024 * 1024;
const MAXIMUM_PROJECTION_BATCH_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_PROJECTION_BATCH_RETAINED_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_PROJECTED_ROW_VALUE_BYTES: usize = 4 * 1024 * 1024;
const MAPPING_ATTEMPTS_UPDATE_TRIGGER: &str = "CREATE TRIGGER mapping_validation_attempts_no_update BEFORE UPDATE ON mapping_validation_attempts BEGIN SELECT RAISE(ABORT, 'mapping validation attempt history is immutable'); END";
const MAPPING_ATTEMPTS_DELETE_TRIGGER: &str = "CREATE TRIGGER mapping_validation_attempts_no_delete BEFORE DELETE ON mapping_validation_attempts BEGIN SELECT RAISE(ABORT, 'mapping validation attempt history is immutable'); END";
const MAPPING_VERSIONS_UPDATE_TRIGGER: &str = "CREATE TRIGGER mapping_versions_no_update BEFORE UPDATE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";
const MAPPING_VERSIONS_DELETE_TRIGGER: &str = "CREATE TRIGGER mapping_versions_no_delete BEFORE DELETE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";

/// Exact profile identity bound into a mapping version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingProfileIdentity {
    pub profile_id: String,
    pub version: String,
    pub sha256: String,
}

/// Exact accepted-source identity bound into a mapping version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingSourceIdentity {
    pub package_sha256: String,
    pub source_files: Vec<String>,
}

/// Allow-listed mapping operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingKind {
    Direct,
    Constant,
    EmptyAsNull,
    Trim,
    UppercaseAscii,
    LowercaseAscii,
    Lookup,
}

impl MappingKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Constant => "constant",
            Self::EmptyAsNull => "empty_as_null",
            Self::Trim => "trim",
            Self::UppercaseAscii => "uppercase_ascii",
            Self::LowercaseAscii => "lowercase_ascii",
            Self::Lookup => "lookup",
        }
    }
}

/// One deterministic source/default binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingBinding {
    pub binding_id: String,
    /// Explicit canonical entity scope. Its absence is supported only by the
    /// legacy root-only mapping contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_column_ordinal: Option<usize>,
    pub target_field_id: String,
    pub kind: MappingKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup_table_id: Option<String>,
}

/// Bounded versioned local lookup table embedded in immutable mapping bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReferenceTable {
    pub table_id: String,
    pub version: String,
    /// Explicit canonical entity scope. Its absence resolves to the root only
    /// for a wholly legacy mapping document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub values: BTreeMap<String, String>,
}

/// One deterministic accepted-file to canonical-entity assignment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceEntityAssignment {
    pub source_file: String,
    pub entity_id: String,
}

/// One exact canonical field pair forming a declared relationship key.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipFieldPair {
    pub from_field_id: String,
    pub to_field_id: String,
}

/// Operator-confirmed relationship semantics copied from the canonical
/// profile. Mapping validation rejects any divergence from profile authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingRelationshipBinding {
    pub relationship_id: String,
    pub from_entity_id: String,
    pub to_entity_id: String,
    pub cardinality: String,
    pub key_pairs: Vec<RelationshipFieldPair>,
}

/// Source Mapping Version document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingDocument {
    pub schema_version: u32,
    pub mapping_id: String,
    pub version: String,
    pub profile: MappingProfileIdentity,
    pub source: MappingSourceIdentity,
    pub root_entity_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_entity_assignments: Vec<SourceEntityAssignment>,
    pub bindings: Vec<MappingBinding>,
    pub reference_tables: Vec<ReferenceTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationship_bindings: Vec<MappingRelationshipBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
}

impl MappingDocument {
    /// Whether this document opts into explicit entity-scoped mapping.
    ///
    /// A document is either wholly explicit or wholly legacy. Semantic
    /// validation rejects partial scope, so callers may safely use this only
    /// after loading verified mapping evidence.
    pub(crate) fn has_explicit_entity_scope(&self) -> bool {
        !self.source_entity_assignments.is_empty()
            || !self.relationship_bindings.is_empty()
            || self
                .bindings
                .iter()
                .any(|binding| binding.target_entity_id.is_some())
            || self
                .reference_tables
                .iter()
                .any(|table| table.entity_id.is_some())
    }

    pub(crate) fn effective_binding_entity<'a>(&'a self, binding: &'a MappingBinding) -> &'a str {
        binding
            .target_entity_id
            .as_deref()
            .unwrap_or(&self.root_entity_id)
    }

    pub(crate) fn effective_table_entity<'a>(&'a self, table: &'a ReferenceTable) -> &'a str {
        table.entity_id.as_deref().unwrap_or(&self.root_entity_id)
    }

    pub(crate) fn entity_for_source_file<'a>(&'a self, source_file: &str) -> Result<&'a str> {
        if !self.has_explicit_entity_scope() {
            return Ok(&self.root_entity_id);
        }
        let mut matches = self
            .source_entity_assignments
            .iter()
            .filter(|assignment| assignment.source_file == source_file);
        let assignment = matches.next().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "source file {source_file:?} has no explicit canonical entity assignment"
            ))
        })?;
        if matches.next().is_some() {
            return Err(KernelError::MappingInvalid(format!(
                "source file {source_file:?} has ambiguous canonical entity assignments"
            )));
        }
        Ok(&assignment.entity_id)
    }
}

/// Canonical field identity. Persisted legacy coverage used an unqualified
/// string; new explicit mappings use the collision-safe entity/field tuple.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum CanonicalFieldIdentity {
    Legacy(String),
    Scoped {
        #[serde(rename = "entityId")]
        entity_id: String,
        #[serde(rename = "fieldId")]
        field_id: String,
    },
}

/// Required-coverage identity. The scoped form avoids delimiter-based parsing.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum RequiredCanonicalFieldIdentity {
    Legacy(String),
    Scoped {
        #[serde(rename = "sourceFile")]
        source_file: String,
        #[serde(rename = "entityId")]
        entity_id: String,
        #[serde(rename = "fieldId")]
        field_id: String,
    },
}

/// Coverage facts persisted with a valid mapping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingCoverage {
    pub schema_version: u32,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub profile: MappingProfileIdentity,
    pub source: MappingSourceIdentity,
    pub validated_job_id: String,
    pub mapped_canonical_fields: Vec<CanonicalFieldIdentity>,
    pub defaulted_canonical_fields: Vec<CanonicalFieldIdentity>,
    pub unmapped_source_columns: Vec<SourceColumnIdentity>,
    pub required_canonical_fields_without_coverage: Vec<RequiredCanonicalFieldIdentity>,
    pub invalid_bindings: Vec<String>,
}

/// Stable accepted source-column identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceColumnIdentity {
    pub source_file: String,
    pub source_column: String,
    pub source_column_ordinal: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
}

/// Result of a successful mapping validation/registration action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingValidationReport {
    pub schema_version: u32,
    pub attempt_id: String,
    pub attempt_hash: String,
    pub state: String,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub coverage: MappingCoverage,
    pub reused_immutable_version: bool,
}

/// Fully checked mapping evidence awaiting one caller-owned registration
/// transaction.
///
/// Engagement submission uses this split phase so the immutable mapping rows
/// and the engagement authority transition can commit atomically while one
/// workspace writer lock remains held.
pub(crate) struct PreparedMappingValidation {
    report: MappingValidationReport,
    input_hash: String,
    profile: MappingProfileIdentity,
    source_hash: String,
    validated_job_id: String,
    document_json: String,
    coverage_json: String,
    coverage_hash: String,
    created_at: String,
}

impl PreparedMappingValidation {
    pub(crate) fn report(&self) -> &MappingValidationReport {
        &self.report
    }

    pub(crate) fn into_report(self) -> MappingValidationReport {
        self.report
    }
}

/// Inspectable immutable mapping facts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingView {
    pub schema_version: u32,
    pub mapping_id: String,
    pub mapping_version: String,
    pub mapping_hash: String,
    pub profile: MappingProfileIdentity,
    pub source: MappingSourceIdentity,
    pub validated_job_id: String,
    pub coverage: MappingCoverage,
    pub created_at: String,
}

/// Canonical row projection used by validation and exact-row reads.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectedRow {
    pub entity_id: String,
    pub source_file: String,
    pub source_ordinal: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub source_values: Vec<String>,
    pub canonical_values: BTreeMap<String, Option<String>>,
    pub lineage: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredMapping {
    pub document: MappingDocument,
    pub mapping_hash: String,
}

#[derive(Debug)]
struct MappingInspectionRow {
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    source_hash: String,
    validated_job_id: String,
    attempt_id: String,
    document_json: String,
    coverage_json: String,
    coverage_hash: String,
    created_at: String,
}

#[derive(Debug)]
struct MappingAttemptAnchor {
    input_hash: String,
    state: String,
    mapping_id: Option<String>,
    mapping_version: Option<String>,
    mapping_hash: Option<String>,
    errors_json: String,
    created_at: String,
    attempt_hash: String,
}

#[derive(Debug)]
struct MappingAttemptIntegrityRow {
    attempt_id: String,
    input_hash: String,
    state: String,
    mapping_id: Option<String>,
    mapping_version: Option<String>,
    mapping_hash: Option<String>,
    errors_json: String,
    created_at: String,
    attempt_hash: String,
}

/// Validate and immutably register one mapping against an accepted job and an
/// exact locally approved profile.
///
/// # Errors
///
/// Invalid schema, authority, source, coverage, operation, or immutable
/// identity collisions are rejected after an immutable invalid-attempt record
/// is written where possible.
#[allow(clippy::too_many_lines)]
pub fn validate_mapping(
    workspace: &Path,
    job_id: &str,
    mapping_schema_path: &Path,
    profile_schema_path: &Path,
    mapping_path: &Path,
) -> Result<MappingValidationReport> {
    let writer = WorkspaceWriter::open(workspace)?;
    let prepared = prepare_mapping_validation_locked(
        &writer,
        job_id,
        mapping_schema_path,
        profile_schema_path,
        mapping_path,
    )?;
    let transaction = writer.connection.unchecked_transaction()?;
    register_prepared_mapping_validation(&transaction, &prepared)?;
    transaction.commit()?;
    Ok(prepared.into_report())
}

/// Validate one mapping while reusing an already-held workspace writer lock.
///
/// Successful evidence is returned without registration so a caller can add
/// the mapping rows to a wider atomic transaction. Invalid attempts retain the
/// public validator's existing immutable-record behavior.
#[allow(clippy::too_many_lines)]
pub(crate) fn prepare_mapping_validation_locked(
    writer: &WorkspaceWriter,
    job_id: &str,
    mapping_schema_path: &Path,
    profile_schema_path: &Path,
    mapping_path: &Path,
) -> Result<PreparedMappingValidation> {
    verify_mapping_immutability_guards(&writer.connection)?;
    let raw = read_regular_bounded_mapping_input(
        mapping_path,
        MAXIMUM_MAPPING_BYTES,
        "mapping document",
    )?;
    let input_hash = sha256_hex(&raw);
    let attempt_id = Uuid::new_v4().to_string();
    let document_value = match parse_unique_json(&raw) {
        Ok(value) => value,
        Err(error) => {
            let detail = error.to_string();
            record_invalid_attempt(&writer.connection, &attempt_id, &input_hash, None, &detail)?;
            return Err(KernelError::MappingInvalid(detail));
        }
    };
    if let Err(detail) = validate_mapping_schema(mapping_schema_path, &document_value) {
        let identity = mapping_identity(&document_value);
        record_invalid_attempt(
            &writer.connection,
            &attempt_id,
            &input_hash,
            identity.as_ref(),
            &detail,
        )?;
        return Err(KernelError::MappingInvalid(detail));
    }
    let document: MappingDocument = serde_json::from_value(document_value.clone())
        .map_err(|error| KernelError::MappingInvalid(error.to_string()))?;
    let mapping_hash = canonical_hash(&document_value)?;

    let validation = validate_mapping_semantics(
        writer,
        job_id,
        profile_schema_path,
        &document,
        &mapping_hash,
    );
    let coverage = match validation {
        Ok(coverage) => coverage,
        Err(error) => {
            record_invalid_attempt(
                &writer.connection,
                &attempt_id,
                &input_hash,
                Some(&(
                    document.mapping_id.as_str(),
                    document.version.as_str(),
                    Some(mapping_hash.as_str()),
                )),
                &error.to_string(),
            )?;
            return Err(error);
        }
    };

    let existing_hash: Option<String> = writer
        .connection
        .query_row(
            "SELECT mapping_hash FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![document.mapping_id, document.version],
            |row| row.get(0),
        )
        .optional()?;
    if existing_hash
        .as_deref()
        .is_some_and(|existing| existing != mapping_hash)
    {
        let detail = format!(
            "mapping {} version {} is already bound to different canonical bytes",
            document.mapping_id, document.version
        );
        record_invalid_attempt(
            &writer.connection,
            &attempt_id,
            &input_hash,
            Some(&(
                document.mapping_id.as_str(),
                document.version.as_str(),
                Some(mapping_hash.as_str()),
            )),
            &detail,
        )?;
        return Err(KernelError::MappingConflict(detail));
    }

    let document_json = canonical_json_string(&document_value)?;
    let coverage_value = serde_json::to_value(&coverage)?;
    let coverage_json = canonical_json_string(&coverage_value)?;
    let coverage_hash = canonical_hash(&coverage_value)?;
    let now = now_rfc3339();
    let attempt_errors = json!([]);
    let attempt_hash = mapping_attempt_hash(
        &attempt_id,
        &input_hash,
        "VALID",
        Some(&document.mapping_id),
        Some(&document.version),
        Some(&mapping_hash),
        &attempt_errors,
        &now,
    )?;

    let reused_immutable_version = existing_hash.is_some();
    Ok(PreparedMappingValidation {
        report: MappingValidationReport {
            schema_version: MAPPING_SCHEMA_VERSION,
            attempt_id,
            attempt_hash,
            state: "VALID".to_owned(),
            mapping_id: document.mapping_id,
            mapping_version: document.version,
            mapping_hash,
            coverage,
            reused_immutable_version,
        },
        input_hash,
        profile: document.profile,
        source_hash: document.source.package_sha256,
        validated_job_id: job_id.to_owned(),
        document_json,
        coverage_json,
        coverage_hash,
        created_at: now,
    })
}

/// Register prepared valid mapping evidence inside a caller-owned transaction.
pub(crate) fn register_prepared_mapping_validation(
    transaction: &rusqlite::Transaction<'_>,
    prepared: &PreparedMappingValidation,
) -> Result<()> {
    verify_mapping_immutability_guards(transaction)?;
    let report = &prepared.report;
    transaction.execute(
        "INSERT INTO mapping_validation_attempts (
             id, input_hash, state, mapping_id, mapping_version, mapping_hash,
             errors_json, created_at, attempt_hash
         ) VALUES (?1, ?2, 'VALID', ?3, ?4, ?5, '[]', ?6, ?7)",
        params![
            report.attempt_id,
            prepared.input_hash,
            report.mapping_id,
            report.mapping_version,
            report.mapping_hash,
            prepared.created_at,
            report.attempt_hash,
        ],
    )?;
    if !report.reused_immutable_version {
        transaction.execute(
            "INSERT INTO mapping_versions (
                 mapping_id, mapping_version, mapping_hash, attempt_id,
                 profile_id, profile_version, profile_hash, source_hash,
                 validated_job_id, document_json, coverage_json, coverage_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                report.mapping_id,
                report.mapping_version,
                report.mapping_hash,
                report.attempt_id,
                prepared.profile.profile_id,
                prepared.profile.version,
                prepared.profile.sha256,
                prepared.source_hash,
                prepared.validated_job_id,
                prepared.document_json,
                prepared.coverage_json,
                prepared.coverage_hash,
                prepared.created_at,
            ],
        )?;
    }
    Ok(())
}

/// Inspect one immutable mapping version.
///
/// # Errors
///
/// Returns a typed mapping error when the identity is unknown or evidence is
/// malformed.
pub fn inspect_mapping(workspace: &Path, mapping_id: &str, version: &str) -> Result<MappingView> {
    let database_path = workspace.join(DATABASE_FILENAME);
    if !database_path.exists() {
        return Err(KernelError::NotFound(format!(
            "workspace mapping database {}",
            database_path.display()
        )));
    }
    let connection = open_read_only(&database_path)?;
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != SCHEMA_VERSION {
        return Err(KernelError::Conflict(format!(
            "mapping inspection requires schema version {SCHEMA_VERSION}; found {schema_version}"
        )));
    }
    let transaction = connection.unchecked_transaction()?;
    let row = load_mapping_inspection_row(&transaction, mapping_id, version)?;
    let (document, coverage) = verify_mapping_inspection_evidence(&row, mapping_id, version)?;
    verify_mapping_inspection_provenance(&transaction, &row, &document, &coverage)?;
    let view = MappingView {
        schema_version: MAPPING_SCHEMA_VERSION,
        mapping_id: mapping_id.to_owned(),
        mapping_version: version.to_owned(),
        mapping_hash: row.mapping_hash,
        profile: document.profile,
        source: document.source,
        validated_job_id: row.validated_job_id,
        coverage,
        created_at: row.created_at,
    };
    transaction.commit()?;
    Ok(view)
}

fn load_mapping_inspection_row(
    connection: &Connection,
    mapping_id: &str,
    version: &str,
) -> Result<MappingInspectionRow> {
    verify_mapping_immutability_guards(connection)?;
    let sizes: Option<(i64, i64, i64)> = connection
        .query_row(
            "SELECT length(CAST(document_json AS BLOB)),
                    length(CAST(coverage_json AS BLOB)),
                    length(CAST(mapping_id AS BLOB))
                  + length(CAST(mapping_version AS BLOB))
                  + length(CAST(mapping_hash AS BLOB))
                  + length(CAST(attempt_id AS BLOB))
                  + length(CAST(profile_id AS BLOB))
                  + length(CAST(profile_version AS BLOB))
                  + length(CAST(profile_hash AS BLOB))
                  + length(CAST(source_hash AS BLOB))
                  + length(CAST(validated_job_id AS BLOB))
                  + length(CAST(coverage_hash AS BLOB))
                  + length(CAST(created_at AS BLOB))
             FROM mapping_versions WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![mapping_id, version],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((document_bytes, coverage_bytes, metadata_bytes)) = sizes {
        let document_bytes = usize::try_from(document_bytes).unwrap_or(usize::MAX);
        let coverage_bytes = usize::try_from(coverage_bytes).unwrap_or(usize::MAX);
        let metadata_bytes = usize::try_from(metadata_bytes).unwrap_or(usize::MAX);
        if document_bytes > MAXIMUM_MAPPING_BYTES
            || coverage_bytes > MAXIMUM_MAPPING_COVERAGE_BYTES
            || metadata_bytes > MAXIMUM_MAPPING_METADATA_BYTES
        {
            return Err(mapping_inspection_integrity(
                mapping_id,
                version,
                format!(
                    "stored document/coverage bytes {document_bytes}/{coverage_bytes} or metadata bytes {metadata_bytes} exceed bounds {MAXIMUM_MAPPING_BYTES}/{MAXIMUM_MAPPING_COVERAGE_BYTES}/{MAXIMUM_MAPPING_METADATA_BYTES}"
                ),
            ));
        }
    }
    connection
        .query_row(
            "SELECT mapping_id, mapping_version, mapping_hash,
                    profile_id, profile_version, profile_hash, source_hash,
                    validated_job_id, attempt_id, document_json, coverage_json, coverage_hash,
                    created_at
             FROM mapping_versions WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![mapping_id, version],
            |row| {
                Ok(MappingInspectionRow {
                    mapping_id: row.get(0)?,
                    mapping_version: row.get(1)?,
                    mapping_hash: row.get(2)?,
                    profile_id: row.get(3)?,
                    profile_version: row.get(4)?,
                    profile_hash: row.get(5)?,
                    source_hash: row.get(6)?,
                    validated_job_id: row.get(7)?,
                    attempt_id: row.get(8)?,
                    document_json: row.get(9)?,
                    coverage_json: row.get(10)?,
                    coverage_hash: row.get(11)?,
                    created_at: row.get(12)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::MappingInvalid(format!("unknown mapping {mapping_id} version {version}"))
        })
}

fn verify_mapping_inspection_evidence(
    row: &MappingInspectionRow,
    mapping_id: &str,
    version: &str,
) -> Result<(MappingDocument, MappingCoverage)> {
    let document_value = parse_unique_json(row.document_json.as_bytes()).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("document JSON is invalid: {error}"),
        )
    })?;
    let document_hash = canonical_hash(&document_value).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("document hash replay failed: {error}"),
        )
    })?;
    if document_hash != row.mapping_hash {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "document canonical hash mismatch",
        ));
    }
    let document: MappingDocument = serde_json::from_value(document_value).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("document contract is invalid: {error}"),
        )
    })?;
    if row.mapping_id != mapping_id
        || row.mapping_version != version
        || document.schema_version != MAPPING_SCHEMA_VERSION
        || document.mapping_id != mapping_id
        || document.version != version
        || document.profile.profile_id != row.profile_id
        || document.profile.version != row.profile_version
        || document.profile.sha256 != row.profile_hash
        || document.source.package_sha256 != row.source_hash
    {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "document identities differ from authoritative mapping columns",
        ));
    }
    reject_mapping_inspection_tombstones(mapping_id, version, &document)?;

    let coverage_value = parse_unique_json(row.coverage_json.as_bytes()).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("coverage JSON is invalid: {error}"),
        )
    })?;
    let coverage_hash = canonical_hash(&coverage_value).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("coverage hash replay failed: {error}"),
        )
    })?;
    if coverage_hash != row.coverage_hash {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "coverage canonical hash mismatch",
        ));
    }
    let coverage: MappingCoverage = serde_json::from_value(coverage_value).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("coverage contract is invalid: {error}"),
        )
    })?;
    if coverage.schema_version != MAPPING_SCHEMA_VERSION
        || coverage.mapping_id != mapping_id
        || coverage.mapping_version != version
        || coverage.mapping_hash != row.mapping_hash
        || coverage.profile != document.profile
        || coverage.source != document.source
        || coverage.validated_job_id != row.validated_job_id
    {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "coverage identities differ from its exact mapping and registration job",
        ));
    }
    let coverage_files = coverage
        .unmapped_source_columns
        .iter()
        .map(|column| column.source_file.clone())
        .collect::<Vec<_>>();
    reject_quarantine_names(&coverage_files).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("coverage names a quarantined source: {error}"),
        )
    })?;
    Ok((document, coverage))
}

fn reject_mapping_inspection_tombstones(
    mapping_id: &str,
    version: &str,
    document: &MappingDocument,
) -> Result<()> {
    let mut source_files = document.source.source_files.clone();
    source_files.extend(
        document
            .bindings
            .iter()
            .filter_map(|binding| binding.source_file.clone()),
    );
    source_files.extend(
        document
            .source_entity_assignments
            .iter()
            .map(|assignment| assignment.source_file.clone()),
    );
    reject_quarantine_names(&source_files).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("document names a quarantined source: {error}"),
        )
    })
}

fn verify_mapping_inspection_provenance(
    connection: &Connection,
    row: &MappingInspectionRow,
    document: &MappingDocument,
    coverage: &MappingCoverage,
) -> Result<()> {
    let mapping_id = row.mapping_id.as_str();
    let version = row.mapping_version.as_str();
    inspect_profile_connection(
        connection,
        &row.profile_id,
        &row.profile_version,
        &row.profile_hash,
    )
    .map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("profile provenance replay failed: {error}"),
        )
    })?;
    replay_mapping_coverage(connection, row, document, coverage)?;
    verify_valid_mapping_attempt(
        connection,
        &row.attempt_id,
        &row.mapping_id,
        &row.mapping_version,
        &row.mapping_hash,
        &row.created_at,
    )?;
    let accepted: Option<(String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT state, source_hash, stored_archive FROM jobs WHERE id = ?1",
            [&row.validated_job_id],
            |job| Ok((job.get(0)?, job.get(1)?, job.get(2)?)),
        )
        .optional()?;
    let Some((state, source_hash, stored_archive)) = accepted else {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "mapping registration job is absent",
        ));
    };
    if state != "COMPLETE" || source_hash.as_deref() != Some(row.source_hash.as_str()) {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "mapping registration job is not COMPLETE with the same accepted source hash",
        ));
    }
    let stored_archive = stored_archive.ok_or_else(|| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            "complete mapping registration job has no accepted archive path",
        )
    })?;
    reject_quarantine_names(std::slice::from_ref(&stored_archive)).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("accepted archive is quarantined: {error}"),
        )
    })?;
    let expected_archive = format!("packages/{}.zip", row.source_hash);
    if stored_archive != expected_archive {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            format!("accepted archive path {stored_archive:?} differs from {expected_archive:?}"),
        ));
    }
    if document.source.source_files.is_empty() {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "mapping document has no accepted source files",
        ));
    }
    for source_file in &document.source.source_files {
        let file_counts: Option<(Option<i64>, i64, i64)> = connection
            .query_row(
                "SELECT length(CAST(header_json AS BLOB)), source_records, stored_records
                 FROM csv_files WHERE job_id = ?1 AND path = ?2",
                params![row.validated_job_id, source_file],
                |file| Ok((file.get(0)?, file.get(1)?, file.get(2)?)),
            )
            .optional()?;
        if !file_counts.is_some_and(|(header_bytes, source_records, stored_records)| {
            header_bytes.is_some_and(|bytes| {
                usize::try_from(bytes).unwrap_or(usize::MAX) <= MAXIMUM_MAPPING_HEADER_BYTES
            }) && source_records == stored_records
        }) {
            return Err(mapping_inspection_integrity(
                mapping_id,
                version,
                format!("source file {source_file:?} is not an exact reconciled accepted CSV"),
            ));
        }
    }
    Ok(())
}

fn replay_mapping_coverage(
    connection: &Connection,
    row: &MappingInspectionRow,
    document: &MappingDocument,
    coverage: &MappingCoverage,
) -> Result<()> {
    let mapping_id = row.mapping_id.as_str();
    let version = row.mapping_version.as_str();
    let profile = load_verified_profile_document(
        connection,
        &row.profile_id,
        &row.profile_version,
        &row.profile_hash,
    )
    .map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("exact profile document cannot be replayed for coverage: {error}"),
        )
    })?;
    let profile_scope = profile_mapping_scope(&profile).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("exact profile cannot be interpreted for coverage replay: {error}"),
        )
    })?;
    if profile_scope.root_entity_id != document.root_entity_id {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "mapping root entity differs from its exact profile",
        ));
    }
    let replayed = build_mapping_coverage(
        connection,
        &row.validated_job_id,
        document,
        &row.mapping_hash,
        &profile_scope,
    )
    .map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            version,
            format!("coverage replay failed: {error}"),
        )
    })?;
    if replayed != *coverage {
        return Err(mapping_inspection_integrity(
            mapping_id,
            version,
            "coverage differs from deterministic profile/source replay",
        ));
    }
    Ok(())
}

pub(crate) fn verify_valid_mapping_attempt(
    connection: &Connection,
    attempt_id: &str,
    mapping_id: &str,
    mapping_version: &str,
    mapping_hash: &str,
    mapping_created_at: &str,
) -> Result<()> {
    let sizes: Option<(i64, i64)> = connection
        .query_row(
            "SELECT length(CAST(errors_json AS BLOB)),
                    length(CAST(id AS BLOB))
                  + length(CAST(input_hash AS BLOB))
                  + length(CAST(state AS BLOB))
                  + COALESCE(length(CAST(mapping_id AS BLOB)), 0)
                  + COALESCE(length(CAST(mapping_version AS BLOB)), 0)
                  + COALESCE(length(CAST(mapping_hash AS BLOB)), 0)
                  + length(CAST(created_at AS BLOB))
                  + length(CAST(attempt_hash AS BLOB))
             FROM mapping_validation_attempts WHERE id = ?1",
            [attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((errors_bytes, metadata_bytes)) = sizes else {
        return Err(mapping_inspection_integrity(
            mapping_id,
            mapping_version,
            "immutable registration attempt is absent",
        ));
    };
    if usize::try_from(errors_bytes).unwrap_or(usize::MAX) > MAXIMUM_ATTEMPT_ERRORS_BYTES
        || usize::try_from(metadata_bytes).unwrap_or(usize::MAX) > MAXIMUM_ATTEMPT_METADATA_BYTES
    {
        return Err(mapping_inspection_integrity(
            mapping_id,
            mapping_version,
            "immutable registration attempt exceeds evidence bounds",
        ));
    }
    let attempt: Option<MappingAttemptAnchor> = connection
        .query_row(
            "SELECT input_hash, state, mapping_id, mapping_version, mapping_hash,
                    errors_json, created_at, attempt_hash
             FROM mapping_validation_attempts WHERE id = ?1",
            [attempt_id],
            |attempt| {
                Ok(MappingAttemptAnchor {
                    input_hash: attempt.get(0)?,
                    state: attempt.get(1)?,
                    mapping_id: attempt.get(2)?,
                    mapping_version: attempt.get(3)?,
                    mapping_hash: attempt.get(4)?,
                    errors_json: attempt.get(5)?,
                    created_at: attempt.get(6)?,
                    attempt_hash: attempt.get(7)?,
                })
            },
        )
        .optional()?;
    let Some(attempt) = attempt else {
        return Err(mapping_inspection_integrity(
            mapping_id,
            mapping_version,
            "immutable registration attempt disappeared during replay",
        ));
    };
    let errors_value = parse_unique_json(attempt.errors_json.as_bytes()).map_err(|error| {
        mapping_inspection_integrity(
            mapping_id,
            mapping_version,
            format!("immutable registration-attempt errors are invalid: {error}"),
        )
    })?;
    let expected_attempt_hash = mapping_attempt_hash(
        attempt_id,
        &attempt.input_hash,
        &attempt.state,
        attempt.mapping_id.as_deref(),
        attempt.mapping_version.as_deref(),
        attempt.mapping_hash.as_deref(),
        &errors_value,
        &attempt.created_at,
    )?;
    if attempt.input_hash.len() != 64
        || !attempt
            .input_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || attempt.state != "VALID"
        || attempt.mapping_id.as_deref() != Some(mapping_id)
        || attempt.mapping_version.as_deref() != Some(mapping_version)
        || attempt.mapping_hash.as_deref() != Some(mapping_hash)
        || attempt.errors_json != "[]"
        || attempt.created_at != mapping_created_at
        || attempt.attempt_hash != expected_attempt_hash
    {
        return Err(mapping_inspection_integrity(
            mapping_id,
            mapping_version,
            "immutable VALID registration attempt does not bind the mapping version",
        ));
    }
    Ok(())
}

fn mapping_inspection_integrity(
    mapping_id: &str,
    version: &str,
    detail: impl std::fmt::Display,
) -> KernelError {
    KernelError::IntegrityFailed(format!(
        "mapping {mapping_id} version {version} inspection failed: {detail}"
    ))
}

fn validate_mapping_schema(
    schema_path: &Path,
    document: &Value,
) -> std::result::Result<(), String> {
    let schema_bytes = read_regular_bounded_mapping_input(
        schema_path,
        MAXIMUM_MAPPING_SCHEMA_BYTES,
        "mapping schema",
    )
    .map_err(|error| {
        format!(
            "mapping schema {} cannot be read safely: {error}",
            schema_path.display()
        )
    })?;
    let schema = parse_unique_json(&schema_bytes).map_err(|error| error.to_string())?;
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| format!("mapping schema is invalid: {error}"))?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::regex())
        .build(&schema)
        .map_err(|error| format!("mapping schema is invalid: {error}"))?;
    let mut errors = validator
        .iter_errors(document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    errors.sort_unstable();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn read_regular_bounded_mapping_input(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let named_before = require_regular_bounded_mapping_input(path, maximum, label)?;
    let mut file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let opened_before = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    require_opened_regular_bounded_mapping_input(path, &opened_before, maximum, label)?;
    verify_mapping_input_identity(path, &file, &named_before, &opened_before, label)?;

    let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
    let initial_capacity = usize::try_from(opened_before.len())
        .unwrap_or(maximum)
        .min(maximum);
    let mut bytes = Vec::with_capacity(initial_capacity);
    (&mut file)
        .take(maximum_u64.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() > maximum {
        return Err(mapping_input_error(format!(
            "{label} bytes {} exceed limit {maximum}: {}",
            bytes.len(),
            path.display()
        )));
    }

    let opened_after = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    require_opened_regular_bounded_mapping_input(path, &opened_after, maximum, label)?;
    if opened_after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err(mapping_input_error(format!(
            "{label} changed size while it was read: {}",
            path.display()
        )));
    }
    let named_after = require_regular_bounded_mapping_input(path, maximum, label)?;
    verify_mapping_input_identity(path, &file, &named_after, &opened_after, label)?;
    Ok(bytes)
}

fn require_regular_bounded_mapping_input(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(mapping_input_error(format!(
            "{label} must be a regular non-link file: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    if crate::durability::is_reparse_point(&metadata) {
        return Err(mapping_input_error(format!(
            "{label} must not be a Windows reparse point: {}",
            path.display()
        )));
    }
    require_mapping_input_size(path, metadata.len(), maximum, label)?;
    Ok(metadata)
}

fn require_opened_regular_bounded_mapping_input(
    path: &Path,
    metadata: &Metadata,
    maximum: usize,
    label: &str,
) -> Result<()> {
    if !metadata.file_type().is_file() {
        return Err(mapping_input_error(format!(
            "opened {label} must be a regular file: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    if crate::durability::is_reparse_point(metadata) {
        return Err(mapping_input_error(format!(
            "opened {label} must not be a Windows reparse point: {}",
            path.display()
        )));
    }
    require_mapping_input_size(path, metadata.len(), maximum, label)
}

fn require_mapping_input_size(path: &Path, size: u64, maximum: usize, label: &str) -> Result<()> {
    let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
    if size > maximum_u64 {
        return Err(mapping_input_error(format!(
            "{label} bytes {size} exceed limit {maximum}: {}",
            path.display()
        )));
    }
    Ok(())
}

fn verify_mapping_input_identity(
    path: &Path,
    file: &File,
    named: &Metadata,
    opened: &Metadata,
    label: &str,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(mapping_input_error(format!(
                "{label} path no longer names the verified file: {}",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))?
    {
        return Err(mapping_input_error(format!(
            "{label} path no longer names the verified file: {}",
            path.display()
        )));
    }
    #[cfg(not(any(unix, windows)))]
    if named.len() != opened.len() {
        return Err(mapping_input_error(format!(
            "{label} path changed while it was read: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    let _ = (named, opened);
    #[cfg(unix)]
    let _ = file;
    Ok(())
}

fn mapping_input_error(detail: String) -> KernelError {
    KernelError::MappingInvalid(detail)
}

fn mapping_identity(value: &Value) -> Option<(&str, &str, Option<&str>)> {
    Some((
        value.get("mappingId")?.as_str()?,
        value.get("version")?.as_str()?,
        None,
    ))
}

fn record_invalid_attempt(
    connection: &Connection,
    attempt_id: &str,
    input_hash: &str,
    identity: Option<&(&str, &str, Option<&str>)>,
    detail: &str,
) -> Result<()> {
    let recorded_detail = if detail.chars().count() > 4_096 {
        format!(
            "{} [diagnostic truncated at immutable-attempt bound]",
            detail.chars().take(4_000).collect::<String>()
        )
    } else {
        detail.to_owned()
    };
    let errors = json!([{ "code": "MAPPING_INVALID", "detail": recorded_detail }]);
    let errors_json = canonical_json_string(&errors)?;
    let (mapping_id, mapping_version, mapping_hash) = identity
        .map_or((None, None, None), |value| {
            (Some(value.0), Some(value.1), value.2)
        });
    let created_at = now_rfc3339();
    let attempt_hash = mapping_attempt_hash(
        attempt_id,
        input_hash,
        "INVALID",
        mapping_id,
        mapping_version,
        mapping_hash,
        &errors,
        &created_at,
    )?;
    let transaction = connection.unchecked_transaction()?;
    verify_mapping_immutability_guards(&transaction)?;
    transaction.execute(
        "INSERT INTO mapping_validation_attempts (
             id, input_hash, state, mapping_id, mapping_version, mapping_hash,
             errors_json, created_at, attempt_hash
         ) VALUES (?1, ?2, 'INVALID', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            attempt_id,
            input_hash,
            mapping_id,
            mapping_version,
            mapping_hash,
            errors_json,
            created_at,
            attempt_hash,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mapping_attempt_hash(
    attempt_id: &str,
    input_hash: &str,
    state: &str,
    mapping_id: Option<&str>,
    mapping_version: Option<&str>,
    mapping_hash: Option<&str>,
    errors: &Value,
    created_at: &str,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": "migration-stream-mapping-validation-attempt/v1",
        "attemptId": attempt_id,
        "inputHash": input_hash,
        "state": state,
        "mappingId": mapping_id,
        "mappingVersion": mapping_version,
        "mappingHash": mapping_hash,
        "errors": errors,
        "createdAt": created_at,
    }))
}

#[allow(clippy::too_many_lines)]
fn validate_mapping_semantics(
    writer: &WorkspaceWriter,
    job_id: &str,
    profile_schema_path: &Path,
    document: &MappingDocument,
    mapping_hash: &str,
) -> Result<MappingCoverage> {
    let connection = &writer.connection;
    if document.schema_version != MAPPING_SCHEMA_VERSION {
        return Err(KernelError::MappingInvalid(format!(
            "mapping schemaVersion must be {MAPPING_SCHEMA_VERSION}"
        )));
    }
    reject_quarantine_names(&document.source.source_files)?;
    let (job_state, source_hash): (String, Option<String>) = connection
        .query_row(
            "SELECT state, source_hash FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| KernelError::MappingInvalid(format!("unknown intake job {job_id}")))?;
    if job_state != "COMPLETE" {
        return Err(KernelError::MappingInvalid(format!(
            "mapping requires a COMPLETE intake job; {job_id} is {job_state}"
        )));
    }
    if source_hash.as_deref() != Some(document.source.package_sha256.as_str()) {
        return Err(KernelError::MappingInvalid(
            "mapping package identity does not match the accepted job source hash".to_owned(),
        ));
    }
    ensure_package_cleanup_not_conflicted(connection, job_id)?;

    let approved = load_approved_profile(
        writer,
        profile_schema_path,
        &document.profile.profile_id,
        &document.profile.version,
        &document.profile.sha256,
    )?;
    assert_runtime_authority(&approved.profile);
    if !approved.view.can_start_new_runs {
        return Err(KernelError::ProfileNotApproved(
            "approved profile registry view blocks new mapping runs".to_owned(),
        ));
    }
    let profile_document = approved.profile.document().clone();
    let profile_scope = profile_mapping_scope(&profile_document)?;
    if document.root_entity_id != profile_scope.root_entity_id {
        return Err(KernelError::MappingInvalid(format!(
            "mapping root entity {:?} does not match approved profile root {:?}",
            document.root_entity_id, profile_scope.root_entity_id
        )));
    }

    build_mapping_coverage(connection, job_id, document, mapping_hash, &profile_scope)
}

#[allow(clippy::too_many_lines)]
fn build_mapping_coverage(
    connection: &Connection,
    job_id: &str,
    document: &MappingDocument,
    mapping_hash: &str,
    profile: &ProfileMappingScope,
) -> Result<MappingCoverage> {
    let accepted_headers = accepted_headers(connection, job_id, &document.source.source_files)?;
    let accepted_column_count = accepted_headers
        .values()
        .try_fold(0_usize, |total, headers| {
            total.checked_add(headers.len()).ok_or_else(|| {
                KernelError::MappingInvalid("mapping coverage item overflow".to_owned())
            })
        })?;
    let maximum_coverage_items = accepted_column_count
        .checked_add(document.bindings.len())
        .ok_or_else(|| KernelError::MappingInvalid("mapping coverage item overflow".to_owned()))?;
    if maximum_coverage_items > MAXIMUM_MAPPING_COVERAGE_ITEMS {
        return Err(KernelError::MappingInvalid(format!(
            "mapping coverage requires at most {maximum_coverage_items} items; limit is {MAXIMUM_MAPPING_COVERAGE_ITEMS}"
        )));
    }
    let explicit_scope = validate_entity_scope(document, profile)?;
    let mut binding_ids = HashSet::new();
    let mut target_scopes = HashMap::<(&str, &str), HashSet<Option<&str>>>::new();
    let mut constant_targets = HashSet::<(&str, &str)>::new();
    let mut file_targets = HashMap::<&str, HashSet<(&str, &str)>>::new();
    let mut mapped_source = BTreeSet::new();
    let mut mapped_targets = BTreeSet::new();
    let mut defaulted_targets = BTreeSet::new();
    let tables = validate_reference_tables(document, profile, explicit_scope)?;
    for binding in &document.bindings {
        if !binding_ids.insert(binding.binding_id.as_str()) {
            return Err(KernelError::MappingInvalid(format!(
                "duplicate bindingId {:?}",
                binding.binding_id
            )));
        }
        if explicit_scope && binding.target_entity_id.is_none() {
            return Err(KernelError::MappingInvalid(format!(
                "binding {:?} has no explicit targetEntityId",
                binding.binding_id
            )));
        }
        let target_entity = document.effective_binding_entity(binding);
        let target = binding.target_field_id.as_str();
        let source_scope = binding.source_file.as_deref();
        let scopes = target_scopes.entry((target_entity, target)).or_default();
        let conflicts = if source_scope.is_none() {
            !scopes.is_empty()
        } else {
            scopes.contains(&None) || scopes.contains(&source_scope)
        };
        if conflicts {
            return Err(KernelError::MappingInvalid(format!(
                "conflicting bindings target canonical field {target_entity}.{target} in the same source scope"
            )));
        }
        scopes.insert(source_scope);
        let Some(fields) = profile.entities.get(target_entity) else {
            return Err(KernelError::MappingInvalid(format!(
                "binding {:?} targets unknown canonical entity {target_entity:?}",
                binding.binding_id
            )));
        };
        let Some(target_required) = fields.get(binding.target_field_id.as_str()) else {
            return Err(KernelError::MappingInvalid(format!(
                "binding {:?} targets unknown canonical field {}.{}",
                binding.binding_id, target_entity, binding.target_field_id
            )));
        };
        validate_binding(
            document,
            binding,
            *target_required,
            target_entity,
            &accepted_headers,
            &tables,
        )?;
        mapped_targets.insert(canonical_field_identity(
            explicit_scope,
            target_entity,
            target,
        ));
        if binding.kind == MappingKind::Constant {
            constant_targets.insert((target_entity, target));
            defaulted_targets.insert(canonical_field_identity(
                explicit_scope,
                target_entity,
                target,
            ));
        } else {
            let source_file = binding.source_file.as_deref().ok_or_else(|| {
                KernelError::MappingInvalid(format!(
                    "binding {:?} lost its source-file scope",
                    binding.binding_id
                ))
            })?;
            file_targets
                .entry(source_file)
                .or_default()
                .insert((target_entity, target));
            mapped_source.insert(SourceColumnIdentity {
                source_file: binding.source_file.clone().unwrap_or_default(),
                source_column: binding.source_column.clone().unwrap_or_default(),
                source_column_ordinal: binding.source_column_ordinal.unwrap_or_default(),
                entity_id: explicit_scope.then(|| target_entity.to_owned()),
            });
        }
    }
    validate_relationship_bindings(document, profile, explicit_scope)?;

    let mut required_without_coverage = Vec::new();
    for source_file in &document.source.source_files {
        let entity_id = document.entity_for_source_file(source_file)?;
        let fields = profile.entities.get(entity_id).ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "source file {source_file:?} resolves to unknown entity {entity_id:?}"
            ))
        })?;
        let scoped_targets = file_targets
            .get(source_file.as_str())
            .cloned()
            .unwrap_or_default();
        for (field, required) in fields {
            if *required
                && !constant_targets.contains(&(entity_id, field.as_str()))
                && !scoped_targets.contains(&(entity_id, field.as_str()))
            {
                required_without_coverage.push(required_field_identity(
                    explicit_scope,
                    source_file,
                    entity_id,
                    field,
                ));
            }
        }
    }
    required_without_coverage.sort_unstable();
    if !required_without_coverage.is_empty() {
        let detail = required_without_coverage
            .iter()
            .map(required_identity_label)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(KernelError::MappingInvalid(format!(
            "required canonical fields have no source/default coverage: {detail}"
        )));
    }

    let mut unmapped_source_columns = Vec::new();
    for (file, headers) in &accepted_headers {
        let entity_id = document.entity_for_source_file(file)?;
        for (ordinal, header) in headers.iter().enumerate() {
            let identity = SourceColumnIdentity {
                source_file: file.clone(),
                source_column: header.clone(),
                source_column_ordinal: ordinal,
                entity_id: explicit_scope.then(|| entity_id.to_owned()),
            };
            if !mapped_source.contains(&identity) {
                unmapped_source_columns.push(identity);
            }
        }
    }
    unmapped_source_columns.sort();

    let coverage = MappingCoverage {
        schema_version: MAPPING_SCHEMA_VERSION,
        mapping_id: document.mapping_id.clone(),
        mapping_version: document.version.clone(),
        mapping_hash: mapping_hash.to_owned(),
        profile: document.profile.clone(),
        source: document.source.clone(),
        validated_job_id: job_id.to_owned(),
        mapped_canonical_fields: mapped_targets.into_iter().collect(),
        defaulted_canonical_fields: defaulted_targets.into_iter().collect(),
        unmapped_source_columns,
        required_canonical_fields_without_coverage: Vec::new(),
        invalid_bindings: Vec::new(),
    };
    let coverage_bytes = canonical_json_string(&serde_json::to_value(&coverage)?)?.len();
    if coverage_bytes > MAXIMUM_MAPPING_COVERAGE_BYTES {
        return Err(KernelError::MappingInvalid(format!(
            "mapping coverage bytes {coverage_bytes} exceed limit {MAXIMUM_MAPPING_COVERAGE_BYTES}"
        )));
    }
    Ok(coverage)
}

fn reject_quarantine_names(files: &[String]) -> Result<()> {
    for file in files {
        let path = Path::new(file);
        if crate::durability::path_uses_tombstone_namespace(path)
            || path
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("part")
                        || extension.eq_ignore_ascii_case("tombstone")
                })
        {
            return Err(KernelError::MappingInvalid(format!(
                "quarantine/live-partial path {file:?} cannot be a mapping source"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ProfileMappingScope {
    root_entity_id: String,
    entities: BTreeMap<String, BTreeMap<String, bool>>,
    relationships: BTreeMap<String, MappingRelationshipBinding>,
}

#[allow(clippy::too_many_lines)]
fn profile_mapping_scope(document: &Value) -> Result<ProfileMappingScope> {
    let entities = document
        .get("entities")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::MappingInvalid("approved profile has no entities".to_owned())
        })?;
    let mut root_entity_id = None;
    let mut scoped_entities = BTreeMap::new();
    for entity in entities {
        let entity_id = entity
            .get("entityId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                KernelError::MappingInvalid("profile entity has no identity".to_owned())
            })?;
        if entity.get("root").and_then(Value::as_bool) == Some(true)
            && root_entity_id.replace(entity_id.to_owned()).is_some()
        {
            return Err(KernelError::MappingInvalid(
                "approved profile has multiple root entities".to_owned(),
            ));
        }
        let fields = entity
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                KernelError::MappingInvalid(format!("profile entity {entity_id:?} has no fields"))
            })?;
        let mut scoped_fields = BTreeMap::new();
        for field in fields {
            let field_id = field
                .get("fieldId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    KernelError::MappingInvalid("profile field has no identity".to_owned())
                })?;
            let required = field
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    KernelError::MappingInvalid(format!(
                        "profile field {entity_id}.{field_id} has no required flag"
                    ))
                })?;
            if scoped_fields
                .insert(field_id.to_owned(), required)
                .is_some()
            {
                return Err(KernelError::MappingInvalid(format!(
                    "profile entity {entity_id:?} has duplicate field {field_id:?}"
                )));
            }
        }
        if scoped_entities
            .insert(entity_id.to_owned(), scoped_fields)
            .is_some()
        {
            return Err(KernelError::MappingInvalid(format!(
                "profile has duplicate entity {entity_id:?}"
            )));
        }
    }
    let root_entity_id = root_entity_id.ok_or_else(|| {
        KernelError::MappingInvalid("approved profile has no root entity".to_owned())
    })?;

    let relationship_values = document
        .get("relationships")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KernelError::MappingInvalid("approved profile has no relationships array".to_owned())
        })?;
    let mut relationships = BTreeMap::new();
    for relationship in relationship_values {
        let relationship_id = relationship.get("relationshipId").and_then(Value::as_str);
        let key_pairs = relationship.get("keyPairs").and_then(Value::as_array);
        let (Some(relationship_id), Some(key_pairs)) = (relationship_id, key_pairs) else {
            if relationship_id.is_some() || key_pairs.is_some() {
                return Err(KernelError::MappingInvalid(
                    "profile relationshipId and keyPairs must be declared together".to_owned(),
                ));
            }
            continue;
        };
        if relationship_id.is_empty() || key_pairs.is_empty() || key_pairs.len() > 64 {
            return Err(KernelError::MappingInvalid(
                "profile keyed relationship identity/pairs exceed supported bounds".to_owned(),
            ));
        }
        let from_entity_id = relationship
            .get("from")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::MappingInvalid("profile relationship has no source entity".to_owned())
            })?;
        let to_entity_id = relationship
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::MappingInvalid("profile relationship has no target entity".to_owned())
            })?;
        let cardinality = relationship
            .get("cardinality")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KernelError::MappingInvalid("profile relationship has no cardinality".to_owned())
            })?;
        let from_fields = scoped_entities.get(from_entity_id).ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "profile relationship {relationship_id:?} has unknown source entity"
            ))
        })?;
        let to_fields = scoped_entities.get(to_entity_id).ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "profile relationship {relationship_id:?} has unknown target entity"
            ))
        })?;
        let mut parsed_pairs = Vec::with_capacity(key_pairs.len());
        let mut unique_pairs = BTreeSet::new();
        for pair in key_pairs {
            let from_field_id =
                pair.get("fromFieldId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        KernelError::MappingInvalid(format!(
                            "profile relationship {relationship_id:?} has an incomplete key pair"
                        ))
                    })?;
            let to_field_id = pair
                .get("toFieldId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    KernelError::MappingInvalid(format!(
                        "profile relationship {relationship_id:?} has an incomplete key pair"
                    ))
                })?;
            if from_fields.get(from_field_id) != Some(&true)
                || to_fields.get(to_field_id) != Some(&true)
            {
                return Err(KernelError::MappingInvalid(format!(
                    "profile relationship {relationship_id:?} key fields must exist and be required"
                )));
            }
            let parsed = RelationshipFieldPair {
                from_field_id: from_field_id.to_owned(),
                to_field_id: to_field_id.to_owned(),
            };
            if !unique_pairs.insert(parsed.clone()) {
                return Err(KernelError::MappingInvalid(format!(
                    "profile relationship {relationship_id:?} repeats a key pair"
                )));
            }
            parsed_pairs.push(parsed);
        }
        let definition = MappingRelationshipBinding {
            relationship_id: relationship_id.to_owned(),
            from_entity_id: from_entity_id.to_owned(),
            to_entity_id: to_entity_id.to_owned(),
            cardinality: cardinality.to_owned(),
            key_pairs: parsed_pairs,
        };
        if relationships
            .insert(relationship_id.to_owned(), definition)
            .is_some()
        {
            return Err(KernelError::MappingInvalid(format!(
                "profile repeats relationshipId {relationship_id:?}"
            )));
        }
    }
    Ok(ProfileMappingScope {
        root_entity_id,
        entities: scoped_entities,
        relationships,
    })
}

fn validate_entity_scope(
    document: &MappingDocument,
    profile: &ProfileMappingScope,
) -> Result<bool> {
    let explicit = document.has_explicit_entity_scope();
    let source_files = document
        .source
        .source_files
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if source_files.len() != document.source.source_files.len() {
        return Err(KernelError::MappingInvalid(
            "mapping sourceFiles contains duplicates".to_owned(),
        ));
    }
    if !explicit {
        return Ok(false);
    }
    if document.source_entity_assignments.len() != source_files.len() {
        return Err(KernelError::MappingInvalid(
            "explicit mapping requires exactly one entity assignment per selected source file"
                .to_owned(),
        ));
    }
    let mut assigned_files = BTreeSet::new();
    let mut assigned_entities = BTreeSet::new();
    for assignment in &document.source_entity_assignments {
        if !source_files.contains(assignment.source_file.as_str())
            || !assigned_files.insert(assignment.source_file.as_str())
            || !profile.entities.contains_key(&assignment.entity_id)
        {
            return Err(KernelError::MappingInvalid(format!(
                "source/entity assignment {:?} -> {:?} is missing, ambiguous, or outside mapping authority",
                assignment.source_file, assignment.entity_id
            )));
        }
        assigned_entities.insert(assignment.entity_id.as_str());
    }
    for binding in &document.bindings {
        let entity_id = binding.target_entity_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "binding {:?} omits targetEntityId in explicit mapping mode",
                binding.binding_id
            ))
        })?;
        if !assigned_entities.contains(entity_id) {
            return Err(KernelError::MappingInvalid(format!(
                "binding {:?} targets entity {entity_id:?} with no source assignment",
                binding.binding_id
            )));
        }
    }
    for table in &document.reference_tables {
        let entity_id = table.entity_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "reference table {:?} omits entityId in explicit mapping mode",
                table.table_id
            ))
        })?;
        if !assigned_entities.contains(entity_id) {
            return Err(KernelError::MappingInvalid(format!(
                "reference table {:?} targets entity {entity_id:?} with no source assignment",
                table.table_id
            )));
        }
    }
    Ok(true)
}

fn validate_relationship_bindings(
    document: &MappingDocument,
    profile: &ProfileMappingScope,
    explicit: bool,
) -> Result<()> {
    if !explicit {
        return Ok(());
    }
    let assigned_entities = document
        .source_entity_assignments
        .iter()
        .map(|assignment| assignment.entity_id.as_str())
        .collect::<BTreeSet<_>>();
    let expected = profile
        .relationships
        .values()
        .filter(|relationship| {
            assigned_entities.contains(relationship.from_entity_id.as_str())
                && assigned_entities.contains(relationship.to_entity_id.as_str())
        })
        .map(|relationship| (relationship.relationship_id.as_str(), relationship))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::new();
    for relationship in &document.relationship_bindings {
        if actual
            .insert(relationship.relationship_id.as_str(), relationship)
            .is_some()
        {
            return Err(KernelError::MappingInvalid(format!(
                "mapping repeats relationship binding {:?}",
                relationship.relationship_id
            )));
        }
    }
    if actual.len() != expected.len() {
        return Err(KernelError::MappingInvalid(
            "explicit mapping must confirm every keyed profile relationship between assigned entities"
                .to_owned(),
        ));
    }
    for (relationship_id, expected_relationship) in expected {
        let actual_relationship = actual.get(relationship_id).ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "mapping did not confirm profile relationship {relationship_id:?}"
            ))
        })?;
        if *actual_relationship != expected_relationship {
            return Err(KernelError::MappingInvalid(format!(
                "mapping relationship {relationship_id:?} differs from canonical profile authority"
            )));
        }
        for pair in &expected_relationship.key_pairs {
            for (entity_id, field_id) in [
                (
                    expected_relationship.from_entity_id.as_str(),
                    pair.from_field_id.as_str(),
                ),
                (
                    expected_relationship.to_entity_id.as_str(),
                    pair.to_field_id.as_str(),
                ),
            ] {
                let matching = document.bindings.iter().filter(|binding| {
                    document.effective_binding_entity(binding) == entity_id
                        && binding.target_field_id == field_id
                });
                let mut found = false;
                for binding in matching {
                    found = true;
                    if !matches!(
                        binding.kind,
                        MappingKind::Direct
                            | MappingKind::Trim
                            | MappingKind::UppercaseAscii
                            | MappingKind::LowercaseAscii
                    ) {
                        return Err(KernelError::MappingInvalid(format!(
                            "relationship key binding {:?} uses unsafe operation {}",
                            binding.binding_id,
                            binding.kind.as_str()
                        )));
                    }
                }
                if !found {
                    return Err(KernelError::MappingInvalid(format!(
                        "relationship {relationship_id:?} key field {entity_id}.{field_id} has no explicit binding"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn canonical_field_identity(
    explicit: bool,
    entity_id: &str,
    field_id: &str,
) -> CanonicalFieldIdentity {
    if explicit {
        CanonicalFieldIdentity::Scoped {
            entity_id: entity_id.to_owned(),
            field_id: field_id.to_owned(),
        }
    } else {
        CanonicalFieldIdentity::Legacy(field_id.to_owned())
    }
}

fn required_field_identity(
    explicit: bool,
    source_file: &str,
    entity_id: &str,
    field_id: &str,
) -> RequiredCanonicalFieldIdentity {
    if explicit {
        RequiredCanonicalFieldIdentity::Scoped {
            source_file: source_file.to_owned(),
            entity_id: entity_id.to_owned(),
            field_id: field_id.to_owned(),
        }
    } else {
        RequiredCanonicalFieldIdentity::Legacy(format!("{source_file}:{field_id}"))
    }
}

fn required_identity_label(identity: &RequiredCanonicalFieldIdentity) -> String {
    match identity {
        RequiredCanonicalFieldIdentity::Legacy(value) => value.clone(),
        RequiredCanonicalFieldIdentity::Scoped {
            source_file,
            entity_id,
            field_id,
        } => format!("{source_file}:{entity_id}.{field_id}"),
    }
}

fn accepted_headers(
    connection: &Connection,
    job_id: &str,
    selected_files: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    if selected_files.is_empty() || selected_files.len() > 32 {
        return Err(KernelError::MappingInvalid(
            "mapping sourceFiles must contain 1..=32 accepted files".to_owned(),
        ));
    }
    let mut accepted = BTreeMap::new();
    let mut total_header_bytes = 0_usize;
    let mut total_coverage_items = 0_usize;
    for file in selected_files {
        if file.is_empty() || file.chars().count() > 1_024 {
            return Err(KernelError::MappingInvalid(
                "mapping source file exceeds the governed path bound".to_owned(),
            ));
        }
        let header_bytes: Option<Option<i64>> = connection
            .query_row(
                "SELECT length(CAST(header_json AS BLOB))
                 FROM csv_files WHERE job_id = ?1 AND path = ?2",
                params![job_id, file],
                |row| row.get(0),
            )
            .optional()?;
        let header_bytes = header_bytes.flatten().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "source file {file:?} is not an accepted CSV for job {job_id}"
            ))
        })?;
        let header_bytes = usize::try_from(header_bytes).unwrap_or(usize::MAX);
        total_header_bytes = total_header_bytes
            .checked_add(header_bytes)
            .ok_or_else(|| {
                KernelError::MappingInvalid("mapping header bytes overflow".to_owned())
            })?;
        if total_header_bytes > MAXIMUM_MAPPING_HEADER_BYTES {
            return Err(KernelError::MappingInvalid(format!(
                "selected source header bytes {total_header_bytes} exceed limit {MAXIMUM_MAPPING_HEADER_BYTES}"
            )));
        }
        let header_json: Option<Option<String>> = connection
            .query_row(
                "SELECT header_json FROM csv_files WHERE job_id = ?1 AND path = ?2",
                params![job_id, file],
                |row| row.get(0),
            )
            .optional()?;
        let header_json = header_json.flatten().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "source file {file:?} is not an accepted CSV for job {job_id}"
            ))
        })?;
        let headers: Vec<String> = serde_json::from_str(&header_json)?;
        if headers.len() > MAXIMUM_GOVERNED_SOURCE_COLUMNS {
            return Err(KernelError::MappingInvalid(format!(
                "source file {file:?} has {} columns; governed mapping limit is {MAXIMUM_GOVERNED_SOURCE_COLUMNS}",
                headers.len()
            )));
        }
        if headers.iter().any(|header| {
            header.is_empty() || header.chars().count() > MAXIMUM_GOVERNED_HEADER_CHARS
        }) {
            return Err(KernelError::MappingInvalid(format!(
                "source file {file:?} has a blank or overlong governed header"
            )));
        }
        total_coverage_items =
            total_coverage_items
                .checked_add(headers.len())
                .ok_or_else(|| {
                    KernelError::MappingInvalid("mapping coverage item overflow".to_owned())
                })?;
        if total_coverage_items > MAXIMUM_MAPPING_COVERAGE_ITEMS {
            return Err(KernelError::MappingInvalid(format!(
                "selected source columns {total_coverage_items} exceed mapping coverage limit {MAXIMUM_MAPPING_COVERAGE_ITEMS}"
            )));
        }
        accepted.insert(file.clone(), headers);
    }
    Ok(accepted)
}

fn validate_reference_tables<'a>(
    document: &'a MappingDocument,
    profile: &ProfileMappingScope,
    explicit: bool,
) -> Result<HashMap<(&'a str, &'a str), &'a ReferenceTable>> {
    let mut result = HashMap::new();
    for table in &document.reference_tables {
        if explicit && table.entity_id.is_none() {
            return Err(KernelError::MappingInvalid(format!(
                "reference table {:?} has no explicit entityId",
                table.table_id
            )));
        }
        let entity_id = document.effective_table_entity(table);
        if !profile.entities.contains_key(entity_id) {
            return Err(KernelError::MappingInvalid(format!(
                "reference table {:?} targets unknown entity {entity_id:?}",
                table.table_id
            )));
        }
        if result
            .insert((entity_id, table.table_id.as_str()), table)
            .is_some()
        {
            return Err(KernelError::MappingInvalid(format!(
                "duplicate reference table {entity_id}.{}",
                table.table_id,
            )));
        }
        if table.values.is_empty() {
            return Err(KernelError::MappingInvalid(format!(
                "reference table {:?} must not be empty",
                table.table_id
            )));
        }
    }
    Ok(result)
}

fn validate_binding(
    document: &MappingDocument,
    binding: &MappingBinding,
    target_required: bool,
    target_entity: &str,
    headers: &BTreeMap<String, Vec<String>>,
    tables: &HashMap<(&str, &str), &ReferenceTable>,
) -> Result<()> {
    if binding.kind == MappingKind::Constant {
        let Some(constant) = binding.constant.as_deref() else {
            return Err(KernelError::MappingInvalid(format!(
                "constant binding {:?} has no constant value",
                binding.binding_id
            )));
        };
        if binding.source_file.is_some()
            || binding.source_column.is_some()
            || binding.source_column_ordinal.is_some()
            || binding.lookup_table_id.is_some()
        {
            return Err(KernelError::MappingInvalid(format!(
                "constant binding {:?} has inconsistent parameters",
                binding.binding_id
            )));
        }
        if target_required && constant.is_empty() {
            return Err(KernelError::MappingInvalid(format!(
                "constant binding {:?} cannot use an empty default for required field {:?}",
                binding.binding_id, binding.target_field_id
            )));
        }
        return Ok(());
    }
    let file = binding.source_file.as_ref().ok_or_else(|| {
        KernelError::MappingInvalid(format!(
            "binding {:?} has no sourceFile",
            binding.binding_id
        ))
    })?;
    let column = binding.source_column.as_ref().ok_or_else(|| {
        KernelError::MappingInvalid(format!(
            "binding {:?} has no sourceColumn",
            binding.binding_id
        ))
    })?;
    let ordinal = binding.source_column_ordinal.ok_or_else(|| {
        KernelError::MappingInvalid(format!(
            "binding {:?} has no sourceColumnOrdinal",
            binding.binding_id
        ))
    })?;
    let accepted = headers.get(file).ok_or_else(|| {
        KernelError::MappingInvalid(format!(
            "binding {:?} references unselected source file {file:?}",
            binding.binding_id
        ))
    })?;
    let source_entity = document.entity_for_source_file(file)?;
    if source_entity != target_entity {
        return Err(KernelError::MappingInvalid(format!(
            "binding {:?} routes source file {file:?} to {source_entity:?} but targets {target_entity:?}",
            binding.binding_id
        )));
    }
    if accepted.get(ordinal) != Some(column) {
        return Err(KernelError::MappingInvalid(format!(
            "binding {:?} source column identity {file:?}/{column:?}/{ordinal} is not accepted",
            binding.binding_id
        )));
    }
    if binding.constant.is_some() {
        return Err(KernelError::MappingInvalid(format!(
            "non-constant binding {:?} includes a constant",
            binding.binding_id
        )));
    }
    if binding.kind == MappingKind::Lookup {
        let table_id = binding.lookup_table_id.as_deref().ok_or_else(|| {
            KernelError::MappingInvalid(format!(
                "lookup binding {:?} has no lookupTableId",
                binding.binding_id
            ))
        })?;
        let Some(table) = tables.get(&(target_entity, table_id)) else {
            return Err(KernelError::MappingInvalid(format!(
                "lookup binding {:?} references unknown table {target_entity}.{table_id}",
                binding.binding_id,
            )));
        };
        if target_required && table.values.values().any(String::is_empty) {
            return Err(KernelError::MappingInvalid(format!(
                "lookup binding {:?} cannot produce an empty value for required field {:?}",
                binding.binding_id, binding.target_field_id
            )));
        }
    } else if binding.lookup_table_id.is_some() {
        return Err(KernelError::MappingInvalid(format!(
            "non-lookup binding {:?} includes lookupTableId",
            binding.binding_id
        )));
    }
    Ok(())
}

pub(crate) fn load_stored_mapping(
    connection: &Connection,
    mapping_id: &str,
    version: &str,
) -> Result<StoredMapping> {
    let row = load_mapping_inspection_row(connection, mapping_id, version)?;
    let (document, coverage) = verify_mapping_inspection_evidence(&row, mapping_id, version)?;
    verify_mapping_inspection_provenance(connection, &row, &document, &coverage)?;
    Ok(StoredMapping {
        document,
        mapping_hash: row.mapping_hash,
    })
}

#[allow(clippy::too_many_lines)] // One bounded SQL preflight and projection pass stay adjacent.
pub(crate) fn project_rows_batch(
    connection: &Connection,
    job_id: &str,
    mapping: &MappingDocument,
    after_file: &str,
    after_ordinal: u64,
    limit: usize,
) -> Result<Vec<ProjectedRow>> {
    if limit == 0 || limit > MAXIMUM_BATCH_ROWS {
        return Err(KernelError::ValidationInvalid(format!(
            "projection batch size must be in 1..={MAXIMUM_BATCH_ROWS}"
        )));
    }
    reject_quarantine_names(&mapping.source.source_files)?;
    let placeholders = std::iter::repeat_n("?", mapping.source.source_files.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT file_path, source_ordinal, line_start, line_end, fields_json
         FROM source_records
         WHERE job_id = ? AND file_path IN ({placeholders})
           AND (file_path > ? OR (file_path = ? AND source_ordinal > ?))
         ORDER BY file_path, source_ordinal
         LIMIT ?"
    );
    let mut values = Vec::with_capacity(mapping.source.source_files.len() + 5);
    values.push(SqlValue::Text(job_id.to_owned()));
    values.extend(
        mapping
            .source
            .source_files
            .iter()
            .cloned()
            .map(SqlValue::Text),
    );
    values.push(SqlValue::Text(after_file.to_owned()));
    values.push(SqlValue::Text(after_file.to_owned()));
    values.push(SqlValue::Integer(sqlite_integer(
        after_ordinal,
        "projection cursor ordinal",
    )?));
    values.push(SqlValue::Integer(sqlite_integer(
        u64::try_from(limit).unwrap_or(u64::MAX),
        "projection batch size",
    )?));
    let preflight_sql = format!(
        "SELECT COALESCE(SUM(length(CAST(fields_json AS BLOB))), 0)
         FROM (
           SELECT fields_json
           FROM source_records
           WHERE job_id = ? AND file_path IN ({placeholders})
             AND (file_path > ? OR (file_path = ? AND source_ordinal > ?))
           ORDER BY file_path, source_ordinal
           LIMIT ?
         )"
    );
    let source_bytes: i64 =
        connection.query_row(&preflight_sql, params_from_iter(values.clone()), |row| {
            row.get(0)
        })?;
    let source_bytes = usize::try_from(source_bytes).unwrap_or(usize::MAX);
    if source_bytes > MAXIMUM_PROJECTION_BATCH_SOURCE_BYTES {
        return Err(KernelError::ValidationBound(format!(
            "projection batch source bytes {source_bytes} exceed limit {MAXIMUM_PROJECTION_BATCH_SOURCE_BYTES}"
        )));
    }
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut projected = Vec::new();
    let mut retained_bytes = 0_usize;
    for row in rows {
        let (file, ordinal, line_start, line_end, fields_json) = row?;
        let mut projected_row = project_record(
            mapping,
            &file,
            nonnegative_i64(ordinal, "source ordinal")?,
            nonnegative_i64(line_start, "source line start")?,
            nonnegative_i64(line_end, "source line end")?,
            serde_json::from_str(&fields_json)?,
        )?;
        projected_row.source_values.clear();
        let row_retained_bytes = projected_row
            .canonical_values
            .values()
            .filter_map(Option::as_ref)
            .try_fold(0_usize, |total, value| {
                total.checked_add(value.len()).ok_or_else(|| {
                    KernelError::ValidationBound("projected batch byte count overflow".to_owned())
                })
            })?
            .checked_add(
                canonical_json_string(&serde_json::to_value(&projected_row.lineage)?)?.len(),
            )
            .ok_or_else(|| {
                KernelError::ValidationBound("projected batch byte count overflow".to_owned())
            })?;
        retained_bytes = retained_bytes
            .checked_add(row_retained_bytes)
            .ok_or_else(|| {
                KernelError::ValidationBound("projected batch byte count overflow".to_owned())
            })?;
        if retained_bytes > MAXIMUM_PROJECTION_BATCH_RETAINED_BYTES {
            return Err(KernelError::ValidationBound(format!(
                "projection batch retained bytes {retained_bytes} exceed limit {MAXIMUM_PROJECTION_BATCH_RETAINED_BYTES}"
            )));
        }
        projected.push(projected_row);
    }
    Ok(projected)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn project_record(
    mapping: &MappingDocument,
    source_file: &str,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    source_values: Vec<String>,
) -> Result<ProjectedRow> {
    if source_values.len() > MAXIMUM_GOVERNED_SOURCE_COLUMNS {
        return Err(KernelError::ValidationBound(format!(
            "accepted row {source_file}/{source_ordinal} has {} columns; governed limit is {MAXIMUM_GOVERNED_SOURCE_COLUMNS}",
            source_values.len()
        )));
    }
    if source_values
        .iter()
        .any(|value| value.chars().count() > MAXIMUM_GOVERNED_SOURCE_VALUE_CHARS)
    {
        return Err(KernelError::ValidationBound(format!(
            "accepted row {source_file}/{source_ordinal} has a source value above the governed character limit"
        )));
    }
    let row_entity = mapping.entity_for_source_file(source_file)?.to_owned();
    let explicit_scope = mapping.has_explicit_entity_scope();
    let tables = mapping
        .reference_tables
        .iter()
        .map(|table| {
            (
                (
                    mapping.effective_table_entity(table),
                    table.table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut canonical_values = BTreeMap::new();
    let mut lineage = BTreeMap::new();
    let mut projected_value_bytes = 0_usize;
    for binding in &mapping.bindings {
        let target_entity = mapping.effective_binding_entity(binding);
        if target_entity != row_entity {
            continue;
        }
        let (value, mut evidence) = if binding.kind == MappingKind::Constant {
            (
                binding.constant.clone(),
                json!({
                    "bindingId": binding.binding_id,
                    "kind": binding.kind.as_str(),
                    "sourceValueIncluded": false
                }),
            )
        } else {
            if binding.source_file.as_deref() != Some(source_file) {
                continue;
            }
            let ordinal = binding.source_column_ordinal.ok_or_else(|| {
                KernelError::ValidationInvalid(format!(
                    "mapping binding {:?} lost its source ordinal",
                    binding.binding_id
                ))
            })?;
            let original = source_values.get(ordinal).ok_or_else(|| {
                KernelError::IntegrityFailed(format!(
                    "accepted row {source_file}/{source_ordinal} has no column ordinal {ordinal}"
                ))
            })?;
            let projected = match binding.kind {
                MappingKind::Direct => Some(original.clone()),
                MappingKind::EmptyAsNull => (!original.is_empty()).then(|| original.clone()),
                MappingKind::Trim => Some(original.trim().to_owned()),
                MappingKind::UppercaseAscii => {
                    if !original.is_ascii() {
                        return Err(KernelError::ValidationInvalid(format!(
                            "uppercase_ascii binding {:?} encountered non-ASCII input",
                            binding.binding_id
                        )));
                    }
                    Some(original.to_ascii_uppercase())
                }
                MappingKind::LowercaseAscii => {
                    if !original.is_ascii() {
                        return Err(KernelError::ValidationInvalid(format!(
                            "lowercase_ascii binding {:?} encountered non-ASCII input",
                            binding.binding_id
                        )));
                    }
                    Some(original.to_ascii_lowercase())
                }
                MappingKind::Lookup => {
                    let table_id = binding.lookup_table_id.as_deref().ok_or_else(|| {
                        KernelError::ValidationInvalid("lookup binding has no table".to_owned())
                    })?;
                    let projected = tables
                        .get(&(target_entity, table_id))
                        .and_then(|table| table.values.get(original))
                        .cloned();
                    if projected.is_none() {
                        return Err(KernelError::ValidationInvalid(format!(
                            "lookup binding {:?} has no exact entry for accepted row {source_file}/{source_ordinal}; source value suppressed",
                            binding.binding_id
                        )));
                    }
                    projected
                }
                MappingKind::Constant => unreachable!("constant handled before source projection"),
            };
            (
                projected,
                json!({
                    "bindingId": binding.binding_id,
                    "kind": binding.kind.as_str(),
                    "sourceFile": source_file,
                    "sourceColumn": binding.source_column,
                    "sourceColumnOrdinal": ordinal,
                    "sourceValueIncluded": false
                }),
            )
        };
        if let Some(object) = evidence.as_object_mut()
            && let Some(table_id) = &binding.lookup_table_id
        {
            object.insert("lookupTableId".to_owned(), Value::String(table_id.clone()));
        }
        if explicit_scope && let Some(object) = evidence.as_object_mut() {
            object.insert(
                "targetEntityId".to_owned(),
                Value::String(target_entity.to_owned()),
            );
        }
        projected_value_bytes = projected_value_bytes
            .checked_add(value.as_ref().map_or(0, String::len))
            .ok_or_else(|| {
                KernelError::ValidationBound("projected row byte count overflow".to_owned())
            })?;
        if projected_value_bytes > MAXIMUM_PROJECTED_ROW_VALUE_BYTES {
            return Err(KernelError::ValidationBound(format!(
                "projected row {source_file}/{source_ordinal} canonical bytes {projected_value_bytes} exceed limit {MAXIMUM_PROJECTED_ROW_VALUE_BYTES}"
            )));
        }
        canonical_values.insert(binding.target_field_id.clone(), value);
        lineage.insert(binding.target_field_id.clone(), evidence);
    }
    Ok(ProjectedRow {
        entity_id: row_entity,
        source_file: source_file.to_owned(),
        source_ordinal,
        line_start,
        line_end,
        source_values,
        canonical_values,
        lineage,
    })
}

/// Replay one exact accepted row through the immutable mapping for a validation
/// run. This never opens package bytes or cleanup namespaces.
pub(crate) fn project_row_for_validation(
    connection: &Connection,
    run_id: &str,
    file: &str,
    ordinal: u64,
) -> Result<Value> {
    reject_quarantine_names(&[file.to_owned()])?;
    let (job_id, mapping_id, mapping_version): (String, String, String) = connection
        .query_row(
            "SELECT job_id, mapping_id, mapping_version FROM validation_runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| KernelError::ReadModel(format!("unknown validation run {run_id}")))?;
    let mapping = load_stored_mapping(connection, &mapping_id, &mapping_version)?;
    if !mapping
        .document
        .source
        .source_files
        .iter()
        .any(|item| item == file)
    {
        return Err(KernelError::ReadModel(format!(
            "file {file:?} is outside validation run {run_id} mapping scope"
        )));
    }
    let row: Option<(i64, i64, String)> = connection
        .query_row(
            "SELECT line_start, line_end, fields_json FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
            params![job_id, file, sqlite_integer(ordinal, "source ordinal")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((line_start, line_end, fields_json)) = row else {
        return Err(KernelError::ReadModel(format!(
            "accepted row {file:?}/{ordinal} does not exist for validation run {run_id}"
        )));
    };
    let projected = project_record(
        &mapping.document,
        file,
        ordinal,
        nonnegative_i64(line_start, "source line start")?,
        nonnegative_i64(line_end, "source line end")?,
        serde_json::from_str(&fields_json)?,
    )?;
    Ok(serde_json::to_value(projected)?)
}

fn nonnegative_i64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        KernelError::IntegrityFailed(format!("{label} is negative in authoritative state"))
    })
}

/// Replay every immutable mapping and retained validation attempt through the
/// same bounded loader used by runtime validation and practitioner reads.
pub(crate) fn verify_integrity(connection: &Connection, issues: &mut Vec<String>) -> Result<usize> {
    verify_mapping_immutability_guards(connection)?;
    verify_mapping_attempts_integrity(connection, issues)?;
    let mut statement = connection.prepare(
        "SELECT rowid, length(CAST(document_json AS BLOB)),
                length(CAST(coverage_json AS BLOB)),
                length(CAST(mapping_id AS BLOB))
              + length(CAST(mapping_version AS BLOB))
              + length(CAST(mapping_hash AS BLOB))
              + length(CAST(attempt_id AS BLOB))
              + length(CAST(profile_id AS BLOB))
              + length(CAST(profile_version AS BLOB))
              + length(CAST(profile_hash AS BLOB))
              + length(CAST(source_hash AS BLOB))
              + length(CAST(validated_job_id AS BLOB))
              + length(CAST(coverage_hash AS BLOB))
              + length(CAST(created_at AS BLOB))
         FROM mapping_versions ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut checked = 0_usize;
    for row in rows {
        let (rowid, document_bytes, coverage_bytes, metadata_bytes) = row?;
        checked = checked.saturating_add(1);
        if usize::try_from(document_bytes).unwrap_or(usize::MAX) > MAXIMUM_MAPPING_BYTES
            || usize::try_from(coverage_bytes).unwrap_or(usize::MAX)
                > MAXIMUM_MAPPING_COVERAGE_BYTES
            || usize::try_from(metadata_bytes).unwrap_or(usize::MAX)
                > MAXIMUM_MAPPING_METADATA_BYTES
        {
            issues.push(format!(
                "mapping row {rowid} exceeds immutable document/coverage/metadata bounds"
            ));
            continue;
        }
        let (mapping_id, mapping_version): (String, String) = connection.query_row(
            "SELECT mapping_id, mapping_version FROM mapping_versions WHERE rowid = ?1",
            [rowid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Err(error) = load_stored_mapping(connection, &mapping_id, &mapping_version) {
            issues.push(format!(
                "mapping {mapping_id}/{mapping_version} exact replay failed: {error}"
            ));
        }
    }
    Ok(checked)
}

fn normalized_schema_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_mapping_immutability_guards(connection: &Connection) -> Result<()> {
    for (table, name, expected) in [
        (
            "mapping_validation_attempts",
            "mapping_validation_attempts_no_update",
            MAPPING_ATTEMPTS_UPDATE_TRIGGER,
        ),
        (
            "mapping_validation_attempts",
            "mapping_validation_attempts_no_delete",
            MAPPING_ATTEMPTS_DELETE_TRIGGER,
        ),
        (
            "mapping_versions",
            "mapping_versions_no_update",
            MAPPING_VERSIONS_UPDATE_TRIGGER,
        ),
        (
            "mapping_versions",
            "mapping_versions_no_delete",
            MAPPING_VERSIONS_DELETE_TRIGGER,
        ),
    ] {
        let expected = normalized_schema_sql(expected);
        let sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                params![name, table],
                |row| row.get(0),
            )
            .optional()?;
        if sql.as_deref().map(normalized_schema_sql).as_deref() != Some(expected.as_str()) {
            return Err(KernelError::IntegrityFailed(format!(
                "mapping immutable guard {name} is missing or altered"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // The closed VALID/INVALID truth table is audited together.
fn verify_mapping_attempts_integrity(
    connection: &Connection,
    issues: &mut Vec<String>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT rowid, length(CAST(errors_json AS BLOB)),
                length(CAST(id AS BLOB))
              + length(CAST(input_hash AS BLOB))
              + length(CAST(state AS BLOB))
              + COALESCE(length(CAST(mapping_id AS BLOB)), 0)
              + COALESCE(length(CAST(mapping_version AS BLOB)), 0)
              + COALESCE(length(CAST(mapping_hash AS BLOB)), 0)
              + length(CAST(created_at AS BLOB))
              + length(CAST(attempt_hash AS BLOB))
         FROM mapping_validation_attempts ORDER BY rowid",
    )?;
    let attempts = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for attempt in attempts {
        let (rowid, errors_bytes, metadata_bytes) = attempt?;
        if usize::try_from(errors_bytes).unwrap_or(usize::MAX) > MAXIMUM_ATTEMPT_ERRORS_BYTES
            || usize::try_from(metadata_bytes).unwrap_or(usize::MAX)
                > MAXIMUM_ATTEMPT_METADATA_BYTES
        {
            issues.push(format!(
                "mapping validation attempt row {rowid} exceeds immutable evidence bounds"
            ));
            continue;
        }
        let attempt = connection.query_row(
            "SELECT id, input_hash, state, mapping_id, mapping_version, mapping_hash,
                    errors_json, created_at, attempt_hash
             FROM mapping_validation_attempts WHERE rowid = ?1",
            [rowid],
            |row| {
                Ok(MappingAttemptIntegrityRow {
                    attempt_id: row.get(0)?,
                    input_hash: row.get(1)?,
                    state: row.get(2)?,
                    mapping_id: row.get(3)?,
                    mapping_version: row.get(4)?,
                    mapping_hash: row.get(5)?,
                    errors_json: row.get(6)?,
                    created_at: row.get(7)?,
                    attempt_hash: row.get(8)?,
                })
            },
        )?;
        let MappingAttemptIntegrityRow {
            attempt_id,
            input_hash,
            state,
            mapping_id,
            mapping_version,
            mapping_hash,
            errors_json,
            created_at,
            attempt_hash,
        } = attempt;
        let identity_shape = mapping_id.is_some() == mapping_version.is_some()
            && (mapping_hash.is_none() || mapping_id.is_some());
        let hashes_valid = is_lowercase_sha256(&input_hash)
            && mapping_hash.as_deref().is_none_or(is_lowercase_sha256)
            && is_lowercase_sha256(&attempt_hash);
        let id_and_time_valid = Uuid::parse_str(&attempt_id)
            .is_ok_and(|parsed| parsed.to_string() == attempt_id)
            && chrono::DateTime::parse_from_rfc3339(&created_at).is_ok();
        let errors = match parse_unique_json(errors_json.as_bytes()) {
            Ok(errors) => errors,
            Err(error) => {
                issues.push(format!(
                    "mapping validation attempt {attempt_id} errors JSON is invalid: {error}"
                ));
                continue;
            }
        };
        let errors_canonical = canonical_json_string(&errors)? == errors_json;
        let expected_hash = mapping_attempt_hash(
            &attempt_id,
            &input_hash,
            &state,
            mapping_id.as_deref(),
            mapping_version.as_deref(),
            mapping_hash.as_deref(),
            &errors,
            &created_at,
        )?;
        let state_shape = match state.as_str() {
            "VALID" => {
                let mapping_exists = if let (Some(mapping_id), Some(version), Some(hash)) = (
                    mapping_id.as_deref(),
                    mapping_version.as_deref(),
                    mapping_hash.as_deref(),
                ) {
                    connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM mapping_versions
                                       WHERE mapping_id = ?1 AND mapping_version = ?2
                                         AND mapping_hash = ?3)",
                        params![mapping_id, version, hash],
                        |row| row.get::<_, bool>(0),
                    )?
                } else {
                    false
                };
                errors.as_array().is_some_and(Vec::is_empty)
                    && mapping_id.is_some()
                    && mapping_version.is_some()
                    && mapping_hash.is_some()
                    && mapping_exists
            }
            "INVALID" => {
                let errors_valid = errors.as_array().is_some_and(|values| {
                    !values.is_empty()
                        && values.iter().all(|value| {
                            value
                                .get("code")
                                .and_then(Value::as_str)
                                .is_some_and(|code| !code.is_empty() && code.len() <= 128)
                                && value.get("detail").and_then(Value::as_str).is_some_and(
                                    |detail| !detail.is_empty() && detail.chars().count() <= 4_096,
                                )
                        })
                });
                let referenced: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mapping_versions WHERE attempt_id = ?1)",
                    [&attempt_id],
                    |row| row.get(0),
                )?;
                errors_valid && !referenced
            }
            _ => false,
        };
        if !identity_shape
            || !hashes_valid
            || !id_and_time_valid
            || !errors_canonical
            || expected_hash != attempt_hash
            || !state_shape
        {
            issues.push(format!(
                "mapping validation attempt {attempt_id} immutable evidence is invalid"
            ));
        }
    }
    Ok(())
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

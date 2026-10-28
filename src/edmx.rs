//! Bounded, offline import of `OData` EDMX into technical catalogue evidence.
//!
//! This module deliberately models only technical metadata. Its only serializable
//! status is `GENERATED_DRAFT`, and it exposes no conversion into a canonical
//! migration object profile.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use quick_xml::{
    XmlVersion,
    events::{BytesStart, Event},
    name::{NamespaceResolver, ResolveResult},
    reader::NsReader,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{KernelError, Result};

const CATALOGUE_FORMAT_VERSION: &str = "1.0.0";
const PARSER_NAME: &str = "migration-stream-edmx";
const PARSER_VERSION: &str = "1.1.0";
const SUPPORTED_PARSER_VERSIONS: [&str; 2] = ["1.0.0", PARSER_VERSION];
const SERIALIZATION_ID: &str = "migration-stream-technical-catalogue-json-v1";
const EVIDENCE_SCOPE: &str = "compact JSON serialization of catalogue; evidence excluded";
const EDM_V2_NAMESPACE: &[u8] = b"http://schemas.microsoft.com/ado/2008/09/edm";
const EDMX_V2_NAMESPACE: &[u8] = b"http://schemas.microsoft.com/ado/2007/06/edmx";
const EDM_V4_NAMESPACE: &[u8] = b"http://docs.oasis-open.org/odata/ns/edm";
const EDMX_V4_NAMESPACE: &[u8] = b"http://docs.oasis-open.org/odata/ns/edmx";
const COMPATIBILITY_REPORT_SCHEMA: &str = "migration-stream/edmx-compatibility-report/v1";

/// `OData` generation established only from the supported EDMX/EDM namespace
/// pair and root version. It is technical parser evidence, not target
/// compatibility or business authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdmxODataGeneration {
    V2,
    V4,
}

/// Scope-neutral unsupported construct classes inventoried by the bounded
/// importer. A later governed destination scope decides whether an occurrence
/// is irrelevant or blocks profile freeze; this scan does not make that choice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdmxUnsupportedConstructKind {
    ComplexType,
    Inheritance,
    Alias,
    Reference,
    ReferentialConstraint,
    EnumType,
    TypeDefinition,
    Action,
    Function,
    Containment,
    Annotation,
    AssociationSet,
    NavigationBinding,
}

/// Aggregate count for one unsupported construct at one stable technical
/// location. Attribute values and annotation payloads are deliberately absent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxUnsupportedConstruct {
    pub kind: EdmxUnsupportedConstructKind,
    pub location: String,
    pub occurrences: u64,
}

/// Aggregate inventory for one qualified extension attribute.
///
/// The namespace and local name are retained, but the value is not. Qualified
/// attributes never satisfy a core EDMX/EDM attribute such as `Name` or `Type`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxQualifiedAttribute {
    pub element: String,
    pub namespace: String,
    pub name: String,
    pub occurrences: u64,
}

/// Bounded counts of the declarations the importer safely extracts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxExtractedFacts {
    pub namespace_count: u64,
    pub entity_count: u64,
    pub property_count: u64,
    pub navigation_count: u64,
    pub association_count: u64,
    pub entity_set_count: u64,
}

/// Deterministic, scope-neutral compatibility evidence for one exact EDMX byte
/// sequence. This report inventories technical facts and unsupported constructs
/// without interpreting annotations, resolving references, or approving a
/// migration profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxCompatibilityReport {
    pub schema_version: String,
    pub snapshot_sha256: String,
    pub byte_length: u64,
    pub odata_generation: EdmxODataGeneration,
    pub extracted: EdmxExtractedFacts,
    pub qualified_attributes: Vec<EdmxQualifiedAttribute>,
    pub unsupported_constructs: Vec<EdmxUnsupportedConstruct>,
}

/// Resource limits applied before and during XML tokenization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxLimits {
    pub max_input_bytes: usize,
    pub max_depth: usize,
    pub max_events: usize,
    pub max_attributes_per_element: usize,
    pub max_attribute_bytes: usize,
    pub max_text_bytes: usize,
}

impl Default for EdmxLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 8 * 1024 * 1024,
            max_depth: 64,
            max_events: 250_000,
            max_attributes_per_element: 64,
            max_attribute_bytes: 64 * 1024,
            max_text_bytes: 1024 * 1024,
        }
    }
}

impl EdmxLimits {
    /// Rejects zero-valued limits, which would make parser behavior surprising.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::EdmxLimit`] when any limit is zero.
    pub fn validate(&self) -> Result<()> {
        if self.max_input_bytes == 0
            || self.max_depth == 0
            || self.max_events == 0
            || self.max_attributes_per_element == 0
            || self.max_attribute_bytes == 0
            || self.max_text_bytes == 0
        {
            return Err(KernelError::EdmxLimit(
                "all EDMX limits must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Caller-supplied identity and source-system context for an immutable snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxSourceContext {
    pub artifact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
}

/// The sole authority state available to an imported technical catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TechnicalCatalogueStatus {
    #[serde(rename = "GENERATED_DRAFT")]
    GeneratedDraft,
}

/// Snapshot provenance retained in the generated catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxSnapshotEvidence {
    pub sha256: String,
    pub byte_length: u64,
    pub context: EdmxSourceContext,
}

/// Fixed security behavior and configurable limits used by the parser.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxParserOptions {
    pub offline: bool,
    pub namespace_prefix_agnostic: bool,
    pub reject_document_type: bool,
    pub external_resolution: bool,
    pub limits: EdmxLimits,
}

/// Deterministic parser identity and the complete option set used for import.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxParserEvidence {
    pub name: String,
    pub version: String,
    pub options: EdmxParserOptions,
}

/// A property facet copied from the source EDMX without business interpretation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxProperty {
    pub name: String,
    #[serde(rename = "type")]
    pub edm_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
}

/// A navigation facet and, where mechanically available, its EDM cardinality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxNavigationProperty {
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub edm_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<String>,
}

/// An EDM entity type with technical keys, properties, and navigation facets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxEntityType {
    pub name: String,
    pub key: Vec<String>,
    pub properties: Vec<EdmxProperty>,
    pub navigation_properties: Vec<EdmxNavigationProperty>,
}

/// One V2 association endpoint retained verbatim.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxAssociationEnd {
    pub role: String,
    #[serde(rename = "type")]
    pub edm_type: String,
    pub multiplicity: String,
}

/// A V2 association used as technical evidence and navigation-cardinality input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxAssociation {
    pub name: String,
    pub ends: Vec<EdmxAssociationEnd>,
}

/// A technical entity set declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxEntitySet {
    pub name: String,
    pub entity_type: String,
}

/// An EDM entity container and its declared entity sets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxEntityContainer {
    pub name: String,
    pub entity_sets: Vec<EdmxEntitySet>,
}

/// One EDM schema namespace and its supported technical declarations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdmxSchema {
    pub namespace: String,
    pub entity_types: Vec<EdmxEntityType>,
    pub associations: Vec<EdmxAssociation>,
    pub entity_containers: Vec<EdmxEntityContainer>,
}

/// Technical catalogue payload. This is intentionally not a migration profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TechnicalCatalogue {
    pub format_version: String,
    pub status: TechnicalCatalogueStatus,
    pub snapshot: EdmxSnapshotEvidence,
    pub parser: EdmxParserEvidence,
    pub schemas: Vec<EdmxSchema>,
}

/// Hash evidence for the deterministic compact serialization of the catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogueHashEvidence {
    pub algorithm: String,
    pub sha256: String,
    pub serialization: String,
    pub scope: String,
}

/// Import result containing the generated catalogue and evidence over its payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TechnicalCatalogueArtifact {
    pub catalogue: TechnicalCatalogue,
    pub evidence: CatalogueHashEvidence,
}

impl TechnicalCatalogueArtifact {
    /// Serializes the complete artifact as deterministic compact JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Json`] if serialization fails.
    pub fn to_deterministic_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Serializes the complete artifact as deterministic pretty JSON plus newline.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Json`] if serialization fails.
    pub fn to_pretty_json(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Verifies the evidence hash against the catalogue payload.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::EdmxParse`] when the stored evidence does not match.
    pub fn verify_evidence(&self) -> Result<()> {
        let actual = hash_catalogue(&self.catalogue)?;
        if actual != self.evidence.sha256 {
            return Err(KernelError::EdmxParse(
                "technical catalogue evidence hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    /// Verify fixed parser identity, security behavior, canonical ordering, and
    /// payload evidence for a persisted technical catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::EdmxParse`] or [`KernelError::EdmxLimit`] when the
    /// artifact is not valid deterministic output of this importer contract.
    pub fn verify_contract(&self) -> Result<()> {
        if self.catalogue.format_version != CATALOGUE_FORMAT_VERSION {
            return Err(KernelError::EdmxParse(format!(
                "unsupported technical catalogue format version {:?}",
                self.catalogue.format_version
            )));
        }
        let parser = &self.catalogue.parser;
        if parser.name != PARSER_NAME
            || !SUPPORTED_PARSER_VERSIONS.contains(&parser.version.as_str())
        {
            return Err(KernelError::EdmxParse(
                "technical catalogue parser identity is not recognized".to_owned(),
            ));
        }
        if !parser.options.offline
            || !parser.options.namespace_prefix_agnostic
            || !parser.options.reject_document_type
            || parser.options.external_resolution
        {
            return Err(KernelError::EdmxParse(
                "technical catalogue parser security options are not the required offline contract"
                    .to_owned(),
            ));
        }
        parser.options.limits.validate()?;
        if self.catalogue.snapshot.context.artifact.trim().is_empty()
            || self.catalogue.snapshot.sha256.len() != 64
            || !self
                .catalogue
                .snapshot
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(KernelError::EdmxParse(
                "technical catalogue snapshot identity is invalid".to_owned(),
            ));
        }
        if self.evidence.algorithm != "SHA-256"
            || self.evidence.serialization != SERIALIZATION_ID
            || self.evidence.scope != EVIDENCE_SCOPE
        {
            return Err(KernelError::EdmxParse(
                "technical catalogue hash-evidence contract is not recognized".to_owned(),
            ));
        }
        let mut canonical_schemas = self.catalogue.schemas.clone();
        sort_and_validate(&mut canonical_schemas)?;
        if canonical_schemas != self.catalogue.schemas {
            return Err(KernelError::EdmxParse(
                "technical catalogue declarations are not canonically ordered".to_owned(),
            ));
        }
        validate_artifact_schema(self)?;
        self.verify_evidence()
    }

    /// Always rejects this generated artifact as approved runtime authority.
    ///
    /// # Errors
    ///
    /// Always returns [`KernelError::GeneratedDraftAuthority`].
    pub const fn require_approved_profile_authority(&self) -> Result<()> {
        Err(KernelError::GeneratedDraftAuthority)
    }
}

/// Reads a bounded EDMX snapshot and imports its technical catalogue offline.
///
/// # Errors
///
/// Returns an I/O, limit, malformed-XML, or unsupported-EDMX kernel error.
pub fn import_edmx_file(
    path: &Path,
    context: EdmxSourceContext,
    limits: &EdmxLimits,
) -> Result<TechnicalCatalogueArtifact> {
    limits.validate()?;
    let bytes = read_bounded(path, limits.max_input_bytes)?;
    import_edmx_bytes(&bytes, context, limits)
}

/// Imports an already captured EDMX snapshot as bounded technical evidence.
///
/// The snapshot hash covers the exact supplied bytes. No import timestamp is
/// introduced, so repeated imports are byte-for-byte deterministic.
///
/// # Errors
///
/// Returns a limit, malformed-XML, unsupported-EDMX, or JSON kernel error.
pub fn import_edmx_bytes(
    bytes: &[u8],
    context: EdmxSourceContext,
    limits: &EdmxLimits,
) -> Result<TechnicalCatalogueArtifact> {
    limits.validate()?;
    if bytes.len() > limits.max_input_bytes {
        return Err(limit_error(
            "input bytes",
            bytes.len(),
            limits.max_input_bytes,
        ));
    }
    if context.artifact.is_empty() {
        return Err(KernelError::EdmxParse(
            "snapshot artifact identity must not be empty".to_owned(),
        ));
    }

    let (mut schemas, _) = parse_schemas(bytes, limits)?;
    sort_and_validate(&mut schemas)?;
    resolve_v2_navigation_cardinalities(&mut schemas)?;

    let byte_length = u64::try_from(bytes.len()).map_err(|_| {
        KernelError::EdmxLimit("input length cannot be represented as u64".to_owned())
    })?;
    let catalogue = TechnicalCatalogue {
        format_version: CATALOGUE_FORMAT_VERSION.to_owned(),
        status: TechnicalCatalogueStatus::GeneratedDraft,
        snapshot: EdmxSnapshotEvidence {
            sha256: sha256_hex(bytes),
            byte_length,
            context,
        },
        parser: EdmxParserEvidence {
            name: PARSER_NAME.to_owned(),
            version: PARSER_VERSION.to_owned(),
            options: EdmxParserOptions {
                offline: true,
                namespace_prefix_agnostic: true,
                reject_document_type: true,
                external_resolution: false,
                limits: limits.clone(),
            },
        },
        schemas,
    };
    let evidence = CatalogueHashEvidence {
        algorithm: "SHA-256".to_owned(),
        sha256: hash_catalogue(&catalogue)?,
        serialization: SERIALIZATION_ID.to_owned(),
        scope: EVIDENCE_SCOPE.to_owned(),
    };
    let artifact = TechnicalCatalogueArtifact {
        catalogue,
        evidence,
    };
    artifact.verify_contract()?;
    Ok(artifact)
}

/// Scan one exact EDMX byte sequence into deterministic, scope-neutral
/// compatibility evidence.
///
/// The same bounded parser used for catalogue import performs this scan. The
/// report inventories unsupported constructs but does not infer their relevance
/// to a future practitioner-selected scope, interpret annotations, resolve
/// external references, or grant profile authority.
///
/// # Errors
///
/// Returns a limit, malformed-XML, unsupported-namespace, or invalid-core-shape
/// error under the same contract as [`import_edmx_bytes`].
pub fn scan_edmx_compatibility(
    bytes: &[u8],
    limits: &EdmxLimits,
) -> Result<EdmxCompatibilityReport> {
    limits.validate()?;
    if bytes.len() > limits.max_input_bytes {
        return Err(limit_error(
            "input bytes",
            bytes.len(),
            limits.max_input_bytes,
        ));
    }
    let (mut schemas, scan) = parse_schemas(bytes, limits)?;
    sort_and_validate(&mut schemas)?;
    resolve_v2_navigation_cardinalities(&mut schemas)?;
    let byte_length = u64::try_from(bytes.len()).map_err(|_| {
        KernelError::EdmxLimit("input length cannot be represented as u64".to_owned())
    })?;
    let generation = scan
        .generation
        .ok_or_else(|| KernelError::EdmxParse("EDMX generation was not established".to_owned()))?;
    Ok(EdmxCompatibilityReport {
        schema_version: COMPATIBILITY_REPORT_SCHEMA.to_owned(),
        snapshot_sha256: sha256_hex(bytes),
        byte_length,
        odata_generation: generation.into(),
        extracted: extracted_facts(&schemas)?,
        qualified_attributes: scan
            .qualified_attributes
            .into_iter()
            .map(
                |((element, namespace, name), occurrences)| EdmxQualifiedAttribute {
                    element,
                    namespace,
                    name,
                    occurrences,
                },
            )
            .collect(),
        unsupported_constructs: scan
            .unsupported_constructs
            .into_iter()
            .map(|((kind, location), occurrences)| EdmxUnsupportedConstruct {
                kind,
                location,
                occurrences,
            })
            .collect(),
    })
}

/// Read and scan a bounded EDMX file without creating workspace evidence.
///
/// # Errors
///
/// Returns an I/O, limit, malformed-XML, unsupported-namespace, or
/// invalid-core-shape error.
pub fn scan_edmx_file_compatibility(
    path: &Path,
    limits: &EdmxLimits,
) -> Result<EdmxCompatibilityReport> {
    limits.validate()?;
    let bytes = read_bounded(path, limits.max_input_bytes)?;
    scan_edmx_compatibility(&bytes, limits)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
    if metadata.len() > maximum_u64 {
        return Err(KernelError::EdmxLimit(format!(
            "input bytes {} exceed limit {maximum}",
            metadata.len()
        )));
    }

    let mut bytes = Vec::new();
    let read_limit = maximum_u64.saturating_add(1);
    BufReader::new(file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() > maximum {
        return Err(limit_error("input bytes", bytes.len(), maximum));
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ODataVersion {
    V2,
    V4,
}

impl From<ODataVersion> for EdmxODataGeneration {
    fn from(value: ODataVersion) -> Self {
        match value {
            ODataVersion::V2 => Self::V2,
            ODataVersion::V4 => Self::V4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElementNamespace {
    Edm(ODataVersion),
    Edmx(ODataVersion),
    Foreign,
    Unbound,
}

impl ElementNamespace {
    const fn label(self) -> &'static str {
        match self {
            Self::Edm(ODataVersion::V2) => "EDM_V2",
            Self::Edmx(ODataVersion::V2) => "EDMX_V2",
            Self::Edm(ODataVersion::V4) => "EDM_V4",
            Self::Edmx(ODataVersion::V4) => "EDMX_V4",
            Self::Foreign => "FOREIGN",
            Self::Unbound => "UNBOUND",
        }
    }
}

#[derive(Default)]
struct CompatibilityScan {
    generation: Option<ODataVersion>,
    qualified_attributes: BTreeMap<(String, String, String), u64>,
    unsupported_constructs: BTreeMap<(EdmxUnsupportedConstructKind, String), u64>,
}

impl CompatibilityScan {
    fn set_generation(&mut self, generation: ODataVersion) -> Result<()> {
        if self
            .generation
            .replace(generation)
            .is_some_and(|existing| existing != generation)
        {
            return Err(KernelError::EdmxParse(
                "EDMX document contains conflicting OData generations".to_owned(),
            ));
        }
        Ok(())
    }

    fn qualified_attribute(&mut self, element: &str, namespace: &str, name: &str) -> Result<()> {
        increment_bounded_counter(
            self.qualified_attributes.entry((
                element.to_owned(),
                namespace.to_owned(),
                name.to_owned(),
            )),
            "qualified extension attribute occurrences",
        )
    }

    fn unsupported(
        &mut self,
        kind: EdmxUnsupportedConstructKind,
        location: impl Into<String>,
    ) -> Result<()> {
        increment_bounded_counter(
            self.unsupported_constructs.entry((kind, location.into())),
            "unsupported construct occurrences",
        )
    }
}

fn increment_bounded_counter<K: Ord>(
    entry: std::collections::btree_map::Entry<'_, K, u64>,
    label: &str,
) -> Result<()> {
    let value = entry.or_insert(0);
    *value = value
        .checked_add(1)
        .ok_or_else(|| KernelError::EdmxLimit(format!("{label} exceed representable bounds")))?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Frame {
    Other,
    IgnoredUnsupported,
    Edmx(ODataVersion),
    DataServices(ODataVersion),
    Schema {
        schema: usize,
        version: ODataVersion,
    },
    EntityType {
        schema: usize,
        entity: usize,
        version: ODataVersion,
    },
    Key {
        schema: usize,
        entity: usize,
        version: ODataVersion,
    },
    Association {
        schema: usize,
        association: usize,
        version: ODataVersion,
    },
    EntityContainer {
        schema: usize,
        container: usize,
        version: ODataVersion,
    },
}

#[derive(Default)]
struct ParseState {
    schemas: Vec<EdmxSchema>,
    root_seen: bool,
    compatibility: CompatibilityScan,
}

fn parse_schemas(
    bytes: &[u8],
    limits: &EdmxLimits,
) -> Result<(Vec<EdmxSchema>, CompatibilityScan)> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader.resolver_mut().set_max_namespace_bindings(
        limits
            .max_attributes_per_element
            .saturating_mul(limits.max_depth)
            .saturating_add(2),
    );
    let mut buffer = Vec::new();
    let mut frames = Vec::new();
    let mut events = 0_usize;
    let mut state = ParseState::default();

    loop {
        let (namespace, event) = reader
            .read_resolved_event_into(&mut buffer)
            .map_err(|error| KernelError::EdmxParse(format!("XML error: {error}")))?;
        if matches!(event, Event::Eof) {
            break;
        }
        events = events.saturating_add(1);
        if events > limits.max_events {
            return Err(limit_error("XML events", events, limits.max_events));
        }

        match event {
            Event::Start(start) => {
                enforce_depth(frames.len().saturating_add(1), limits)?;
                let namespace = classify_namespace(namespace)?;
                let frame = process_element(
                    &start,
                    namespace,
                    reader.resolver(),
                    &frames,
                    limits,
                    &mut state,
                )?;
                frames.push(frame);
            }
            Event::Empty(start) => {
                enforce_depth(frames.len().saturating_add(1), limits)?;
                let namespace = classify_namespace(namespace)?;
                process_element(
                    &start,
                    namespace,
                    reader.resolver(),
                    &frames,
                    limits,
                    &mut state,
                )?;
            }
            Event::End(_) => {
                if frames.pop().is_none() {
                    return Err(KernelError::EdmxParse(
                        "XML contains an unmatched closing element".to_owned(),
                    ));
                }
            }
            Event::Text(text) => {
                enforce_text_size(text.len(), limits)?;
                let raw_text: &[u8] = text.as_ref().as_bytes();
                if frames.is_empty() && raw_text.iter().any(|byte| !byte.is_ascii_whitespace()) {
                    return Err(KernelError::EdmxParse(
                        "non-whitespace text is forbidden outside the Edmx root".to_owned(),
                    ));
                }
            }
            Event::CData(text) => {
                enforce_text_size(text.len(), limits)?;
                if frames.is_empty() {
                    return Err(KernelError::EdmxParse(
                        "CDATA is forbidden outside the Edmx root".to_owned(),
                    ));
                }
            }
            Event::DocType(_) => {
                return Err(KernelError::EdmxParse(
                    "DOCTYPE declarations are forbidden".to_owned(),
                ));
            }
            _ => {}
        }
        buffer.clear();
    }

    if !frames.is_empty() {
        return Err(KernelError::EdmxParse(
            "XML ended with unclosed elements".to_owned(),
        ));
    }
    if state.schemas.is_empty() {
        return Err(KernelError::EdmxParse(
            "EDMX does not contain an EDM Schema".to_owned(),
        ));
    }
    Ok((state.schemas, state.compatibility))
}

fn classify_namespace(namespace: ResolveResult<'_>) -> Result<ElementNamespace> {
    match namespace {
        ResolveResult::Bound(namespace) => match namespace.as_ref().as_bytes() {
            EDM_V2_NAMESPACE => Ok(ElementNamespace::Edm(ODataVersion::V2)),
            EDM_V4_NAMESPACE => Ok(ElementNamespace::Edm(ODataVersion::V4)),
            EDMX_V2_NAMESPACE => Ok(ElementNamespace::Edmx(ODataVersion::V2)),
            EDMX_V4_NAMESPACE => Ok(ElementNamespace::Edmx(ODataVersion::V4)),
            _ => Ok(ElementNamespace::Foreign),
        },
        ResolveResult::Unbound => Ok(ElementNamespace::Unbound),
        ResolveResult::Unknown(prefix) => Err(KernelError::EdmxParse(format!(
            "element uses undeclared namespace prefix {prefix:?}"
        ))),
    }
}

fn enforce_depth(actual: usize, limits: &EdmxLimits) -> Result<()> {
    if actual > limits.max_depth {
        return Err(limit_error("XML depth", actual, limits.max_depth));
    }
    Ok(())
}

fn enforce_text_size(actual: usize, limits: &EdmxLimits) -> Result<()> {
    if actual > limits.max_text_bytes {
        return Err(limit_error("text bytes", actual, limits.max_text_bytes));
    }
    Ok(())
}

fn process_element(
    start: &BytesStart<'_>,
    namespace: ElementNamespace,
    resolver: &NamespaceResolver,
    frames: &[Frame],
    limits: &EdmxLimits,
    state: &mut ParseState,
) -> Result<Frame> {
    let local = start.local_name();
    let local = local.as_ref().as_bytes();
    let recognized = is_recognized_element(local);
    let unsupported = unsupported_element_kind(namespace, local);
    let capture = recognized || unsupported.is_some();
    let element = format!("{}/{}", namespace.label(), String::from_utf8_lossy(local));
    let attributes = bounded_attributes(
        start,
        limits,
        capture,
        resolver,
        &element,
        &mut state.compatibility,
    )?;
    let technical_location =
        compatibility_location(local, &attributes, frames, &state.schemas, &element);
    record_unsupported_attributes(
        namespace,
        local,
        &attributes,
        &technical_location,
        &mut state.compatibility,
    )?;
    if let Some(kind) = unsupported {
        state.compatibility.unsupported(kind, technical_location)?;
        return Ok(if ignored_unsupported_subtree(kind) {
            Frame::IgnoredUnsupported
        } else {
            Frame::Other
        });
    }
    if matches!(frames.last(), Some(Frame::IgnoredUnsupported)) {
        return Ok(Frame::IgnoredUnsupported);
    }

    let frame = match (namespace, local) {
        (ElementNamespace::Edmx(version), b"Edmx") => process_edmx(
            &attributes,
            frames,
            version,
            &mut state.root_seen,
            &mut state.compatibility,
        )?,
        (ElementNamespace::Edmx(version), b"DataServices") => {
            process_data_services(frames, version)?
        }
        (ElementNamespace::Edm(version), b"Schema") => {
            process_schema(&attributes, frames, &mut state.schemas, version)?
        }
        (ElementNamespace::Edm(version), b"EntityType") => {
            process_entity_type(&attributes, frames, &mut state.schemas, version)?
        }
        (ElementNamespace::Edm(version), b"Key") => process_key(frames, version)?,
        (ElementNamespace::Edm(version), b"PropertyRef") => {
            process_property_ref(&attributes, frames, &mut state.schemas, version)?;
            Frame::Other
        }
        (ElementNamespace::Edm(version), b"Property") => {
            process_property(&attributes, frames, &mut state.schemas, version)?;
            Frame::Other
        }
        (ElementNamespace::Edm(version), b"NavigationProperty") => {
            process_navigation(&attributes, frames, &mut state.schemas, version)?;
            Frame::Other
        }
        (ElementNamespace::Edm(version), b"Association") => {
            process_association(&attributes, frames, &mut state.schemas, version)?
        }
        (ElementNamespace::Edm(version), b"End") => {
            process_association_end(&attributes, frames, &mut state.schemas, version)?;
            Frame::Other
        }
        (ElementNamespace::Edm(version), b"EntityContainer") => {
            process_entity_container(&attributes, frames, &mut state.schemas, version)?
        }
        (ElementNamespace::Edm(version), b"EntitySet") => {
            process_entity_set(&attributes, frames, &mut state.schemas, version)?;
            Frame::Other
        }
        (_, _) if recognized => {
            return Err(KernelError::EdmxParse(format!(
                "recognized element {} uses an unsupported namespace",
                String::from_utf8_lossy(local)
            )));
        }
        _ if frames.is_empty() => {
            return Err(KernelError::EdmxParse(
                "document root must be Edmx in a supported EDMX namespace".to_owned(),
            ));
        }
        _ => Frame::Other,
    };
    Ok(frame)
}

fn compatibility_location(
    local: &[u8],
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &[EdmxSchema],
    fallback: &str,
) -> String {
    if local == b"Schema" {
        return attributes
            .get("Namespace")
            .cloned()
            .unwrap_or_else(|| fallback.to_owned());
    }
    let parent = frames.iter().rev().find_map(|frame| match *frame {
        Frame::Schema { schema, .. } => schemas.get(schema).map(|value| value.namespace.clone()),
        Frame::EntityType { schema, entity, .. } | Frame::Key { schema, entity, .. } => {
            schemas.get(schema).and_then(|value| {
                value
                    .entity_types
                    .get(entity)
                    .map(|entity| format!("{}.{}", value.namespace, entity.name))
            })
        }
        Frame::Association {
            schema,
            association,
            ..
        } => schemas.get(schema).and_then(|value| {
            value
                .associations
                .get(association)
                .map(|association| format!("{}.{}", value.namespace, association.name))
        }),
        Frame::EntityContainer {
            schema, container, ..
        } => schemas.get(schema).and_then(|value| {
            value
                .entity_containers
                .get(container)
                .map(|container| format!("{}.{}", value.namespace, container.name))
        }),
        Frame::Other | Frame::IgnoredUnsupported | Frame::Edmx(_) | Frame::DataServices(_) => None,
    });
    let Some(parent) = parent else {
        return fallback.to_owned();
    };
    let Some(name) = attributes.get("Name") else {
        return format!("{parent}/{}", String::from_utf8_lossy(local));
    };
    if matches!(
        local,
        b"EntityType"
            | b"ComplexType"
            | b"Association"
            | b"EntityContainer"
            | b"Property"
            | b"NavigationProperty"
    ) {
        format!("{parent}.{name}")
    } else {
        format!("{parent}/{}:{name}", String::from_utf8_lossy(local))
    }
}

fn is_recognized_element(local: &[u8]) -> bool {
    matches!(
        local,
        b"Edmx"
            | b"DataServices"
            | b"Schema"
            | b"EntityType"
            | b"Key"
            | b"PropertyRef"
            | b"Property"
            | b"NavigationProperty"
            | b"Association"
            | b"End"
            | b"EntityContainer"
            | b"EntitySet"
    )
}

fn is_core_attribute_name(name: &str) -> bool {
    matches!(
        name,
        "Version"
            | "Namespace"
            | "Name"
            | "Type"
            | "Nullable"
            | "MaxLength"
            | "Precision"
            | "Scale"
            | "Partner"
            | "Relationship"
            | "FromRole"
            | "ToRole"
            | "Role"
            | "Multiplicity"
            | "EntityType"
    )
}

fn unsupported_element_kind(
    namespace: ElementNamespace,
    local: &[u8],
) -> Option<EdmxUnsupportedConstructKind> {
    match (namespace, local) {
        (ElementNamespace::Edm(_), b"ComplexType" | b"CollectionType" | b"RowType") => {
            Some(EdmxUnsupportedConstructKind::ComplexType)
        }
        (ElementNamespace::Edm(_), b"EnumType") => Some(EdmxUnsupportedConstructKind::EnumType),
        (ElementNamespace::Edm(_), b"TypeDefinition") => {
            Some(EdmxUnsupportedConstructKind::TypeDefinition)
        }
        (ElementNamespace::Edm(_), b"ReferentialConstraint") => {
            Some(EdmxUnsupportedConstructKind::ReferentialConstraint)
        }
        (ElementNamespace::Edm(_), b"AssociationSet") => {
            Some(EdmxUnsupportedConstructKind::AssociationSet)
        }
        (ElementNamespace::Edm(_), b"Action" | b"ActionImport") => {
            Some(EdmxUnsupportedConstructKind::Action)
        }
        (ElementNamespace::Edm(_), b"Function" | b"FunctionImport") => {
            Some(EdmxUnsupportedConstructKind::Function)
        }
        (ElementNamespace::Edm(_), b"Annotation" | b"Annotations" | b"Term") => {
            Some(EdmxUnsupportedConstructKind::Annotation)
        }
        (ElementNamespace::Edm(_), b"NavigationPropertyBinding") => {
            Some(EdmxUnsupportedConstructKind::NavigationBinding)
        }
        (ElementNamespace::Edmx(_), b"Reference" | b"Include" | b"IncludeAnnotations") => {
            Some(EdmxUnsupportedConstructKind::Reference)
        }
        _ => None,
    }
}

const fn ignored_unsupported_subtree(kind: EdmxUnsupportedConstructKind) -> bool {
    matches!(
        kind,
        EdmxUnsupportedConstructKind::ComplexType
            | EdmxUnsupportedConstructKind::Reference
            | EdmxUnsupportedConstructKind::ReferentialConstraint
            | EdmxUnsupportedConstructKind::EnumType
            | EdmxUnsupportedConstructKind::TypeDefinition
            | EdmxUnsupportedConstructKind::Action
            | EdmxUnsupportedConstructKind::Function
            | EdmxUnsupportedConstructKind::AssociationSet
    )
}

fn record_unsupported_attributes(
    namespace: ElementNamespace,
    local: &[u8],
    attributes: &BTreeMap<String, String>,
    location: &str,
    compatibility: &mut CompatibilityScan,
) -> Result<()> {
    if !matches!(namespace, ElementNamespace::Edm(_)) {
        return Ok(());
    }
    if local == b"Schema" && attributes.contains_key("Alias") {
        compatibility.unsupported(EdmxUnsupportedConstructKind::Alias, location)?;
    }
    if (matches!(local, b"EntityType" | b"ComplexType") && attributes.contains_key("BaseType"))
        || (local == b"EntityContainer" && attributes.contains_key("Extends"))
    {
        compatibility.unsupported(EdmxUnsupportedConstructKind::Inheritance, location)?;
    }
    if local == b"NavigationProperty"
        && let Some(value) = attributes.get("ContainsTarget")
    {
        match value.as_str() {
            "true" => {
                compatibility.unsupported(EdmxUnsupportedConstructKind::Containment, location)?;
            }
            "false" => {}
            _ => {
                return Err(KernelError::EdmxParse(
                    "NavigationProperty ContainsTarget must be true or false".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn process_edmx(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    version: ODataVersion,
    root_seen: &mut bool,
    compatibility: &mut CompatibilityScan,
) -> Result<Frame> {
    if !frames.is_empty() || *root_seen {
        return Err(KernelError::EdmxParse(
            "Edmx must be the single document root".to_owned(),
        ));
    }
    let declared = required_attribute(attributes, "Edmx", "Version")?;
    let supported = match version {
        ODataVersion::V2 => declared == "1.0",
        ODataVersion::V4 => matches!(declared, "4.0" | "4.01"),
    };
    if !supported {
        return Err(KernelError::EdmxParse(format!(
            "Edmx Version {declared:?} does not match its namespace"
        )));
    }
    *root_seen = true;
    compatibility.set_generation(version)?;
    Ok(Frame::Edmx(version))
}

fn process_data_services(frames: &[Frame], version: ODataVersion) -> Result<Frame> {
    match frames.last() {
        Some(Frame::Edmx(parent_version)) if *parent_version == version => {
            Ok(Frame::DataServices(version))
        }
        _ => Err(KernelError::EdmxParse(
            "DataServices must be a direct child of matching-version Edmx".to_owned(),
        )),
    }
}

fn process_schema(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut Vec<EdmxSchema>,
    version: ODataVersion,
) -> Result<Frame> {
    match frames.last() {
        Some(Frame::DataServices(parent_version)) if *parent_version == version => {}
        _ => {
            return Err(KernelError::EdmxParse(
                "Schema must be a direct child of matching-version DataServices".to_owned(),
            ));
        }
    }
    let namespace = required_attribute(attributes, "Schema", "Namespace")?.to_owned();
    let index = schemas.len();
    schemas.push(EdmxSchema {
        namespace,
        entity_types: Vec::new(),
        associations: Vec::new(),
        entity_containers: Vec::new(),
    });
    Ok(Frame::Schema {
        schema: index,
        version,
    })
}

fn process_entity_type(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<Frame> {
    let schema = require_schema_parent(frames, "EntityType", version)?;
    let name = required_attribute(attributes, "EntityType", "Name")?.to_owned();
    let entity = schemas
        .get_mut(schema)
        .ok_or_else(invalid_parser_state)?
        .entity_types
        .len();
    schemas[schema].entity_types.push(EdmxEntityType {
        name,
        key: Vec::new(),
        properties: Vec::new(),
        navigation_properties: Vec::new(),
    });
    Ok(Frame::EntityType {
        schema,
        entity,
        version,
    })
}

fn process_key(frames: &[Frame], version: ODataVersion) -> Result<Frame> {
    let (schema, entity) = require_entity_parent(frames, "Key", version)?;
    Ok(Frame::Key {
        schema,
        entity,
        version,
    })
}

fn process_property_ref(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<()> {
    let entity = require_key_parent(frames, version)?;
    let name = required_attribute(attributes, "PropertyRef", "Name")?.to_owned();
    entity_mut(schemas, entity)?.key.push(name);
    Ok(())
}

fn process_property(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<()> {
    let property = EdmxProperty {
        name: required_attribute(attributes, "Property", "Name")?.to_owned(),
        edm_type: required_attribute(attributes, "Property", "Type")?.to_owned(),
        nullable: optional_bool(attributes, "Property", "Nullable")?,
        max_length: attributes.get("MaxLength").cloned(),
        precision: attributes.get("Precision").cloned(),
        scale: attributes.get("Scale").cloned(),
    };
    entity_mut(schemas, require_entity_parent(frames, "Property", version)?)?
        .properties
        .push(property);
    Ok(())
}

fn process_navigation(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<()> {
    let edm_type = attributes.get("Type").cloned();
    let nullable = optional_bool(attributes, "NavigationProperty", "Nullable")?;
    let (collection, cardinality) = direct_cardinality(edm_type.as_deref(), nullable);
    let navigation = EdmxNavigationProperty {
        name: required_attribute(attributes, "NavigationProperty", "Name")?.to_owned(),
        edm_type,
        nullable,
        partner: attributes.get("Partner").cloned(),
        relationship: attributes.get("Relationship").cloned(),
        from_role: attributes.get("FromRole").cloned(),
        to_role: attributes.get("ToRole").cloned(),
        collection,
        cardinality,
    };
    entity_mut(
        schemas,
        require_entity_parent(frames, "NavigationProperty", version)?,
    )?
    .navigation_properties
    .push(navigation);
    Ok(())
}

fn process_association(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<Frame> {
    let schema = require_schema_parent(frames, "Association", version)?;
    let name = required_attribute(attributes, "Association", "Name")?.to_owned();
    let association = schemas
        .get_mut(schema)
        .ok_or_else(invalid_parser_state)?
        .associations
        .len();
    schemas[schema].associations.push(EdmxAssociation {
        name,
        ends: Vec::new(),
    });
    Ok(Frame::Association {
        schema,
        association,
        version,
    })
}

fn process_association_end(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<()> {
    let association = require_association_parent(frames, version)?;
    let endpoint = EdmxAssociationEnd {
        role: required_attribute(attributes, "End", "Role")?.to_owned(),
        edm_type: required_attribute(attributes, "End", "Type")?.to_owned(),
        multiplicity: required_attribute(attributes, "End", "Multiplicity")?.to_owned(),
    };
    association_mut(schemas, association)?.ends.push(endpoint);
    Ok(())
}

fn process_entity_container(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<Frame> {
    let schema = require_schema_parent(frames, "EntityContainer", version)?;
    let name = required_attribute(attributes, "EntityContainer", "Name")?.to_owned();
    let container = schemas
        .get_mut(schema)
        .ok_or_else(invalid_parser_state)?
        .entity_containers
        .len();
    schemas[schema].entity_containers.push(EdmxEntityContainer {
        name,
        entity_sets: Vec::new(),
    });
    Ok(Frame::EntityContainer {
        schema,
        container,
        version,
    })
}

fn process_entity_set(
    attributes: &BTreeMap<String, String>,
    frames: &[Frame],
    schemas: &mut [EdmxSchema],
    version: ODataVersion,
) -> Result<()> {
    let container = require_container_parent(frames, version)?;
    let entity_set = EdmxEntitySet {
        name: required_attribute(attributes, "EntitySet", "Name")?.to_owned(),
        entity_type: required_attribute(attributes, "EntitySet", "EntityType")?.to_owned(),
    };
    container_mut(schemas, container)?
        .entity_sets
        .push(entity_set);
    Ok(())
}

fn bounded_attributes(
    start: &BytesStart<'_>,
    limits: &EdmxLimits,
    capture: bool,
    resolver: &NamespaceResolver,
    element: &str,
    compatibility: &mut CompatibilityScan,
) -> Result<BTreeMap<String, String>> {
    let attributes = start.attributes();
    let mut output = BTreeMap::new();
    let mut count = 0_usize;
    for attribute in attributes {
        count = count.saturating_add(1);
        if count > limits.max_attributes_per_element {
            return Err(limit_error(
                "attributes per element",
                count,
                limits.max_attributes_per_element,
            ));
        }
        let attribute = attribute.map_err(|error| {
            KernelError::EdmxParse(format!("malformed or duplicate XML attribute: {error}"))
        })?;
        let raw_name = attribute.key.as_ref();
        let raw_value = attribute.value.as_ref();
        let raw_size = raw_name.len().saturating_add(raw_value.len());
        if raw_size > limits.max_attribute_bytes {
            return Err(limit_error(
                "attribute bytes",
                raw_size,
                limits.max_attribute_bytes,
            ));
        }
        if raw_name == "xmlns" || raw_name.starts_with("xmlns:") {
            continue;
        }
        if raw_name.contains(':') {
            let (namespace, local_name) = resolver.resolve_attribute(attribute.key);
            let local_name = local_name.as_ref();
            let namespace = match namespace {
                ResolveResult::Bound(namespace) => namespace.as_ref().to_owned(),
                ResolveResult::Unknown(prefix) => {
                    return Err(KernelError::EdmxParse(format!(
                        "attribute uses undeclared namespace prefix {prefix:?}"
                    )));
                }
                ResolveResult::Unbound => {
                    return Err(KernelError::EdmxParse(
                        "qualified attribute has no bound namespace".to_owned(),
                    ));
                }
            };
            if capture && is_core_attribute_name(local_name) {
                return Err(KernelError::EdmxParse(format!(
                    "recognized element attribute {raw_name} must be unqualified"
                )));
            }
            compatibility.qualified_attribute(element, &namespace, local_name)?;
            continue;
        }
        if !capture {
            continue;
        }
        let name = raw_name.to_owned();
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| {
                KernelError::EdmxParse(format!("invalid value for attribute {name}: {error}"))
            })?
            .into_owned();
        if output.insert(name.clone(), value).is_some() {
            return Err(KernelError::EdmxParse(format!(
                "duplicate local attribute name: {name}"
            )));
        }
    }
    Ok(output)
}

fn required_attribute<'a>(
    attributes: &'a BTreeMap<String, String>,
    element: &str,
    name: &str,
) -> Result<&'a str> {
    attributes
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            KernelError::EdmxParse(format!(
                "{element} is missing or has an empty required {name} attribute"
            ))
        })
}

fn optional_bool(
    attributes: &BTreeMap<String, String>,
    element: &str,
    name: &str,
) -> Result<Option<bool>> {
    let Some(value) = attributes.get(name) else {
        return Ok(None);
    };
    match value.as_str() {
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        _ => Err(KernelError::EdmxParse(format!(
            "{element} has invalid Boolean {name} value {value}"
        ))),
    }
}

fn require_schema_parent(frames: &[Frame], element: &str, version: ODataVersion) -> Result<usize> {
    match frames.last() {
        Some(Frame::Schema {
            schema,
            version: parent_version,
        }) if *parent_version == version => Ok(*schema),
        _ => Err(KernelError::EdmxParse(format!(
            "{element} must be a direct child of a matching-version Schema"
        ))),
    }
}

fn require_entity_parent(
    frames: &[Frame],
    element: &str,
    version: ODataVersion,
) -> Result<(usize, usize)> {
    match frames.last() {
        Some(Frame::EntityType {
            schema,
            entity,
            version: parent_version,
        }) if *parent_version == version => Ok((*schema, *entity)),
        _ => Err(KernelError::EdmxParse(format!(
            "{element} must be a direct child of a matching-version EntityType"
        ))),
    }
}

fn require_key_parent(frames: &[Frame], version: ODataVersion) -> Result<(usize, usize)> {
    match frames.last() {
        Some(Frame::Key {
            schema,
            entity,
            version: parent_version,
        }) if *parent_version == version => Ok((*schema, *entity)),
        _ => Err(KernelError::EdmxParse(
            "PropertyRef must be a direct child of a matching-version Key".to_owned(),
        )),
    }
}

fn require_association_parent(frames: &[Frame], version: ODataVersion) -> Result<(usize, usize)> {
    match frames.last() {
        Some(Frame::Association {
            schema,
            association,
            version: parent_version,
        }) if *parent_version == version => Ok((*schema, *association)),
        _ => Err(KernelError::EdmxParse(
            "End must be a direct child of a matching-version Association".to_owned(),
        )),
    }
}

fn require_container_parent(frames: &[Frame], version: ODataVersion) -> Result<(usize, usize)> {
    match frames.last() {
        Some(Frame::EntityContainer {
            schema,
            container,
            version: parent_version,
        }) if *parent_version == version => Ok((*schema, *container)),
        _ => Err(KernelError::EdmxParse(
            "EntitySet must be a direct child of a matching-version EntityContainer".to_owned(),
        )),
    }
}

fn entity_mut(
    schemas: &mut [EdmxSchema],
    (schema, entity): (usize, usize),
) -> Result<&mut EdmxEntityType> {
    schemas
        .get_mut(schema)
        .and_then(|value| value.entity_types.get_mut(entity))
        .ok_or_else(invalid_parser_state)
}

fn association_mut(
    schemas: &mut [EdmxSchema],
    (schema, association): (usize, usize),
) -> Result<&mut EdmxAssociation> {
    schemas
        .get_mut(schema)
        .and_then(|value| value.associations.get_mut(association))
        .ok_or_else(invalid_parser_state)
}

fn container_mut(
    schemas: &mut [EdmxSchema],
    (schema, container): (usize, usize),
) -> Result<&mut EdmxEntityContainer> {
    schemas
        .get_mut(schema)
        .and_then(|value| value.entity_containers.get_mut(container))
        .ok_or_else(invalid_parser_state)
}

fn invalid_parser_state() -> KernelError {
    KernelError::EdmxParse("internal EDMX parser state is inconsistent".to_owned())
}

fn direct_cardinality(
    edm_type: Option<&str>,
    nullable: Option<bool>,
) -> (Option<bool>, Option<String>) {
    let Some(edm_type) = edm_type else {
        return (None, None);
    };
    if edm_type.starts_with("Collection(") && edm_type.ends_with(')') {
        return (Some(true), Some("0..*".to_owned()));
    }
    let cardinality = if nullable == Some(false) { "1" } else { "0..1" };
    (Some(false), Some(cardinality.to_owned()))
}

#[derive(Clone, Debug)]
struct AssociationLookup {
    schema_namespace: String,
    name: String,
    qualified_name: String,
    ends: Vec<EdmxAssociationEnd>,
}

fn resolve_v2_navigation_cardinalities(schemas: &mut [EdmxSchema]) -> Result<()> {
    let lookups: Vec<_> = schemas
        .iter()
        .flat_map(|schema| {
            schema
                .associations
                .iter()
                .map(|association| AssociationLookup {
                    schema_namespace: schema.namespace.clone(),
                    name: association.name.clone(),
                    qualified_name: format!("{}.{}", schema.namespace, association.name),
                    ends: association.ends.clone(),
                })
        })
        .collect();

    for schema in schemas {
        let schema_namespace = schema.namespace.clone();
        for navigation in schema
            .entity_types
            .iter_mut()
            .flat_map(|entity| entity.navigation_properties.iter_mut())
        {
            if navigation.edm_type.is_some() {
                continue;
            }
            let (Some(relationship), Some(to_role)) = (
                navigation.relationship.as_deref(),
                navigation.to_role.as_deref(),
            ) else {
                continue;
            };
            let mut candidates = lookups.iter().filter(|lookup| {
                lookup.qualified_name == relationship
                    || (lookup.schema_namespace == schema_namespace && lookup.name == relationship)
            });
            let Some(association) = candidates.next() else {
                continue;
            };
            if candidates.next().is_some() {
                return Err(KernelError::EdmxParse(format!(
                    "NavigationProperty {:?} has ambiguous V2 Relationship {:?}",
                    navigation.name, relationship
                )));
            }
            if let Some(endpoint) = association.ends.iter().find(|end| end.role == to_role) {
                let (collection, cardinality) = multiplicity_cardinality(&endpoint.multiplicity);
                navigation.collection = collection;
                navigation.cardinality = cardinality;
            }
        }
    }
    Ok(())
}

fn multiplicity_cardinality(value: &str) -> (Option<bool>, Option<String>) {
    match value {
        "*" => (Some(true), Some("0..*".to_owned())),
        "1" => (Some(false), Some("1".to_owned())),
        "0..1" => (Some(false), Some("0..1".to_owned())),
        _ => (None, None),
    }
}

fn sort_and_validate(schemas: &mut [EdmxSchema]) -> Result<()> {
    schemas.sort_by(|left, right| left.namespace.cmp(&right.namespace));
    reject_duplicates(
        "schema namespace",
        schemas.iter().map(|schema| schema.namespace.as_str()),
    )?;

    for schema in schemas {
        schema
            .entity_types
            .sort_by(|left, right| left.name.cmp(&right.name));
        reject_duplicates(
            "entity type name",
            schema
                .entity_types
                .iter()
                .map(|entity| entity.name.as_str()),
        )?;
        for entity in &mut schema.entity_types {
            entity.key.sort();
            reject_duplicates("key property", entity.key.iter().map(String::as_str))?;
            entity
                .properties
                .sort_by(|left, right| left.name.cmp(&right.name));
            reject_duplicates(
                "property name",
                entity
                    .properties
                    .iter()
                    .map(|property| property.name.as_str()),
            )?;
            entity
                .navigation_properties
                .sort_by(|left, right| left.name.cmp(&right.name));
            reject_duplicates(
                "navigation property name",
                entity
                    .navigation_properties
                    .iter()
                    .map(|navigation| navigation.name.as_str()),
            )?;
        }

        schema
            .associations
            .sort_by(|left, right| left.name.cmp(&right.name));
        reject_duplicates(
            "association name",
            schema
                .associations
                .iter()
                .map(|association| association.name.as_str()),
        )?;
        for association in &mut schema.associations {
            association
                .ends
                .sort_by(|left, right| left.role.cmp(&right.role));
            reject_duplicates(
                "association end role",
                association.ends.iter().map(|end| end.role.as_str()),
            )?;
        }

        schema
            .entity_containers
            .sort_by(|left, right| left.name.cmp(&right.name));
        reject_duplicates(
            "entity container name",
            schema
                .entity_containers
                .iter()
                .map(|container| container.name.as_str()),
        )?;
        for container in &mut schema.entity_containers {
            container
                .entity_sets
                .sort_by(|left, right| left.name.cmp(&right.name));
            reject_duplicates(
                "entity set name",
                container.entity_sets.iter().map(|set| set.name.as_str()),
            )?;
        }
    }
    Ok(())
}

fn reject_duplicates<'a>(kind: &str, names: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(KernelError::EdmxParse(format!("duplicate {kind}: {name}")));
        }
    }
    Ok(())
}

fn extracted_facts(schemas: &[EdmxSchema]) -> Result<EdmxExtractedFacts> {
    let mut entities = 0_usize;
    let mut properties = 0_usize;
    let mut navigations = 0_usize;
    let mut associations = 0_usize;
    let mut entity_sets = 0_usize;
    for schema in schemas {
        entities = entities
            .checked_add(schema.entity_types.len())
            .ok_or_else(|| count_overflow("entity"))?;
        associations = associations
            .checked_add(schema.associations.len())
            .ok_or_else(|| count_overflow("association"))?;
        for entity in &schema.entity_types {
            properties = properties
                .checked_add(entity.properties.len())
                .ok_or_else(|| count_overflow("property"))?;
            navigations = navigations
                .checked_add(entity.navigation_properties.len())
                .ok_or_else(|| count_overflow("navigation"))?;
        }
        for container in &schema.entity_containers {
            entity_sets = entity_sets
                .checked_add(container.entity_sets.len())
                .ok_or_else(|| count_overflow("entity set"))?;
        }
    }
    Ok(EdmxExtractedFacts {
        namespace_count: u64::try_from(schemas.len()).map_err(|_| count_overflow("namespace"))?,
        entity_count: u64::try_from(entities).map_err(|_| count_overflow("entity"))?,
        property_count: u64::try_from(properties).map_err(|_| count_overflow("property"))?,
        navigation_count: u64::try_from(navigations).map_err(|_| count_overflow("navigation"))?,
        association_count: u64::try_from(associations)
            .map_err(|_| count_overflow("association"))?,
        entity_set_count: u64::try_from(entity_sets).map_err(|_| count_overflow("entity set"))?,
    })
}

fn count_overflow(kind: &str) -> KernelError {
    KernelError::EdmxLimit(format!("{kind} count exceeds representable bounds"))
}

fn limit_error(kind: &str, actual: usize, limit: usize) -> KernelError {
    KernelError::EdmxLimit(format!("{kind} {actual} exceed limit {limit}"))
}

fn validate_artifact_schema(artifact: &TechnicalCatalogueArtifact) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../schemas/technical-catalogue.schema.json"
    ))
    .map_err(|error| {
        KernelError::EdmxParse(format!(
            "embedded technical catalogue schema is invalid JSON: {error}"
        ))
    })?;
    jsonschema::draft202012::meta::validate(&schema).map_err(|error| {
        KernelError::EdmxParse(format!(
            "embedded technical catalogue schema is invalid: {error}"
        ))
    })?;
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .map_err(|error| {
            KernelError::EdmxParse(format!(
                "embedded technical catalogue schema cannot be compiled: {error}"
            ))
        })?;
    let instance = serde_json::to_value(artifact)?;
    let mut errors = validator
        .iter_errors(&instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        errors.sort_unstable();
        return Err(KernelError::EdmxParse(format!(
            "technical catalogue violates its artifact schema: {}",
            errors.join("; ")
        )));
    }
    Ok(())
}

fn hash_catalogue(catalogue: &TechnicalCatalogue) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(catalogue)?))
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::canonical::lower_hex(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2: &[u8] = br#"<?xml version="1.0" encoding="utf-8"?>
<edmx:Edmx xmlns:edmx="http://schemas.microsoft.com/ado/2007/06/edmx" Version="1.0">
  <edmx:DataServices>
    <Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Legacy.Model">
      <EntityType Name="Customer">
        <Key><PropertyRef Name="Id" /></Key>
        <Property Name="Id" Type="Edm.String" Nullable="false" MaxLength="10" />
        <NavigationProperty Name="Orders" Relationship="Legacy.Model.CustomerOrders" FromRole="Customer" ToRole="Orders" />
      </EntityType>
      <EntityType Name="Order">
        <Key><PropertyRef Name="Id" /></Key>
        <Property Name="Id" Type="Edm.Int32" Nullable="false" Precision="10" Scale="0" />
      </EntityType>
      <Association Name="CustomerOrders">
        <End Role="Orders" Type="Legacy.Model.Order" Multiplicity="*" />
        <End Role="Customer" Type="Legacy.Model.Customer" Multiplicity="1" />
      </Association>
      <EntityContainer Name="LegacyContainer">
        <EntitySet Name="Orders" EntityType="Legacy.Model.Order" />
        <EntitySet Name="Customers" EntityType="Legacy.Model.Customer" />
      </EntityContainer>
    </Schema>
  </edmx:DataServices>
</edmx:Edmx>"#;

    fn context(artifact: &str) -> EdmxSourceContext {
        EdmxSourceContext {
            artifact: artifact.to_owned(),
            system: Some("SYNTHETIC_ODATA_V4".to_owned()),
            client: Some("SYNTHETIC".to_owned()),
            release: Some("SYNTHETIC_SAMPLE".to_owned()),
        }
    }

    fn v4_document(schemas: &str) -> String {
        format!(
            r#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" xmlns:d="http://docs.oasis-open.org/odata/ns/edm" Version="4.0"><e:DataServices>{schemas}</e:DataServices></e:Edmx>"#
        )
    }

    fn v2_document(schemas: &str) -> String {
        format!(
            r#"<e:Edmx xmlns:e="http://schemas.microsoft.com/ado/2007/06/edmx" Version="1.0"><e:DataServices>{schemas}</e:DataServices></e:Edmx>"#
        )
    }

    #[test]
    fn utf8_attributes_keep_entity_and_xml_whitespace_normalization() {
        let document = v4_document(
            r#"<d:Schema Namespace="Δοκιμή"><d:EntityType Name="Πελάτης"><d:Property Name="Label" Type="Edm.String" MaxLength="A&amp;B&#x9;C	D"/></d:EntityType></d:Schema>"#,
        );
        let artifact = import_edmx_bytes(
            document.as_bytes(),
            context("utf8.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let schema = &artifact.catalogue.schemas[0];
        assert_eq!(schema.namespace, "Δοκιμή");
        assert_eq!(schema.entity_types[0].name, "Πελάτης");
        assert_eq!(
            schema.entity_types[0].properties[0].max_length.as_deref(),
            Some("A&B\tC D")
        );
        artifact.verify_contract().unwrap();
    }

    #[test]
    fn rejects_invalid_utf8_and_excess_namespace_declarations() {
        let document = v4_document(r#"<d:Schema Namespace="X"/>"#);
        let invalid = document.replace("Namespace=\"X\"", "Namespace=\"INVALID\"");
        let mut invalid = invalid.into_bytes();
        let offset = invalid
            .windows(7)
            .position(|value| value == b"INVALID")
            .unwrap();
        invalid[offset] = 0xff;
        assert!(matches!(
            import_edmx_bytes(&invalid, context("invalid.edmx"), &EdmxLimits::default()),
            Err(KernelError::EdmxParse(_))
        ));
        let limits = EdmxLimits {
            max_attributes_per_element: 2,
            ..EdmxLimits::default()
        };
        let document = document.replacen(
            "Version=\"4.0\"",
            "xmlns:x=\"urn:x\" xmlns:y=\"urn:y\" Version=\"4.0\"",
            1,
        );
        assert!(matches!(
            import_edmx_bytes(document.as_bytes(), context("namespaces.edmx"), &limits),
            Err(KernelError::EdmxLimit(_))
        ));
    }

    #[test]
    fn fixture_is_deterministic_and_matches_golden() {
        let bytes = include_bytes!("../fixtures/metadata/sanitized-customer-account.edmx");
        let first = import_edmx_bytes(
            bytes,
            context("fixtures/metadata/sanitized-customer-account.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let second = import_edmx_bytes(
            bytes,
            context("fixtures/metadata/sanitized-customer-account.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.to_deterministic_json().unwrap(),
            second.to_deterministic_json().unwrap()
        );
        assert_eq!(
            String::from_utf8(first.to_pretty_json().unwrap()).unwrap(),
            include_str!("../fixtures/golden/sanitized-customer-account.catalogue.json")
        );
        first.verify_contract().unwrap();
    }

    #[test]
    fn persisted_v1_parser_evidence_remains_replayable() {
        let mut legacy =
            import_edmx_bytes(V2, context("legacy-v1.edmx"), &EdmxLimits::default()).unwrap();
        legacy.catalogue.parser.version = "1.0.0".to_owned();
        legacy.evidence.sha256 = hash_catalogue(&legacy.catalogue).unwrap();

        legacy.verify_contract().unwrap();
    }

    #[test]
    fn unknown_parser_evidence_fails_closed() {
        let mut unknown =
            import_edmx_bytes(V2, context("unknown.edmx"), &EdmxLimits::default()).unwrap();
        unknown.catalogue.parser.version = "9.9.9".to_owned();
        unknown.evidence.sha256 = hash_catalogue(&unknown.catalogue).unwrap();

        let error = unknown.verify_contract().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("parser identity is not recognized")
        );
    }

    #[test]
    fn golden_satisfies_technical_catalogue_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/technical-catalogue.schema.json"))
                .unwrap();
        jsonschema::draft202012::meta::validate(&schema).unwrap();
        let validator = jsonschema::draft202012::options().build(&schema).unwrap();
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/golden/sanitized-customer-account.catalogue.json"
        ))
        .unwrap();

        assert!(validator.is_valid(&golden));
    }

    #[test]
    fn source_order_does_not_change_catalogue_schema_order() {
        let first = br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" xmlns:x="http://docs.oasis-open.org/odata/ns/edm" Version="4.0">
          <e:DataServices><x:Schema Namespace="Order.Test">
            <x:EntityType Name="B"><x:Property Name="Z" Type="Edm.String"/><x:Property Name="A" Type="Edm.String"/></x:EntityType>
            <x:EntityType Name="A"><x:Property Name="Id" Type="Edm.String"/></x:EntityType>
          </x:Schema></e:DataServices>
        </e:Edmx>"#;
        let second = br#"<m:Edmx xmlns:m="http://docs.oasis-open.org/odata/ns/edmx" xmlns:z="http://docs.oasis-open.org/odata/ns/edm" Version="4.0">
          <m:DataServices><z:Schema Namespace="Order.Test">
            <z:EntityType Name="A"><z:Property Type="Edm.String" Name="Id"/></z:EntityType>
            <z:EntityType Name="B"><z:Property Type="Edm.String" Name="A"/><z:Property Type="Edm.String" Name="Z"/></z:EntityType>
          </z:Schema></m:DataServices>
        </m:Edmx>"#;
        let first =
            import_edmx_bytes(first, context("order.edmx"), &EdmxLimits::default()).unwrap();
        let second =
            import_edmx_bytes(second, context("order.edmx"), &EdmxLimits::default()).unwrap();

        assert_eq!(first.catalogue.schemas, second.catalogue.schemas);
        assert_ne!(
            first.catalogue.snapshot.sha256,
            second.catalogue.snapshot.sha256
        );
    }

    #[test]
    fn v2_association_resolves_navigation_cardinality() {
        let artifact = import_edmx_bytes(V2, context("v2.edmx"), &EdmxLimits::default()).unwrap();
        let customer = artifact.catalogue.schemas[0]
            .entity_types
            .iter()
            .find(|entity| entity.name == "Customer")
            .unwrap();
        let navigation = &customer.navigation_properties[0];

        assert_eq!(navigation.collection, Some(true));
        assert_eq!(navigation.cardinality.as_deref(), Some("0..*"));
        assert_eq!(
            artifact.catalogue.schemas[0].associations[0].ends[0].role,
            "Customer"
        );
    }

    #[test]
    fn vendor_extension_v2_attributes_are_tolerated_but_never_interpreted() {
        let bytes = include_bytes!("../fixtures/metadata/sanitized-sales-v2.edmx");
        let artifact = import_edmx_bytes(
            bytes,
            context("sanitized-sales-v2.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let first = scan_edmx_compatibility(bytes, &EdmxLimits::default()).unwrap();
        let second = scan_edmx_compatibility(bytes, &EdmxLimits::default()).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.schema_version,
            "migration-stream/edmx-compatibility-report/v1"
        );
        assert_eq!(first.odata_generation, EdmxODataGeneration::V2);
        assert_eq!(first.snapshot_sha256, artifact.catalogue.snapshot.sha256);
        assert_eq!(first.extracted.namespace_count, 1);
        assert_eq!(first.extracted.entity_count, 2);
        assert_eq!(first.extracted.property_count, 29);
        assert_eq!(first.extracted.navigation_count, 2);
        assert_eq!(first.extracted.association_count, 1);
        assert_eq!(first.extracted.entity_set_count, 2);
        assert!(first.qualified_attributes.iter().any(|attribute| {
            attribute.namespace == "urn:migration-stream:synthetic:edmx:extensions"
                && attribute.name == "label"
        }));
        assert!(first.qualified_attributes.iter().any(|attribute| {
            attribute.namespace == "http://schemas.microsoft.com/ado/2007/08/dataservices/metadata"
                && attribute.name == "DataServiceVersion"
        }));
        let unsupported = first
            .unsupported_constructs
            .iter()
            .map(|construct| construct.kind)
            .collect::<BTreeSet<_>>();
        for kind in [
            EdmxUnsupportedConstructKind::ReferentialConstraint,
            EdmxUnsupportedConstructKind::AssociationSet,
            EdmxUnsupportedConstructKind::Function,
            EdmxUnsupportedConstructKind::Annotation,
        ] {
            assert!(unsupported.contains(&kind), "missing {kind:?}");
        }
        assert!(first.unsupported_constructs.iter().any(|construct| {
            construct.kind == EdmxUnsupportedConstructKind::ReferentialConstraint
                && construct.location
                    == "Synthetic.VendorExtended.Sales.SalesOrderItemAssociation/ReferentialConstraint"
        }));
        let report_json = serde_json::to_string(&first).unwrap();
        assert!(!report_json.contains("Synthetic sales order"));
        assert!(!report_json.contains("Synthetic local check"));

        let schema = &artifact.catalogue.schemas[0];
        let root = schema
            .entity_types
            .iter()
            .find(|entity| entity.name == "SalesOrder")
            .unwrap();
        let navigation = root
            .navigation_properties
            .iter()
            .find(|navigation| navigation.name == "to_Item")
            .unwrap();
        assert_eq!(navigation.cardinality.as_deref(), Some("0..*"));
        assert_eq!(schema.associations[0].ends.len(), 2);
        assert_eq!(root.key, ["SalesOrder"]);
    }

    #[test]
    fn revised_v2_snapshot_adds_only_the_explicit_extension_property() {
        let original_bytes = include_bytes!("../fixtures/metadata/sanitized-sales-v2.edmx");
        let revised_bytes = include_bytes!("../fixtures/metadata/sanitized-sales-v2-revised.edmx");
        let original = import_edmx_bytes(
            original_bytes,
            context("sanitized-sales-v2.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let revised = import_edmx_bytes(
            revised_bytes,
            context("sanitized-sales-v2-revised.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let original_report =
            scan_edmx_compatibility(original_bytes, &EdmxLimits::default()).unwrap();
        let revised_report =
            scan_edmx_compatibility(revised_bytes, &EdmxLimits::default()).unwrap();

        assert_ne!(
            original.catalogue.snapshot.sha256,
            revised.catalogue.snapshot.sha256
        );
        assert_ne!(original.evidence.sha256, revised.evidence.sha256);
        assert_eq!(
            revised_report.extracted.property_count,
            original_report.extracted.property_count + 1
        );
        let revised_root = revised.catalogue.schemas[0]
            .entity_types
            .iter()
            .find(|entity| entity.name == "SalesOrder")
            .unwrap();
        assert!(
            revised_root
                .properties
                .iter()
                .any(|property| property.name == "extensionReference")
        );
        let original_root = original.catalogue.schemas[0]
            .entity_types
            .iter()
            .find(|entity| entity.name == "SalesOrder")
            .unwrap();
        assert!(
            original_root
                .properties
                .iter()
                .all(|property| property.name != "extensionReference")
        );
    }

    #[test]
    fn v4_report_inventories_unsupported_classes_without_resolving_them() {
        let bytes = include_bytes!("../fixtures/metadata/sanitized-sales-v4.edmx");
        let artifact = import_edmx_bytes(
            bytes,
            context("sanitized-sales-v4.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let report = scan_edmx_compatibility(bytes, &EdmxLimits::default()).unwrap();

        assert_eq!(report.odata_generation, EdmxODataGeneration::V4);
        assert_eq!(report.extracted.namespace_count, 1);
        assert_eq!(report.extracted.entity_count, 4);
        assert_eq!(report.extracted.property_count, 16);
        assert_eq!(report.extracted.navigation_count, 3);
        assert_eq!(report.extracted.association_count, 0);
        assert_eq!(report.extracted.entity_set_count, 3);
        let unsupported = report
            .unsupported_constructs
            .iter()
            .map(|construct| construct.kind)
            .collect::<BTreeSet<_>>();
        for kind in [
            EdmxUnsupportedConstructKind::ComplexType,
            EdmxUnsupportedConstructKind::Inheritance,
            EdmxUnsupportedConstructKind::Alias,
            EdmxUnsupportedConstructKind::Reference,
            EdmxUnsupportedConstructKind::ReferentialConstraint,
            EdmxUnsupportedConstructKind::EnumType,
            EdmxUnsupportedConstructKind::TypeDefinition,
            EdmxUnsupportedConstructKind::Action,
            EdmxUnsupportedConstructKind::Function,
            EdmxUnsupportedConstructKind::Containment,
            EdmxUnsupportedConstructKind::Annotation,
            EdmxUnsupportedConstructKind::NavigationBinding,
        ] {
            assert!(unsupported.contains(&kind), "missing {kind:?}");
        }
        assert!(report.unsupported_constructs.iter().any(|construct| {
            construct.kind == EdmxUnsupportedConstructKind::ComplexType
                && construct.location == "Synthetic.ODataV4.Sales.AuditStamp"
        }));
        assert!(report.unsupported_constructs.iter().any(|construct| {
            construct.kind == EdmxUnsupportedConstructKind::Inheritance
                && construct.location == "Synthetic.ODataV4.Sales.DerivedAudit"
        }));
        assert!(report.unsupported_constructs.iter().any(|construct| {
            construct.kind == EdmxUnsupportedConstructKind::Containment
                && construct.location == "Synthetic.ODataV4.Sales.SalesOrder.to_Attachments"
        }));
        assert_eq!(
            report.qualified_attributes,
            [] as [EdmxQualifiedAttribute; 0]
        );
        assert_eq!(
            artifact.catalogue.schemas[0]
                .entity_types
                .iter()
                .find(|entity| entity.name == "DerivedAudit")
                .unwrap()
                .properties[0]
                .name,
            "AuditNote"
        );
        assert!(
            artifact.catalogue.schemas[0]
                .entity_types
                .iter()
                .all(|entity| entity
                    .properties
                    .iter()
                    .all(|property| property.name != "CreatedBy"))
        );
    }

    #[test]
    fn qualified_core_attributes_still_fail_closed() {
        let xml = v2_document(
            r#"<d:Schema xmlns:d="http://schemas.microsoft.com/ado/2008/09/edm" xmlns:ext="urn:migration-stream:synthetic:edmx:extensions" Namespace="X"><d:EntityType ext:Name="Injected"/></d:Schema>"#,
        );
        let error = scan_edmx_compatibility(xml.as_bytes(), &EdmxLimits::default()).unwrap_err();

        assert!(error.to_string().contains("ext:Name"));
        assert!(error.to_string().contains("must be unqualified"));
    }

    #[test]
    fn rejects_recognized_elements_outside_supported_namespaces() {
        let cases = [
            v4_document(
                r#"<d:Schema Namespace="X"><f:EntityType xmlns:f="urn:foreign" Name="Injected"/></d:Schema>"#,
            ),
            v4_document(r#"<d:Schema Namespace="X"><EntityType Name="Unbound"/></d:Schema>"#),
            v4_document(
                r#"<d:Schema Namespace="X"><missing:EntityType Name="Unknown"/></d:Schema>"#,
            ),
        ];

        for xml in cases {
            let error = import_edmx_bytes(
                xml.as_bytes(),
                context("namespace.edmx"),
                &EdmxLimits::default(),
            )
            .unwrap_err();
            assert!(matches!(error, KernelError::EdmxParse(_)));
        }
    }

    #[test]
    fn rejects_mixed_edm_namespace_versions() {
        let xml = r#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" xmlns:d="http://schemas.microsoft.com/ado/2008/09/edm" Version="4.0"><e:DataServices><d:Schema Namespace="Mixed"/></e:DataServices></e:Edmx>"#;
        let error = import_edmx_bytes(
            xml.as_bytes(),
            context("mixed.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("matching-version DataServices"));
    }

    #[test]
    fn requires_direct_edmx_data_services_schema_spine() {
        let cases = [
            r#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" xmlns:x="urn:extension" xmlns:d="http://docs.oasis-open.org/odata/ns/edm" Version="4.0"><x:Wrapper><e:DataServices><d:Schema Namespace="X"/></e:DataServices></x:Wrapper></e:Edmx>"#,
            r#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" xmlns:d="http://docs.oasis-open.org/odata/ns/edm" Version="4.0"><e:DataServices><d:Annotation><d:Schema Namespace="X"/></d:Annotation></e:DataServices></e:Edmx>"#,
        ];

        for xml in cases {
            let error = import_edmx_bytes(
                xml.as_bytes(),
                context("spine.edmx"),
                &EdmxLimits::default(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("direct child"));
        }
    }

    #[test]
    fn requires_exact_direct_parents_for_supported_declarations() {
        let schema_bodies = [
            r#"<d:Annotation><d:EntityType Name="Nested"/></d:Annotation>"#,
            r#"<d:EntityType Name="E"><d:Annotation><d:Property Name="P" Type="Edm.String"/></d:Annotation></d:EntityType>"#,
            r#"<d:EntityType Name="E"><d:Key><d:Annotation><d:PropertyRef Name="P"/></d:Annotation></d:Key></d:EntityType>"#,
            r#"<d:EntityType Name="E"><d:Key><d:NavigationProperty Name="N" Type="X.E"/></d:Key></d:EntityType>"#,
            r#"<d:Annotation><d:Association Name="A"/></d:Annotation>"#,
            r#"<d:End Role="R" Type="X.E" Multiplicity="1"/>"#,
            r#"<d:Annotation><d:EntityContainer Name="C"/></d:Annotation>"#,
            r#"<d:EntitySet Name="S" EntityType="X.E"/>"#,
        ];

        for body in schema_bodies {
            let xml = v4_document(&format!(r#"<d:Schema Namespace="X">{body}</d:Schema>"#));
            let error = import_edmx_bytes(
                xml.as_bytes(),
                context("nesting.edmx"),
                &EdmxLimits::default(),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("direct child"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn rejects_blank_required_attributes_and_qualified_attribute_injection() {
        let invalid_schemas = [
            r#"<d:Schema Namespace=" "/>"#,
            r#"<d:Schema Namespace="X"><d:EntityType Name=""/></d:Schema>"#,
            r#"<d:Schema Namespace="X"><d:EntityType Name="E"><d:Property Name="P" Type=" "/></d:EntityType></d:Schema>"#,
            r#"<d:Schema Namespace="X"><d:Association Name="A"><d:End Role="" Type="X.E" Multiplicity="1"/></d:Association></d:Schema>"#,
            r#"<d:Schema Namespace="X"><d:EntityContainer Name="C"><d:EntitySet Name="S" EntityType=""/></d:EntityContainer></d:Schema>"#,
            r#"<d:Schema Namespace="X" xmlns:evil="urn:evil"><d:EntityType evil:Name="Injected"/></d:Schema>"#,
        ];

        for schema in invalid_schemas {
            let xml = v4_document(schema);
            let error = import_edmx_bytes(
                xml.as_bytes(),
                context("attributes.edmx"),
                &EdmxLimits::default(),
            )
            .unwrap_err();
            assert!(matches!(error, KernelError::EdmxParse(_)));
        }
    }

    #[test]
    fn v2_unqualified_associations_are_scoped_to_the_navigation_schema() {
        let alpha = r#"<Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Alpha"><EntityType Name="E"><NavigationProperty Name="N" Relationship="Shared" ToRole="Target"/></EntityType><Association Name="Shared"><End Role="Target" Type="Alpha.E" Multiplicity="*"/></Association></Schema>"#;
        let beta = r#"<Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Beta"><EntityType Name="E"><NavigationProperty Name="N" Relationship="Shared" ToRole="Target"/></EntityType><Association Name="Shared"><End Role="Target" Type="Beta.E" Multiplicity="1"/></Association></Schema>"#;
        let first = v2_document(&format!("{beta}{alpha}"));
        let second = v2_document(&format!("{alpha}{beta}"));
        let first = import_edmx_bytes(
            first.as_bytes(),
            context("scoped.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();
        let second = import_edmx_bytes(
            second.as_bytes(),
            context("scoped.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap();

        assert_eq!(first.catalogue.schemas, second.catalogue.schemas);
        assert_eq!(
            first.catalogue.schemas[0].entity_types[0].navigation_properties[0]
                .cardinality
                .as_deref(),
            Some("0..*")
        );
        assert_eq!(
            first.catalogue.schemas[1].entity_types[0].navigation_properties[0]
                .cardinality
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn rejects_ambiguous_v2_relationship_candidates() {
        let schemas = r#"<Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Local"><EntityType Name="E"><NavigationProperty Name="N" Relationship="Other.Shared" ToRole="Target"/></EntityType><Association Name="Other.Shared"><End Role="Target" Type="Local.E" Multiplicity="1"/></Association></Schema><Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Other"><Association Name="Shared"><End Role="Target" Type="Other.E" Multiplicity="*"/></Association></Schema>"#;
        let xml = v2_document(schemas);
        let error = import_edmx_bytes(
            xml.as_bytes(),
            context("ambiguous.edmx"),
            &EdmxLimits::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("ambiguous V2 Relationship"));
    }

    #[test]
    fn contract_verification_rejects_schema_invalid_artifact_mutation() {
        let mut artifact =
            import_edmx_bytes(V2, context("v2.edmx"), &EdmxLimits::default()).unwrap();
        artifact.catalogue.schemas[0].namespace.clear();
        artifact.evidence.sha256 = hash_catalogue(&artifact.catalogue).unwrap();
        let error = artifact.verify_contract().unwrap_err();

        assert!(error.to_string().contains("violates its artifact schema"));
    }

    #[test]
    fn rejects_document_types() {
        let xml = br#"<!DOCTYPE Schema SYSTEM "https://example.invalid/metadata.dtd"><Schema Namespace="X"/>"#;
        let error =
            import_edmx_bytes(xml, context("doctype.edmx"), &EdmxLimits::default()).unwrap_err();
        assert!(matches!(error, KernelError::EdmxParse(_)));
        assert!(error.to_string().contains("DOCTYPE"));
    }

    #[test]
    fn enforces_every_configured_limit() {
        let base = EdmxLimits::default();
        let cases = [
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0"/>"#.as_slice(),
                EdmxLimits {
                    max_input_bytes: 4,
                    ..base.clone()
                },
            ),
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0"><e:DataServices><d:Schema xmlns:d="http://docs.oasis-open.org/odata/ns/edm" Namespace="X"/></e:DataServices></e:Edmx>"#.as_slice(),
                EdmxLimits {
                    max_depth: 2,
                    ..base.clone()
                },
            ),
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0"><e:DataServices/></e:Edmx>"#.as_slice(),
                EdmxLimits {
                    max_events: 1,
                    ..base.clone()
                },
            ),
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0"/>"#.as_slice(),
                EdmxLimits {
                    max_attributes_per_element: 1,
                    ..base.clone()
                },
            ),
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0"/>"#.as_slice(),
                EdmxLimits {
                    max_attribute_bytes: 4,
                    ..base.clone()
                },
            ),
            (
                br#"<e:Edmx xmlns:e="http://docs.oasis-open.org/odata/ns/edmx" Version="4.0">long</e:Edmx>"#.as_slice(),
                EdmxLimits {
                    max_text_bytes: 3,
                    ..base
                },
            ),
        ];

        for (xml, limits) in cases {
            let error = import_edmx_bytes(xml, context("limited.edmx"), &limits).unwrap_err();
            assert!(matches!(error, KernelError::EdmxLimit(_)));
        }
    }

    #[test]
    fn generated_catalogue_cannot_become_runtime_authority() {
        let artifact = import_edmx_bytes(V2, context("v2.edmx"), &EdmxLimits::default()).unwrap();
        let error = artifact.require_approved_profile_authority().unwrap_err();
        assert!(matches!(error, KernelError::GeneratedDraftAuthority));
        assert_eq!(error.code(), "GENERATED_DRAFT_AUTHORITY");
    }
}
